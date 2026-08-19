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
pub fn merge_hooks(settings: &mut Value, bergr_cmd: &str) {
    let hooks = settings
        .as_object_mut()
        .expect("settings.json root must be an object")
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks = hooks.as_object_mut().expect("hooks must be an object");

    for event in HOOK_EVENTS {
        let entries = hooks
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        let entries = entries.as_array_mut().expect("hook event must be an array");

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
}

fn entry_command_contains(entry: &Value, needle: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|inner| {
            inner.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|c| c.contains(needle))
            })
        })
        .unwrap_or(false)
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

/// Runs the `init` command against the real environment: refuses to run from a
/// `target/` build directory (the hooks would then reference a path that stops
/// existing on the next `cargo clean`), merges hooks into `~/.claude/settings.json`,
/// writes `~/.config/bergr/tmux.conf`, creates the cache root, and warns about any
/// still-running amux watcher.
pub fn run() {
    let exe = std::env::current_exe().expect("could not resolve current executable path");
    if exe.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "bergr init: refusing to run from a build directory ({}).\n\
             Install first: cargo install --path . --root ~/.local",
            exe.display()
        );
        std::process::exit(1);
    }
    let bergr_bin = exe.to_string_lossy().into_owned();

    let settings_path = home().join(".claude").join("settings.json");
    let mut settings: Value = match fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("bergr init: {} is not valid JSON: {e}", settings_path.display());
            std::process::exit(1);
        }),
        Err(_) => Value::Object(serde_json::Map::new()),
    };

    let backup_path = settings_path.with_extension("json.bergr-bak");
    if settings_path.exists() && !backup_path.exists() {
        if let Err(e) = fs::copy(&settings_path, &backup_path) {
            eprintln!("bergr init: could not back up settings.json: {e}");
            std::process::exit(1);
        }
    }

    merge_hooks(&mut settings, &format!("{bergr_bin} event"));
    let rendered = serde_json::to_string_pretty(&settings).unwrap();
    if let Err(e) = fs::write(&settings_path, rendered) {
        eprintln!("bergr init: could not write {}: {e}", settings_path.display());
        std::process::exit(1);
    }

    let bergr_conf_dir = home().join(".config").join("bergr");
    if let Err(e) = fs::create_dir_all(&bergr_conf_dir) {
        eprintln!("bergr init: could not create {}: {e}", bergr_conf_dir.display());
        std::process::exit(1);
    }
    let tmux_conf_path = bergr_conf_dir.join("tmux.conf");
    if let Err(e) = fs::write(&tmux_conf_path, tmux_conf_contents(&bergr_bin)) {
        eprintln!("bergr init: could not write {}: {e}", tmux_conf_path.display());
        std::process::exit(1);
    }

    if let Err(e) = state::cache_root().and_then(|root| fs::create_dir_all(&root)) {
        eprintln!("bergr init: could not create cache root: {e}");
        std::process::exit(1);
    }

    let user_tmux_conf = home().join(".tmux.conf");
    let source_line = format!("source-file {}", tmux_conf_path.display());
    let already_sourced = fs::read_to_string(&user_tmux_conf)
        .map(|t| t.contains(&*tmux_conf_path.to_string_lossy()))
        .unwrap_or(false);

    println!("bergr init: wrote {}", settings_path.display());
    println!("bergr init: wrote {}", tmux_conf_path.display());
    if already_sourced {
        println!("bergr init: {} already sources bergr's tmux config", user_tmux_conf.display());
    } else {
        println!(
            "bergr init: add this to {}, then run `tmux source-file {}`:\n    {source_line}",
            user_tmux_conf.display(),
            user_tmux_conf.display(),
        );
    }

    let legacy_cache = home().join(".cache").join("amux");
    let live_watchers = find_running_amux_watchers(&legacy_cache);
    for session in live_watchers {
        eprintln!(
            "bergr init: warning: amux watcher still running for session '{session}'. \
             Kill it: kill $(cat {}/{session}/watch.pid)",
            legacy_cache.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture() -> Value {
        let text =
            fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/settings.json"))
                .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn replaces_amux_entries_and_keeps_unrelated_hooks() {
        let mut settings = load_fixture();
        merge_hooks(&mut settings, "/home/schimetschka/.local/bin/bergr event");

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
        merge_hooks(&mut settings, cmd);

        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            let bergr_count = entries
                .iter()
                .filter(|e| entry_command_contains(e, cmd))
                .count();
            assert_eq!(bergr_count, 1, "event {event} should have exactly one bergr entry");
        }
    }

    #[test]
    fn rerunning_merge_is_a_no_op() {
        let mut settings = load_fixture();
        let cmd = "/home/schimetschka/.local/bin/bergr event";
        merge_hooks(&mut settings, cmd);
        let once = settings.clone();
        merge_hooks(&mut settings, cmd);
        assert_eq!(settings, once, "a second merge must be idempotent");
    }

    #[test]
    fn unrelated_top_level_keys_survive() {
        let mut settings = load_fixture();
        let before_model = settings["model"].clone();
        merge_hooks(&mut settings, "/x/bergr event");
        assert_eq!(settings["model"], before_model);
    }

    #[test]
    fn event_with_no_prior_hooks_still_gets_bergr_entry() {
        let mut settings = serde_json::json!({});
        merge_hooks(&mut settings, "/x/bergr event");
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
}
