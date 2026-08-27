use crate::fs_util;
use crate::hook::HookPayload;
use crate::name::strip_suffix;
use crate::state::{self, StateRecord};
use crate::tmux;
use std::env;
use std::io::{Read, stdin};
use std::process::Command;

/// Resolves the agent name: `$BERGR_AGENT` override (mirrors amux's `AMUX_AGENT`),
/// else the current tmux window name (suffix stripped), else `basename($PWD)`.
///
/// Not unit-tested — every branch depends on process-global state that isn't
/// injectable/mockable here. `strip_suffix` is covered separately in `name.rs`.
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
/// renames the current window.
///
/// Never fails outward — every error is logged and treated as a no-op, since this
/// sits on Claude Code's hook path where a non-zero exit could block a tool call.
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
            if let Err(e) = state::remove_state_file(&path) {
                eprintln!("bergr event: failed deleting {}: {e}", path.display());
                return;
            }
            rename_current_window(&session, &agent);
        }
        Some(new_state) => {
            let new_name = format!("{agent}{}", new_state.symbol());
            let record = StateRecord {
                agent: agent.clone(),
                state: new_state,
                updated_at: now_utc(),
                harness: "claude".to_string(),
                session: session.clone(),
                window: new_name.clone(),
                window_id: tmux::current_window_id(),
            };
            if let Err(e) = fs_util::write_atomic(&path, &record.to_kv()) {
                eprintln!("bergr event: failed writing {}: {e}", path.display());
                return;
            }
            rename_current_window(&session, &new_name);
        }
    }
}

/// Renames the current window by its live index, not by matching `agent` against
/// window names — that breaks when `BERGR_AGENT` diverges from the window name.
fn rename_current_window(session: &str, new_name: &str) {
    let Some(index) = tmux::current_window_index() else {
        return;
    };
    if !tmux::rename_window(session, &index, new_name) {
        eprintln!("bergr event: failed to rename window {index} to '{new_name}'");
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
            Err(e) => {
                eprintln!("bergr event: `date` output was not valid UTF-8: {e}");
                String::new()
            }
        },
        Err(e) => {
            eprintln!("bergr event: could not run `date`: {e}");
            String::new()
        }
    }
}
