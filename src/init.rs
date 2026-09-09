use crate::fs_util::write_atomic;
use crate::state;
use serde_json::Value;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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

/// Rewrites `hooks[event]` in-place: drops previously-generated berger hooks (any old
/// `berger_cmd` shape, so a reinstalled binary replaces rather than duplicates its hook),
/// and ensures exactly one entry
/// with `command == berger_cmd`. Unrelated entries are left untouched.
///
/// `settings` is hand-editable, not a trusted internal type, so a shape mismatch is
/// reported as an error rather than a panic.
pub fn merge_hooks(settings: &mut Value, berger_cmd: &str) -> Result<(), String> {
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

        let mut kept = Vec::new();
        for mut entry in entries.drain(..) {
            remove_stale_berger_commands(&mut entry, berger_cmd);
            if !entry_hooks_is_empty(&entry) {
                kept.push(entry);
            }
        }
        *entries = kept;

        // Exact match, not `entry_command_contains`'s substring check: a hand-added
        // command that merely mentions `berger_cmd` as a substring (e.g. the same
        // path invoked with extra flags) must not be mistaken for the canonical
        // hook — that would suppress installing the real one.
        let already_present = entries
            .iter()
            .any(|entry| entry_command_equals(entry, berger_cmd));
        if !already_present {
            entries.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": berger_cmd }]
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
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

fn entry_command_equals(entry: &Value, command: &str) -> bool {
    let Some(inner) = entry.get("hooks").and_then(Value::as_array) else {
        return false;
    };
    inner
        .iter()
        .any(|h| h.get("command").and_then(Value::as_str) == Some(command))
}

/// Drops nested `hooks[].command` entries that are a previously-generated berger hook
/// but no longer match the current `berger_cmd` (e.g. after the binary moved), keeping
/// any sibling commands in the same group.
fn remove_stale_berger_commands(entry: &mut Value, berger_cmd: &str) {
    let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
        return;
    };
    let mut kept = Vec::new();
    for hook in inner.drain(..) {
        let stale = match hook.get("command").and_then(Value::as_str) {
            Some(c) => c != berger_cmd && is_generated_berger_command(c),
            None => false,
        };
        if !stale {
            kept.push(hook);
        }
    }
    *inner = kept;
}

/// True for a command shaped like a `berger init`-generated hook: a single
/// shell-quoted path whose basename is `berger`, followed by the `event` subcommand.
/// Matching by shape (not exact string) is what lets re-init recognize its own prior
/// output even when the binary's path has changed since the last run.
fn is_generated_berger_command(command: &str) -> bool {
    let Some(rest) = command.strip_suffix(" event") else {
        return false;
    };
    // Accept both the current shell-quoted shape and the unquoted shape emitted by
    // builds before quoting was added (commit 4761c87) — a hook from an older build
    // of this tool must still be recognized as stale, not just future ones.
    let path = match rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        Some(quoted_path) => shell_unquote_body(quoted_path),
        None => rest.to_string(),
    };
    Path::new(&path).file_name().and_then(|n| n.to_str()) == Some("berger")
}

fn entry_hooks_is_empty(entry: &Value) -> bool {
    match entry.get("hooks").and_then(Value::as_array) {
        Some(inner) => inner.is_empty(),
        None => true,
    }
}

/// Quotes `s` for safe use as a single argument in a POSIX shell command line,
/// even when `s` itself contains single quotes (e.g. `/Users/Jane O'Connor/berger`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Exact inverse of `shell_quote`'s body. Scans byte-by-byte for the literal `'\''`
/// escape unit rather than blindly replacing that substring — any other alignment of
/// those bytes is real content, not an escape.
fn shell_unquote_body(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = String::with_capacity(body.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"'\\''") {
            out.push('\'');
            i += 4;
        } else {
            let ch_len = body[i..].chars().next().map(char::len_utf8).unwrap_or(1);
            out.push_str(&body[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

pub fn tmux_conf_contents(berger_bin: &str) -> String {
    // `#{q:session_name}` asks tmux itself to shell-quote the expanded session name —
    // `berger_bin` is quoted up front since it's known before tmux ever expands this
    // string, but the session name isn't known until expansion time, so only tmux's
    // own format modifier can quote it safely (session names may contain spaces,
    // quotes, `$`, backticks, or `;` — tmux only rejects `.` and `:`).
    format!(
        "# ~/.config/berger/tmux.conf — managed by `berger init`, do not edit\n\
         set -g allow-rename off\n\
         set -g automatic-rename off\n\
         bind-key M run-shell \"{} sync --session #{{q:session_name}}\"\n",
        shell_quote(berger_bin)
    )
}

fn exit_on_error<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("berger init: {context}: {e}");
        std::process::exit(1);
    })
}

/// Requires `value` to be an absolute path, matching `state::cache_root()`'s check
/// on `HOME` — a relative HOME (e.g. `work`) would otherwise make callers write
/// settings and config paths below the current directory instead of the user's
/// home. Pure function of its input so the validation is testable without
/// mutating the process's real `HOME` env var.
fn validate_home(value: Option<std::ffi::OsString>) -> Result<PathBuf, &'static str> {
    value
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or("HOME is not set or is not an absolute path")
}

fn home() -> PathBuf {
    exit_on_error(
        validate_home(env::var_os("HOME")),
        "resolving home directory",
    )
}

/// A Cargo build output directory: `target/{debug,release}/...`, or the
/// cross-compiled `target/<triple>/{debug,release}/...` — not merely any path with a
/// `target` component (e.g. `/opt/target/bin/berger` is a legitimate install path). A
/// custom Cargo profile name isn't detected; this is a best-effort guard.
fn is_cargo_build_dir(exe: &Path) -> bool {
    let components: Vec<_> = exe.components().collect();
    for (i, c) in components.iter().enumerate() {
        if c.as_os_str() != "target" {
            continue;
        }
        let is_profile =
            |c: &std::path::Component| c.as_os_str() == "debug" || c.as_os_str() == "release";
        if components.get(i + 1).is_some_and(is_profile)
            || components.get(i + 2).is_some_and(is_profile)
        {
            return true;
        }
    }
    false
}

fn resolve_berger_bin() -> String {
    let exe = env::current_exe().expect("could not resolve current executable path");
    if is_cargo_build_dir(&exe) {
        eprintln!(
            "berger init: refusing to run from a build directory ({}).\n\
             Install first: cargo install --path . --root ~/.local",
            exe.display()
        );
        std::process::exit(1);
    }
    exe.clone()
        .into_os_string()
        .into_string()
        .unwrap_or_else(|_| {
            eprintln!(
                "berger init: executable path is not valid UTF-8 ({}); \
             move berger to a path with only UTF-8 characters and re-run init.",
                exe.display()
            );
            std::process::exit(1);
        })
}

/// Merges berger's hook into `~/.claude/settings.json`, backing up the previous
/// contents once (on first run only, since re-running init should not clobber
/// a backup that predates any berger changes).
fn update_claude_settings(berger_bin: &str) -> PathBuf {
    let settings_path = home().join(".claude").join("settings.json");

    let mut settings: Value = match fs::read_to_string(&settings_path) {
        Ok(text) => exit_on_error(
            serde_json::from_str(&text),
            &format!("{} is not valid JSON", settings_path.display()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(e) => exit_on_error(
            Err::<Value, _>(e),
            &format!("could not read {}", settings_path.display()),
        ),
    };

    let backup_path = settings_path.with_extension("json.berger-bak");
    if settings_path.exists() && !backup_path.exists() {
        exit_on_error(
            fs::copy(&settings_path, &backup_path),
            "could not back up settings.json",
        );
    }

    exit_on_error(
        merge_hooks(&mut settings, &format!("{} event", shell_quote(berger_bin))),
        &format!("{} has an unexpected shape", settings_path.display()),
    );
    let rendered = serde_json::to_string_pretty(&settings).unwrap();
    exit_on_error(
        write_atomic(&settings_path, &rendered),
        &format!("could not write {}", settings_path.display()),
    );

    settings_path
}

fn berger_config_dir() -> PathBuf {
    xdg_subdir("XDG_CONFIG_HOME", ".config", "berger")
}

/// `$xdg_var/name`, falling back to `$HOME/home_fallback_dir/name` when the XDG var
/// is unset, empty, or relative. Relative values are rejected rather than resolved
/// against the CWD, since `init`/`sync` can run from different working directories
/// and this feeds `berger reset`'s recursive delete.
fn xdg_subdir(xdg_var: &str, home_fallback_dir: &str, name: &str) -> PathBuf {
    match env::var_os(xdg_var) {
        None => home().join(home_fallback_dir).join(name),
        Some(dir) => {
            let dir = exit_on_error(dir.into_string().map_err(|_| "not valid UTF-8"), xdg_var);
            if Path::new(&dir).is_absolute() {
                PathBuf::from(dir).join(name)
            } else {
                home().join(home_fallback_dir).join(name)
            }
        }
    }
}

fn write_berger_tmux_conf(berger_bin: &str) -> PathBuf {
    let berger_conf_dir = berger_config_dir();
    exit_on_error(
        fs::create_dir_all(&berger_conf_dir),
        &format!("could not create {}", berger_conf_dir.display()),
    );

    let tmux_conf_path = berger_conf_dir.join("tmux.conf");
    exit_on_error(
        write_atomic(&tmux_conf_path, &tmux_conf_contents(berger_bin)),
        &format!("could not write {}", tmux_conf_path.display()),
    );

    tmux_conf_path
}

fn is_source_line_for(line: &str, path: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("source-file") else {
        return false;
    };
    rest.trim().trim_matches('"').trim_matches('\'') == path
}

fn sources_path(tmux_conf_contents: &str, path: &Path) -> bool {
    let path = path.to_string_lossy();
    tmux_conf_contents
        .lines()
        .any(|line| is_source_line_for(line, &path))
}

fn report_tmux_conf_sourcing(tmux_conf_path: &Path) {
    let user_tmux_conf = home().join(".tmux.conf");
    let source_line = format!("source-file \"{}\"", tmux_conf_path.display());
    let contents = fs::read_to_string(&user_tmux_conf).unwrap_or_default();
    let already_sourced = sources_path(&contents, tmux_conf_path);

    if already_sourced {
        println!(
            "berger init: {} already sources berger's tmux config",
            user_tmux_conf.display()
        );
    } else {
        println!(
            "berger init: add this to {}, then run `tmux source-file {}`:\n    {source_line}",
            user_tmux_conf.display(),
            user_tmux_conf.display(),
        );
    }
}

/// Runs the `init` command: refuses to run from a build directory, merges hooks into
/// `~/.claude/settings.json`, writes `~/.config/berger/tmux.conf`, creates the cache
/// root.
pub fn run() {
    let berger_bin = resolve_berger_bin();

    let settings_path = update_claude_settings(&berger_bin);
    let tmux_conf_path = write_berger_tmux_conf(&berger_bin);

    exit_on_error(
        state::cache_root().and_then(|root| fs::create_dir_all(&root)),
        "could not create cache root",
    );

    println!("berger init: wrote {}", settings_path.display());
    println!("berger init: wrote {}", tmux_conf_path.display());
    report_tmux_conf_sourcing(&tmux_conf_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_home_accepts_absolute_path() {
        assert_eq!(
            validate_home(Some("/home/jane".into())),
            Ok(PathBuf::from("/home/jane"))
        );
    }

    #[test]
    fn validate_home_rejects_relative_path() {
        assert!(validate_home(Some("work".into())).is_err());
    }

    #[test]
    fn validate_home_rejects_empty() {
        assert!(validate_home(Some("".into())).is_err());
    }

    #[test]
    fn validate_home_rejects_unset() {
        assert!(validate_home(None).is_err());
    }

    #[test]
    fn rejects_cargo_debug_and_release_output() {
        assert!(is_cargo_build_dir(Path::new("/repo/target/debug/berger")));
        assert!(is_cargo_build_dir(Path::new("/repo/target/release/berger")));
    }

    #[test]
    fn rejects_cross_compiled_cargo_output() {
        assert!(is_cargo_build_dir(Path::new(
            "/repo/target/x86_64-unknown-linux-gnu/release/berger"
        )));
    }

    #[test]
    fn allows_target_as_an_unrelated_path_component() {
        assert!(!is_cargo_build_dir(Path::new("/opt/target/bin/berger")));
        assert!(!is_cargo_build_dir(Path::new(
            "/home/target/.local/bin/berger"
        )));
    }

    #[test]
    fn allows_installed_path_with_no_target_component() {
        assert!(!is_cargo_build_dir(Path::new(
            "/home/schimetschka/.local/bin/berger"
        )));
    }

    fn load_fixture() -> Value {
        let text = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/settings.json"
        ))
        .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn preserves_amux_entries_and_keeps_unrelated_hooks() {
        let mut settings = load_fixture();
        merge_hooks(&mut settings, "/home/schimetschka/.local/bin/berger event").unwrap();

        let pre_tool_use = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "rtk hook claude")),
            "unrelated rtk hook must survive the merge"
        );
        assert!(
            pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "amux")),
            "amux command must survive the merge"
        );
    }

    #[test]
    fn every_event_gets_exactly_one_berger_entry() {
        let mut settings = load_fixture();
        let cmd = "/home/schimetschka/.local/bin/berger event";
        merge_hooks(&mut settings, cmd).unwrap();

        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            let berger_count = entries
                .iter()
                .filter(|e| entry_command_contains(e, cmd))
                .count();
            assert_eq!(
                berger_count, 1,
                "event {event} should have exactly one berger entry"
            );
        }
    }

    #[test]
    fn moved_binary_replaces_stale_berger_entry_instead_of_appending() {
        let mut settings = load_fixture();
        let old_cmd = "'/old/path/berger' event";
        merge_hooks(&mut settings, old_cmd).unwrap();

        let new_cmd = "'/new/path/berger' event";
        merge_hooks(&mut settings, new_cmd).unwrap();

        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert!(
                !entries.iter().any(|e| entry_command_contains(e, old_cmd)),
                "event {event} should no longer reference the old berger path"
            );
            let new_count = entries
                .iter()
                .filter(|e| entry_command_contains(e, new_cmd))
                .count();
            assert_eq!(
                new_count, 1,
                "event {event} should have exactly one berger entry after the move"
            );
        }
    }

    #[test]
    fn is_generated_berger_command_matches_quoted_path_with_event_suffix() {
        assert!(is_generated_berger_command("'/x/berger' event"));
        assert!(is_generated_berger_command(
            "'/Users/Jane O'\\''Connor/berger' event"
        ));
        assert!(!is_generated_berger_command("'/x/berger' sync"));
        assert!(!is_generated_berger_command("rtk hook claude"));
        assert!(!is_generated_berger_command("'/x/berger-migration' event"));
    }

    #[test]
    fn is_generated_berger_command_matches_unquoted_legacy_shape() {
        assert!(is_generated_berger_command("/x/berger event"));
        assert!(!is_generated_berger_command("/x/berger-migration event"));
    }

    #[test]
    fn rerunning_merge_is_a_no_op() {
        let mut settings = load_fixture();
        let cmd = "/home/schimetschka/.local/bin/berger event";
        merge_hooks(&mut settings, cmd).unwrap();
        let once = settings.clone();
        merge_hooks(&mut settings, cmd).unwrap();
        assert_eq!(settings, once, "a second merge must be idempotent");
    }

    #[test]
    fn unrelated_top_level_keys_survive() {
        let mut settings = load_fixture();
        let before_model = settings["model"].clone();
        merge_hooks(&mut settings, "/x/berger event").unwrap();
        assert_eq!(settings["model"], before_model);
    }

    #[test]
    fn event_with_no_prior_hooks_still_gets_berger_entry() {
        let mut settings = serde_json::json!({});
        merge_hooks(&mut settings, "/x/berger event").unwrap();
        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert_eq!(entries.len(), 1);
        }
    }

    #[test]
    fn tmux_conf_uses_absolute_path_and_session_flag() {
        let conf = tmux_conf_contents("/home/schimetschka/.local/bin/berger");
        assert!(conf.contains("'/home/schimetschka/.local/bin/berger' sync --session"));
        assert!(conf.contains("allow-rename off"));
    }

    #[test]
    fn tmux_conf_quotes_path_with_spaces() {
        let conf = tmux_conf_contents("/Users/Jane Doe/.local/bin/berger");
        assert!(conf.contains("'/Users/Jane Doe/.local/bin/berger' sync --session"));
    }

    #[test]
    fn tmux_conf_quotes_expanded_session_name() {
        // `#{session_name}` is expanded by tmux into the shell command *before* the
        // shell sees it, so an unquoted expansion is a shell-injection vector via a
        // maliciously or accidentally named session (tmux only rejects `.` and `:`
        // in session names — spaces, quotes, `$`, backticks, `;` are all legal).
        // `#{q:...}` asks tmux itself to shell-quote the expansion.
        let conf = tmux_conf_contents("/x/berger");
        assert!(conf.contains("sync --session #{q:session_name}"));
    }

    #[test]
    fn tmux_conf_escapes_path_with_single_quote() {
        let conf = tmux_conf_contents("/Users/Jane O'Connor/.local/bin/berger");
        assert!(conf.contains("'/Users/Jane O'\\''Connor/.local/bin/berger' sync --session"));
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("has'quote"), "'has'\\''quote'");
    }

    #[test]
    fn merge_hooks_reports_error_instead_of_panicking_on_non_object_hooks() {
        let mut settings = serde_json::json!({ "hooks": [] });
        let result = merge_hooks(&mut settings, "/x/berger event");
        assert!(result.is_err());
    }

    #[test]
    fn merge_hooks_reports_error_when_root_is_not_an_object() {
        let mut settings = serde_json::json!([]);
        let result = merge_hooks(&mut settings, "/x/berger event");
        assert!(result.is_err());
    }

    #[test]
    fn xdg_subdir_prefers_xdg_var_when_set() {
        assert_eq!(
            xdg_subdir("HOME", ".cache", "berger"),
            home().join("berger"),
            "HOME is always set, so it should be used verbatim as the XDG base"
        );
    }

    #[test]
    fn xdg_subdir_falls_back_when_var_unset() {
        assert_eq!(
            xdg_subdir("BERGER_TEST_UNSET_XDG_VAR", ".cache", "berger"),
            home().join(".cache").join("berger")
        );
    }

    #[test]
    fn xdg_subdir_falls_back_when_var_relative() {
        // SAFETY: single-threaded test, restored immediately after use.
        unsafe {
            env::set_var("BERGER_TEST_RELATIVE_XDG_VAR", "relative/cache");
        }
        let result = xdg_subdir("BERGER_TEST_RELATIVE_XDG_VAR", ".cache", "berger");
        unsafe {
            env::remove_var("BERGER_TEST_RELATIVE_XDG_VAR");
        }
        assert_eq!(result, home().join(".cache").join("berger"));
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
    fn sources_path_ignores_unrelated_path_with_matching_substring() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "source-file /home/x/.config/amux-backup/tmux.conf\n";
        assert!(!sources_path(conf, path));
    }

    #[test]
    fn sources_path_detects_quoted_directive() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "source-file \"/home/x/.config/amux/tmux.conf\"\n";
        assert!(sources_path(conf, path));
    }
}
