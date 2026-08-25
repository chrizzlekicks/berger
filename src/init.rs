use crate::state;
use serde_json::Value;
use std::fs;
use std::path::Path;

const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// Rewrites `hooks[event]` in-place: entries whose `command` contains "amux" are
/// dropped, and exactly one entry with `command == bergr_cmd` is ensured to exist.
/// All other entries (e.g. `{matcher: "Bash", command: "rtk hook claude"}`) are left
/// untouched, in whatever position they were found.
///
/// `settings` comes from a hand-editable file, not a trusted internal type, so a
/// shape mismatch (e.g. `"hooks": []` instead of an object) is reported as an error
/// rather than a panic.
pub fn merge_hooks(settings: &mut Value, bergr_cmd: &str) -> Result<(), String> {
    let root = settings
        .as_object_mut()
        .ok_or("settings.json root must be an object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks = hooks.as_object_mut().ok_or("\"hooks\" must be an object")?;

    for event in HOOK_EVENTS {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let entries = entries
            .as_array_mut()
            .ok_or_else(|| format!("\"hooks.{event}\" must be an array"))?;

        entries.retain(|entry| !entry_command_contains(entry, "amux"));

        let already_present = entries
            .iter()
            .any(|entry| entry_command_contains(entry, bergr_cmd));
        if !already_present {
            entries.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": bergr_cmd }]
            }));
        }
    }
    Ok(())
}

fn entry_command_contains(entry: &Value, needle: &str) -> bool {
    let Some(inner) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    inner
        .iter()
        .any(|h| match h.get("command").and_then(Value::as_str) {
            Some(c) => c.contains(needle),
            None => false,
        })
}

/// Detects a still-running amux watcher so `init` can warn about it — a live watcher
/// would keep polling the legacy cache dir and fighting bergr's own renames.
pub fn find_running_amux_watchers(legacy_cache_root: &Path) -> Vec<String> {
    let Ok(sessions) = fs::read_dir(legacy_cache_root) else {
        return Vec::new();
    };
    sessions
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("watch.pid").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect()
}

pub fn tmux_conf_contents(bergr_bin: &str) -> String {
    format!(
        "# ~/.config/bergr/tmux.conf — managed by `bergr init`, do not edit\n\
         set -g allow-rename off\n\
         set -g automatic-rename off\n\
         bind-key M run-shell \"{bergr_bin} sync --session #{{session_name}}\"\n"
    )
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").expect("HOME must be set"))
}

/// Writes via tmpfile + rename so a crash mid-write can never leave `path`
/// truncated or half-written — these files (Claude's hook config, bergr's tmux
/// config) are read on every session start, unlike state files that get
/// rewritten constantly and can tolerate a rare loss.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

fn exit_on_error<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("bergr init: {context}: {e}");
        std::process::exit(1);
    })
}

fn resolve_bergr_bin() -> String {
    let exe = std::env::current_exe().expect("could not resolve current executable path");
    if exe.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "bergr init: refusing to run from a build directory ({}).\n\
             Install first: cargo install --path . --root ~/.local",
            exe.display()
        );
        std::process::exit(1);
    }
    exe.to_string_lossy().into_owned()
}

/// Merges bergr's hook into `~/.claude/settings.json`, backing up the previous
/// contents once (on first run only, since re-running init should not clobber
/// a backup that predates any bergr changes).
fn update_claude_settings(bergr_bin: &str) -> std::path::PathBuf {
    let settings_path = home().join(".claude").join("settings.json");
    if let Some(dir) = settings_path.parent() {
        exit_on_error(
            fs::create_dir_all(dir),
            &format!("could not create {}", dir.display()),
        );
    }

    let mut settings: Value = match fs::read_to_string(&settings_path) {
        Ok(text) => exit_on_error(
            serde_json::from_str(&text),
            &format!("{} is not valid JSON", settings_path.display()),
        ),
        Err(_) => Value::Object(serde_json::Map::new()),
    };

    let backup_path = settings_path.with_extension("json.bergr-bak");
    if settings_path.exists() && !backup_path.exists() {
        exit_on_error(
            fs::copy(&settings_path, &backup_path),
            "could not back up settings.json",
        );
    }

    exit_on_error(
        merge_hooks(&mut settings, &format!("{bergr_bin} event")),
        &format!("{} has an unexpected shape", settings_path.display()),
    );
    let rendered = serde_json::to_string_pretty(&settings).unwrap();
    exit_on_error(
        write_atomic(&settings_path, &rendered),
        &format!("could not write {}", settings_path.display()),
    );

    settings_path
}

fn bergr_config_dir() -> std::path::PathBuf {
    xdg_subdir("XDG_CONFIG_HOME", ".config", "bergr")
}

/// `$xdg_var/name`, falling back to `$HOME/home_fallback_dir/name` when the XDG
/// var is unset or empty — the same fallback rule the amux prototype used.
fn xdg_subdir(xdg_var: &str, home_fallback_dir: &str, name: &str) -> std::path::PathBuf {
    match std::env::var(xdg_var) {
        Ok(dir) if !dir.is_empty() => std::path::PathBuf::from(dir).join(name),
        _ => home().join(home_fallback_dir).join(name),
    }
}

fn write_bergr_tmux_conf(bergr_bin: &str) -> std::path::PathBuf {
    let bergr_conf_dir = bergr_config_dir();
    exit_on_error(
        fs::create_dir_all(&bergr_conf_dir),
        &format!("could not create {}", bergr_conf_dir.display()),
    );

    let tmux_conf_path = bergr_conf_dir.join("tmux.conf");
    exit_on_error(
        write_atomic(&tmux_conf_path, &tmux_conf_contents(bergr_bin)),
        &format!("could not write {}", tmux_conf_path.display()),
    );

    tmux_conf_path
}

fn legacy_amux_tmux_conf_path() -> std::path::PathBuf {
    xdg_subdir("XDG_CONFIG_HOME", ".config", "amux").join("tmux.conf")
}

fn sources_path(tmux_conf_contents: &str, path: &Path) -> bool {
    let path = path.to_string_lossy();
    tmux_conf_contents.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line.starts_with("source-file") && line.contains(&*path)
    })
}

fn warn_about_stale_amux_source_line(user_tmux_conf_contents: &str) {
    let legacy_path = legacy_amux_tmux_conf_path();
    if sources_path(user_tmux_conf_contents, &legacy_path) {
        eprintln!(
            "bergr init: warning: {} still sources the old amux config ({}). \
             Remove that `source-file` line — amux's `bind-key M` can otherwise \
             override bergr's.",
            home().join(".tmux.conf").display(),
            legacy_path.display()
        );
    }
}

fn report_tmux_conf_sourcing(tmux_conf_path: &Path) {
    let user_tmux_conf = home().join(".tmux.conf");
    let source_line = format!("source-file {}", tmux_conf_path.display());
    let contents = fs::read_to_string(&user_tmux_conf).unwrap_or_default();
    let already_sourced = sources_path(&contents, tmux_conf_path);

    warn_about_stale_amux_source_line(&contents);

    if already_sourced {
        println!(
            "bergr init: {} already sources bergr's tmux config",
            user_tmux_conf.display()
        );
    } else {
        println!(
            "bergr init: add this to {}, then run `tmux source-file {}`:\n    {source_line}",
            user_tmux_conf.display(),
            user_tmux_conf.display(),
        );
    }
}

pub(crate) fn legacy_amux_cache_root() -> std::path::PathBuf {
    xdg_subdir("XDG_CACHE_HOME", ".cache", "amux")
}

fn warn_about_live_amux_watchers() {
    let legacy_cache = legacy_amux_cache_root();
    for session in find_running_amux_watchers(&legacy_cache) {
        eprintln!(
            "bergr init: warning: amux watcher still running for session '{session}'. \
             Kill it: kill $(cat {}/{session}/watch.pid)",
            legacy_cache.display()
        );
    }
}

/// Runs the `init` command against the real environment: refuses to run from a
/// `target/` build directory (the hooks would then reference a path that stops
/// existing on the next `cargo clean`), merges hooks into `~/.claude/settings.json`,
/// writes `~/.config/bergr/tmux.conf`, creates the cache root, and warns about any
/// still-running amux watcher.
pub fn run() {
    let bergr_bin = resolve_bergr_bin();

    let settings_path = update_claude_settings(&bergr_bin);
    let tmux_conf_path = write_bergr_tmux_conf(&bergr_bin);

    exit_on_error(
        state::cache_root().and_then(|root| fs::create_dir_all(&root)),
        "could not create cache root",
    );

    println!("bergr init: wrote {}", settings_path.display());
    println!("bergr init: wrote {}", tmux_conf_path.display());
    report_tmux_conf_sourcing(&tmux_conf_path);
    warn_about_live_amux_watchers();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture() -> Value {
        let text = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/settings.json"
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn replaces_amux_entries_and_keeps_unrelated_hooks() {
        let mut settings = load_fixture();
        merge_hooks(&mut settings, "/home/schimetschka/.local/bin/bergr event").unwrap();

        let pre_tool_use = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "rtk hook claude")),
            "unrelated rtk hook must survive the merge"
        );
        assert!(
            !pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "amux")),
            "no amux command should remain"
        );
    }

    #[test]
    fn every_event_gets_exactly_one_bergr_entry() {
        let mut settings = load_fixture();
        let cmd = "/home/schimetschka/.local/bin/bergr event";
        merge_hooks(&mut settings, cmd).unwrap();

        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            let bergr_count = entries
                .iter()
                .filter(|e| entry_command_contains(e, cmd))
                .count();
            assert_eq!(
                bergr_count, 1,
                "event {event} should have exactly one bergr entry"
            );
        }
    }

    #[test]
    fn rerunning_merge_is_a_no_op() {
        let mut settings = load_fixture();
        let cmd = "/home/schimetschka/.local/bin/bergr event";
        merge_hooks(&mut settings, cmd).unwrap();
        let once = settings.clone();
        merge_hooks(&mut settings, cmd).unwrap();
        assert_eq!(settings, once, "a second merge must be idempotent");
    }

    #[test]
    fn unrelated_top_level_keys_survive() {
        let mut settings = load_fixture();
        let before_model = settings["model"].clone();
        merge_hooks(&mut settings, "/x/bergr event").unwrap();
        assert_eq!(settings["model"], before_model);
    }

    #[test]
    fn event_with_no_prior_hooks_still_gets_bergr_entry() {
        let mut settings = serde_json::json!({});
        merge_hooks(&mut settings, "/x/bergr event").unwrap();
        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert_eq!(entries.len(), 1);
        }
    }

    #[test]
    fn detects_no_watcher_when_no_legacy_cache() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(find_running_amux_watchers(&missing).is_empty());
    }

    #[test]
    fn detects_running_watcher_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("myproject");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("watch.pid"), "12345").unwrap();
        assert_eq!(find_running_amux_watchers(dir.path()), vec!["myproject"]);
    }

    #[test]
    fn tmux_conf_uses_absolute_path_and_session_flag() {
        let conf = tmux_conf_contents("/home/schimetschka/.local/bin/bergr");
        assert!(conf.contains("/home/schimetschka/.local/bin/bergr sync --session"));
        assert!(conf.contains("allow-rename off"));
    }

    #[test]
    fn merge_hooks_reports_error_instead_of_panicking_on_non_object_hooks() {
        let mut settings = serde_json::json!({ "hooks": [] });
        let result = merge_hooks(&mut settings, "/x/bergr event");
        assert!(result.is_err());
    }

    #[test]
    fn merge_hooks_reports_error_when_root_is_not_an_object() {
        let mut settings = serde_json::json!([]);
        let result = merge_hooks(&mut settings, "/x/bergr event");
        assert!(result.is_err());
    }

    #[test]
    fn xdg_subdir_prefers_xdg_var_when_set() {
        assert_eq!(
            xdg_subdir("HOME", ".cache", "amux"),
            home().join("amux"),
            "HOME is always set, so it should be used verbatim as the XDG base"
        );
    }

    #[test]
    fn xdg_subdir_falls_back_when_var_unset() {
        assert_eq!(
            xdg_subdir("BERGR_TEST_UNSET_XDG_VAR", ".cache", "amux"),
            home().join(".cache").join("amux")
        );
    }

    #[test]
    fn sources_path_ignores_commented_out_line() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "# source-file /home/x/.config/amux/tmux.conf\n";
        assert!(!sources_path(conf, path));
    }

    #[test]
    fn sources_path_detects_real_directive() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "set -g mouse on\nsource-file /home/x/.config/amux/tmux.conf\n";
        assert!(sources_path(conf, path));
    }

    #[test]
    fn legacy_amux_tmux_conf_path_uses_config_fallback_shape() {
        assert_eq!(
            legacy_amux_tmux_conf_path(),
            home().join(".config").join("amux").join("tmux.conf")
        );
    }
}
