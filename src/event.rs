use crate::fs_util;
use crate::hook::HookPayload;
use crate::name::strip_suffix;
use crate::state::{self, StateRecord};
use crate::tmux;
use std::env;
use std::fs;
use std::io::{Read, stdin};
use std::process::Command;

/// Resolves the agent name for this invocation: `$BERGR_AGENT` overrides (mirrors the
/// amux prototype's `AMUX_AGENT`, letting the tracked agent differ from the window
/// name), then the current tmux window name (suffix stripped, since a window bergr
/// already renamed must still match its own state file), then `basename($PWD)`.
fn resolve_agent() -> String {
    if let Ok(a) = env::var("BERGR_AGENT")
        && !a.is_empty()
    {
        return strip_suffix(&a);
    }
    if let Some(window) = tmux::current_window_name() {
        return strip_suffix(&window);
    }
    match env::current_dir() {
        Ok(p) => match p.file_name() {
            Some(n) => n.to_string_lossy().into_owned(),
            None => String::new(),
        },
        Err(_) => String::new(),
    }
}

/// Runs the `event` command: reads a hook payload from stdin, updates state, and
/// (when a tmux session is resolvable) renames the corresponding window.
///
/// Never fails outward. Every error path is logged to stderr and treated as a no-op —
/// `bergr event` sits on Claude Code's hook path, where a non-zero exit can block a
/// tool call or prompt, so a bergr bug must never be able to interfere with the
/// user's session.
pub fn run() {
    let mut input = String::new();
    if stdin().read_to_string(&mut input).is_err() {
        eprintln!("bergr event: failed to read stdin");
        return;
    }

    let payload: HookPayload = match serde_json::from_str(&input) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bergr event: malformed hook payload: {e}");
            return;
        }
    };

    let Some(session) = tmux::current_session() else {
        // Not inside tmux — a normal condition (plain terminal, IDE, CI), not an
        // error. Nothing to update.
        return;
    };
    let agent = resolve_agent();

    let path = match state::state_path(&session, &agent) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("bergr event: cannot resolve state path: {e}");
            return;
        }
    };

    match state::state_for_event(&payload.hook_event_name) {
        None => {
            // SessionEnd (or an unrecognized event): clear state and strip any
            // suffix from the window, rather than leaving it stale. This is the one
            // path where, with no watcher to notice the file vanish, bergr itself
            // must actively clear the suffix.
            let _ = fs::remove_file(&path);
            rename_matching_window(&session, &agent, &agent);
        }
        Some(new_state) => {
            let record = StateRecord {
                agent: agent.clone(),
                state: new_state,
                updated_at: now_utc(),
                harness: "claude".to_string(),
                session: session.clone(),
                window: agent.clone(),
            };
            if let Err(e) = fs_util::write_atomic(&path, &record.to_kv()) {
                eprintln!("bergr event: failed writing {}: {e}", path.display());
                return;
            }
            let new_name = format!("{agent}{}", new_state.symbol());
            rename_matching_window(&session, &agent, &new_name);
        }
    }
}

fn rename_matching_window(session: &str, agent: &str, new_name: &str) {
    let Some(windows) = tmux::list_windows(session) else {
        return;
    };
    for window in windows {
        if strip_suffix(&window.name) != agent {
            continue;
        }
        if window.name != new_name {
            tmux::rename_window(session, &window.index, new_name);
        }
    }
}

fn now_utc() -> String {
    // No chrono dependency: shell out, matching the prototype's own `date -u`.
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match output {
        Ok(o) => match String::from_utf8(o.stdout) {
            Ok(s) => s.trim().to_string(),
            Err(_) => String::new(),
        },
        Err(_) => String::new(),
    }
}
