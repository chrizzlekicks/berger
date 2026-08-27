use crate::fs_util;
use crate::fs_util::encode_path_component;
use crate::hook::HookPayload;
use crate::name::strip_suffix;
use crate::reconcile::window_matches_record;
use crate::state::{self, StateRecord};
use crate::tmux::{self, Window};
use std::env;
use std::io::{Read, stdin};
use std::path::PathBuf;
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

    match state::state_for_event(&payload.hook_event_name) {
        None => {
            // SessionEnd (or an unrecognized event): clear state and strip any
            // suffix from the window, rather than leaving it stale. This is the one
            // path where, with no watcher to notice the file vanish, bergr itself
            // must actively clear the suffix.
            //
            // Uses the same window-to-record matching as `plan_renames`
            // (window_matches_record), not `agent`-derived paths and names: if the
            // window was renamed since its record was written (e.g. BERGR_AGENT
            // diverging from the window name), both the record to delete and the
            // name to restore must be found from the window's live identity, not
            // resolve_agent()'s output, or the wrong file gets deleted and/or the
            // window gets renamed to a name it never had.
            //
            // If the window's identity can't be read at all (tmux failing mid-call,
            // not merely "not inside tmux" — that already returned above at
            // `current_session()`), this skips the rename too rather than falling
            // back to `agent`: a stale suffix left behind here is recoverable via
            // `bergr sync`, but renaming to a fabricated name would not be.
            let Some(window) = current_window() else {
                return;
            };
            let base = strip_suffix(&window.name);
            // Delete by the path the matching record was actually read from — not
            // one recomputed from `agent` — mirroring `sync`'s prune_orphaned
            // (src/sync.rs): a record whose filename has drifted from its current
            // agent/window must still be found and removed.
            if let Some(path) = find_record_path_for_window(&session, &window)
                && let Err(e) = state::remove_state_file(&path)
            {
                eprintln!("bergr event: failed deleting {}: {e}", path.display());
                return;
            }
            rename_current_window(&session, &base);
        }
        Some(new_state) => {
            let path = match state::state_path(&session, &agent) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("bergr event: cannot resolve state path: {e}");
                    return;
                }
            };
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

/// The current tmux window as a `tmux::Window`, or `None` if any of its id, index,
/// or name can't be read (e.g. not inside tmux).
fn current_window() -> Option<Window> {
    Some(Window {
        id: tmux::current_window_id()?,
        index: tmux::current_window_index()?,
        name: tmux::current_window_name()?,
    })
}

/// Scans `session`'s state directory for the record matching `window` (see
/// `reconcile::window_matches_record`), returning the path it was actually read
/// from. Mirrors `sync`'s orphan-pruning scan (src/sync.rs) — both go through
/// `state::read_session_records` and the same matching predicate, so they agree on
/// how a record is found by window identity rather than by re-deriving a path from
/// an agent name.
fn find_record_path_for_window(session: &str, window: &Window) -> Option<PathBuf> {
    let dir = state::cache_root()
        .ok()?
        .join(encode_path_component(session));
    for (path, record) in state::read_session_records(&dir) {
        if window_matches_record(window, &record) {
            return Some(path);
        }
    }
    None
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
