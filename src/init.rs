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

/// Rewrites `hooks[event]` in-place: entries whose `command` contains "amux" or is a
/// previously-generated bergr hook (any path ending in `bergr event`, not just the
/// current `bergr_cmd`) are dropped, and exactly one entry with `command == bergr_cmd`
/// is ensured to exist. All other entries (e.g. `{matcher: "Bash", command: "rtk hook
/// claude"}`) are left untouched, in whatever position they were found.
///
/// Recognizing prior bergr hooks by shape (not just exact match against the current
/// `bergr_cmd`) matters when the binary has moved: without it, re-running `init`
/// after a reinstall/move would append the new command alongside the old one instead
/// of replacing it, so every event would invoke both — a removed old binary produces
/// recurring hook failures.
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

        let mut kept = Vec::new();
        for mut entry in entries.drain(..) {
            remove_matching_commands(&mut entry, "amux");
            remove_stale_bergr_commands(&mut entry, bergr_cmd);
            if !entry_hooks_is_empty(&entry) {
                kept.push(entry);
            }
        }
        *entries = kept;

        // Exact match, not `entry_command_contains`'s substring check: a hand-added
        // command that merely mentions `bergr_cmd` as a substring (e.g. the same
        // path invoked with extra flags) must not be mistaken for the canonical
        // hook — that would suppress installing the real one.
        let already_present = entries
            .iter()
            .any(|entry| entry_command_equals(entry, bergr_cmd));
        if !already_present {
            entries.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": bergr_cmd }]
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

/// Drops only the nested `hooks[].command` entries matching `needle`, keeping any
/// sibling commands in the same group (e.g. an unrelated audit hook next to amux's).
fn remove_matching_commands(entry: &mut Value, needle: &str) {
    let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
        return;
    };
    let mut kept = Vec::new();
    for hook in inner.drain(..) {
        let matches = match hook.get("command").and_then(Value::as_str) {
            Some(c) => c.contains(needle),
            None => false,
        };
        if !matches {
            kept.push(hook);
        }
    }
    *inner = kept;
}

/// Drops nested `hooks[].command` entries that are a previously-generated bergr hook
/// but no longer match the current `bergr_cmd` (e.g. after the binary moved), keeping
/// any sibling commands in the same group.
fn remove_stale_bergr_commands(entry: &mut Value, bergr_cmd: &str) {
    let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
        return;
    };
    let mut kept = Vec::new();
    for hook in inner.drain(..) {
        let stale = match hook.get("command").and_then(Value::as_str) {
            Some(c) => c != bergr_cmd && is_generated_bergr_command(c),
            None => false,
        };
        if !stale {
            kept.push(hook);
        }
    }
    *inner = kept;
}

/// True for a command shaped like a `bergr init`-generated hook: a single
/// shell-quoted path whose basename is `bergr`, followed by the `event` subcommand.
/// Matching by shape (not exact string) is what lets re-init recognize its own prior
/// output even when the binary's path has changed since the last run.
fn is_generated_bergr_command(command: &str) -> bool {
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
    Path::new(&path).file_name().and_then(|n| n.to_str()) == Some("bergr")
}

fn entry_hooks_is_empty(entry: &Value) -> bool {
    match entry.get("hooks").and_then(Value::as_array) {
        Some(inner) => inner.is_empty(),
        None => true,
    }
}

/// Detects a still-running amux watcher so `init` can warn about it — a live watcher
/// would keep polling the legacy cache dir and fighting bergr's own renames. A stale
/// `watch.pid` (process exited, or its PID reused by something else) does not count:
/// checking `/proc/<pid>/cmdline` for "amux watch" avoids telling the user to kill an
/// unrelated process that happens to have reused the same PID.
pub fn find_running_amux_watchers(legacy_cache_root: &Path) -> Vec<String> {
    let Ok(sessions) = fs::read_dir(legacy_cache_root) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in sessions {
        let Ok(entry) = entry else { continue };
        let pid_path = entry.path().join("watch.pid");
        let Ok(pid_text) = fs::read_to_string(&pid_path) else {
            continue;
        };
        let Ok(pid) = pid_text.trim().parse::<u32>() else {
            continue;
        };
        if !is_amux_watch_process(pid) {
            continue;
        }
        if let Ok(name) = entry.file_name().into_string() {
            names.push(name);
        }
    }
    names
}

/// Confirms PID both is alive and is actually running `amux watch`, not merely that
/// the PID exists (a stale PID can be reused by an unrelated process).
#[cfg(target_os = "linux")]
fn is_amux_watch_process(pid: u32) -> bool {
    match fs::read(Path::new("/proc").join(pid.to_string()).join("cmdline")) {
        Ok(cmdline) => {
            let args: Vec<&str> = cmdline
                .split(|&b| b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| std::str::from_utf8(a).unwrap_or(""))
                .collect();
            argv_is_amux_watch(&args)
        }
        Err(_) => false,
    }
}

/// Same check as the Linux path, but without `/proc`: ask `ps` for the command line.
/// `ps`'s reconstructed string is whitespace-joined, not true argv, so this is only
/// as precise as splitting on whitespace allows — good enough to reject an unrelated
/// process whose args merely *mention* "amux" and "watch" somewhere.
#[cfg(not(target_os = "linux"))]
fn is_amux_watch_process(pid: u32) -> bool {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let Ok(cmdline) = std::str::from_utf8(&output.stdout) else {
        return false;
    };
    let args: Vec<&str> = cmdline.split_whitespace().collect();
    argv_is_amux_watch(&args)
}

/// True when the process's first two words are `amux watch` — not merely present
/// anywhere in the command line (e.g. `sh -c 'echo amux watch'` must not match).
/// `args[0]` may itself be `"amux watch"` as one word (amux sets argv[0] that way
/// via `exec -a`), so words are gathered by splitting each arg on whitespace first.
fn argv_is_amux_watch(args: &[&str]) -> bool {
    let mut words = args.iter().flat_map(|a| a.split_whitespace());
    let Some(program) = words.next() else {
        return false;
    };
    let basename = program.rsplit('/').next().unwrap_or(program);
    basename == "amux" && words.next() == Some("watch")
}

/// Quotes `s` for safe use as a single argument in a POSIX shell command line,
/// even when `s` itself contains single quotes (e.g. `/Users/Jane O'Connor/bergr`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Exact inverse of `shell_quote`'s body (the text between the outer quotes):
/// walks `body` looking for the literal `'\''` escape sequence at each position,
/// rather than blindly replacing every occurrence of that substring — a byte-for-byte
/// scan is required because `shell_quote` only ever emits `'\''` as a whole escape
/// unit, so any other alignment of those bytes in `body` is real content, not an
/// escape, and must be left untouched.
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

pub fn tmux_conf_contents(bergr_bin: &str) -> String {
    // `#{q:session_name}` asks tmux itself to shell-quote the expanded session name —
    // `bergr_bin` is quoted up front since it's known before tmux ever expands this
    // string, but the session name isn't known until expansion time, so only tmux's
    // own format modifier can quote it safely (session names may contain spaces,
    // quotes, `$`, backticks, or `;` — tmux only rejects `.` and `:`).
    format!(
        "# ~/.config/bergr/tmux.conf — managed by `bergr init`, do not edit\n\
         set -g allow-rename off\n\
         set -g automatic-rename off\n\
         bind-key M run-shell \"{} sync --session #{{q:session_name}}\"\n",
        shell_quote(bergr_bin)
    )
}

fn home() -> PathBuf {
    let home = env::var("HOME").expect("HOME must be set");
    if home.is_empty() {
        panic!("HOME must be set");
    }
    PathBuf::from(home)
}

fn exit_on_error<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    result.unwrap_or_else(|e| {
        eprintln!("bergr init: {context}: {e}");
        std::process::exit(1);
    })
}

fn resolve_bergr_bin() -> String {
    let exe = env::current_exe().expect("could not resolve current executable path");
    if exe.components().any(|c| c.as_os_str() == "target") {
        eprintln!(
            "bergr init: refusing to run from a build directory ({}).\n\
             Install first: cargo install --path . --root ~/.local",
            exe.display()
        );
        std::process::exit(1);
    }
    exe.clone().into_os_string().into_string().unwrap_or_else(|_| {
        eprintln!(
            "bergr init: executable path is not valid UTF-8 ({}); \
             move bergr to a path with only UTF-8 characters and re-run init.",
            exe.display()
        );
        std::process::exit(1);
    })
}

/// Merges bergr's hook into `~/.claude/settings.json`, backing up the previous
/// contents once (on first run only, since re-running init should not clobber
/// a backup that predates any bergr changes).
fn update_claude_settings(bergr_bin: &str) -> PathBuf {
    let settings_path = home().join(".claude").join("settings.json");

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
        merge_hooks(&mut settings, &format!("{} event", shell_quote(bergr_bin))),
        &format!("{} has an unexpected shape", settings_path.display()),
    );
    let rendered = serde_json::to_string_pretty(&settings).unwrap();
    exit_on_error(
        write_atomic(&settings_path, &rendered),
        &format!("could not write {}", settings_path.display()),
    );

    settings_path
}

fn bergr_config_dir() -> PathBuf {
    xdg_subdir("XDG_CONFIG_HOME", ".config", "bergr")
}

/// `$xdg_var/name`, falling back to `$HOME/home_fallback_dir/name` when the XDG
/// var is unset, empty, or relative — the same fallback rule the amux prototype
/// used. A relative value is rejected rather than resolved against the current
/// directory: `init`/`sync` and this process can run from different working
/// directories, so a relative path would point at different trees for each,
/// and `legacy_amux_cache_root` feeds this into `bergr reset`'s recursive delete.
fn xdg_subdir(xdg_var: &str, home_fallback_dir: &str, name: &str) -> PathBuf {
    match env::var(xdg_var) {
        Ok(dir) if Path::new(&dir).is_absolute() => PathBuf::from(dir).join(name),
        _ => home().join(home_fallback_dir).join(name),
    }
}

fn write_bergr_tmux_conf(bergr_bin: &str) -> PathBuf {
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

fn legacy_amux_tmux_conf_path() -> PathBuf {
    xdg_subdir("XDG_CONFIG_HOME", ".config", "amux").join("tmux.conf")
}

fn is_source_line_for(line: &str, path: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("source-file") else {
        return false;
    };
    let arg = rest.trim().trim_matches('"').trim_matches('\'');
    arg == path
}

fn sources_path(tmux_conf_contents: &str, path: &Path) -> bool {
    let path = path.to_string_lossy();
    for line in tmux_conf_contents.lines() {
        if is_source_line_for(line, &path) {
            return true;
        }
    }
    false
}

fn strip_source_line(tmux_conf_contents: &str, path: &Path) -> String {
    let path = path.to_string_lossy();
    let mut result = String::new();
    for line in tmux_conf_contents.lines() {
        if !is_source_line_for(line, &path) {
            result.push_str(line);
            result.push('\n');
        }
    }
    result
}

/// Removes the old amux `source-file` line from `~/.tmux.conf`, backing up the
/// previous contents once (on first run only, mirroring `update_claude_settings`)
/// so amux's `bind-key M` can no longer override bergr's.
fn remove_stale_amux_source_line(user_tmux_conf: &Path, contents: &str) {
    let legacy_path = legacy_amux_tmux_conf_path();
    if !sources_path(contents, &legacy_path) {
        return;
    }

    let backup_path = user_tmux_conf.with_extension("conf.bergr-bak");
    if !backup_path.exists() {
        exit_on_error(
            fs::copy(user_tmux_conf, &backup_path),
            "could not back up .tmux.conf",
        );
    }

    let updated = strip_source_line(contents, &legacy_path);
    exit_on_error(
        write_atomic(user_tmux_conf, &updated),
        &format!("could not write {}", user_tmux_conf.display()),
    );

    println!(
        "bergr init: removed stale amux `source-file` line from {} (backup: {})",
        user_tmux_conf.display(),
        backup_path.display()
    );
}

fn report_tmux_conf_sourcing(tmux_conf_path: &Path) {
    let user_tmux_conf = home().join(".tmux.conf");
    let source_line = format!("source-file \"{}\"", tmux_conf_path.display());
    let contents = fs::read_to_string(&user_tmux_conf).unwrap_or_default();
    let already_sourced = sources_path(&contents, tmux_conf_path);

    remove_stale_amux_source_line(&user_tmux_conf, &contents);

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

pub(crate) fn legacy_amux_cache_root() -> PathBuf {
    xdg_subdir("XDG_CACHE_HOME", ".cache", "amux")
}

fn warn_about_live_amux_watchers() {
    let legacy_cache = legacy_amux_cache_root();
    let sessions = find_running_amux_watchers(&legacy_cache);
    for session in sessions {
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
    fn keeps_sibling_command_in_same_hook_group_as_amux() {
        let mut settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [
                            { "type": "command", "command": "amux mark --state working" },
                            { "type": "command", "command": "audit-log record" }
                        ]
                    }
                ]
            }
        });
        merge_hooks(&mut settings, "/x/bergr event").unwrap();

        let pre_tool_use = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(
            pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "audit-log record")),
            "sibling command in the same hook group must survive"
        );
        assert!(
            !pre_tool_use
                .iter()
                .any(|e| entry_command_contains(e, "amux")),
            "amux command must still be removed"
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
    fn moved_binary_replaces_stale_bergr_entry_instead_of_appending() {
        let mut settings = load_fixture();
        let old_cmd = "'/old/path/bergr' event";
        merge_hooks(&mut settings, old_cmd).unwrap();

        let new_cmd = "'/new/path/bergr' event";
        merge_hooks(&mut settings, new_cmd).unwrap();

        for event in HOOK_EVENTS {
            let entries = settings["hooks"][event].as_array().unwrap();
            assert!(
                !entries.iter().any(|e| entry_command_contains(e, old_cmd)),
                "event {event} should no longer reference the old bergr path"
            );
            let new_count = entries
                .iter()
                .filter(|e| entry_command_contains(e, new_cmd))
                .count();
            assert_eq!(
                new_count, 1,
                "event {event} should have exactly one bergr entry after the move"
            );
        }
    }

    #[test]
    fn is_generated_bergr_command_matches_quoted_path_with_event_suffix() {
        assert!(is_generated_bergr_command("'/x/bergr' event"));
        assert!(is_generated_bergr_command(
            "'/Users/Jane O'\\''Connor/bergr' event"
        ));
        assert!(!is_generated_bergr_command("'/x/bergr' sync"));
        assert!(!is_generated_bergr_command("rtk hook claude"));
        assert!(!is_generated_bergr_command("'/x/bergr-migration' event"));
    }

    #[test]
    fn is_generated_bergr_command_matches_unquoted_legacy_shape() {
        assert!(is_generated_bergr_command("/x/bergr event"));
        assert!(!is_generated_bergr_command("/x/bergr-migration event"));
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
    fn argv_matches_amux_watch() {
        assert!(argv_is_amux_watch(&["amux", "watch"]));
        assert!(argv_is_amux_watch(&[
            "/usr/local/bin/amux",
            "watch",
            "--verbose"
        ]));
    }

    #[test]
    fn argv_rejects_unrelated_process() {
        assert!(!argv_is_amux_watch(&["sleep", "999999"]));
        assert!(!argv_is_amux_watch(&["amux", "status"]));
    }

    #[test]
    fn argv_rejects_words_mentioned_out_of_position() {
        assert!(!argv_is_amux_watch(&[
            "sh",
            "-c",
            "echo amux watch; sleep 60"
        ]));
        assert!(!argv_is_amux_watch(&["watch", "amux"]));
    }

    #[test]
    fn ignores_live_pid_that_is_not_amux_watch() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("myproject");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("watch.pid"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert!(find_running_amux_watchers(dir.path()).is_empty());
    }

    #[test]
    fn detects_running_watcher_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("myproject");
        fs::create_dir_all(&session_dir).unwrap();
        let mut child = std::process::Command::new("bash")
            .arg("-c")
            .arg("exec -a 'amux watch' sleep 60")
            .spawn()
            .unwrap();
        fs::write(session_dir.join("watch.pid"), child.id().to_string()).unwrap();

        // `exec -a` renames argv[0] after bash starts, so poll briefly for it to land
        // instead of racing the immediate post-spawn state (still "bash -c ...").
        let mut detected = Vec::new();
        for _ in 0..50 {
            detected = find_running_amux_watchers(dir.path());
            if !detected.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(detected, vec!["myproject"]);

        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn ignores_stale_watcher_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().join("myproject");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(session_dir.join("watch.pid"), "999999999").unwrap();
        assert!(find_running_amux_watchers(dir.path()).is_empty());
    }

    #[test]
    fn tmux_conf_uses_absolute_path_and_session_flag() {
        let conf = tmux_conf_contents("/home/schimetschka/.local/bin/bergr");
        assert!(conf.contains("'/home/schimetschka/.local/bin/bergr' sync --session"));
        assert!(conf.contains("allow-rename off"));
    }

    #[test]
    fn tmux_conf_quotes_path_with_spaces() {
        let conf = tmux_conf_contents("/Users/Jane Doe/.local/bin/bergr");
        assert!(conf.contains("'/Users/Jane Doe/.local/bin/bergr' sync --session"));
    }

    #[test]
    fn tmux_conf_quotes_expanded_session_name() {
        // `#{session_name}` is expanded by tmux into the shell command *before* the
        // shell sees it, so an unquoted expansion is a shell-injection vector via a
        // maliciously or accidentally named session (tmux only rejects `.` and `:`
        // in session names — spaces, quotes, `$`, backticks, `;` are all legal).
        // `#{q:...}` asks tmux itself to shell-quote the expansion.
        let conf = tmux_conf_contents("/x/bergr");
        assert!(conf.contains("sync --session #{q:session_name}"));
    }

    #[test]
    fn tmux_conf_escapes_path_with_single_quote() {
        let conf = tmux_conf_contents("/Users/Jane O'Connor/.local/bin/bergr");
        assert!(conf.contains("'/Users/Jane O'\\''Connor/.local/bin/bergr' sync --session"));
    }

    #[test]
    fn shell_quote_escapes_embedded_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("has'quote"), "'has'\\''quote'");
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
    fn xdg_subdir_falls_back_when_var_relative() {
        // SAFETY: single-threaded test, restored immediately after use.
        unsafe {
            env::set_var("BERGR_TEST_RELATIVE_XDG_VAR", "relative/cache");
        }
        let result = xdg_subdir("BERGR_TEST_RELATIVE_XDG_VAR", ".cache", "amux");
        unsafe {
            env::remove_var("BERGR_TEST_RELATIVE_XDG_VAR");
        }
        assert_eq!(result, home().join(".cache").join("amux"));
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

    #[test]
    fn legacy_amux_tmux_conf_path_uses_config_fallback_shape() {
        assert_eq!(
            legacy_amux_tmux_conf_path(),
            home().join(".config").join("amux").join("tmux.conf")
        );
    }

    #[test]
    fn strip_source_line_removes_only_the_matching_directive() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "set -g mouse on\nsource-file /home/x/.config/amux/tmux.conf\nset -g history-limit 5000\n";
        assert_eq!(
            strip_source_line(conf, path),
            "set -g mouse on\nset -g history-limit 5000\n"
        );
    }

    #[test]
    fn strip_source_line_keeps_commented_out_line() {
        let path = Path::new("/home/x/.config/amux/tmux.conf");
        let conf = "# source-file /home/x/.config/amux/tmux.conf\n";
        assert_eq!(
            strip_source_line(conf, path),
            "# source-file /home/x/.config/amux/tmux.conf\n"
        );
    }
}
