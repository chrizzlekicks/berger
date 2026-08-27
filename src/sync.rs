use crate::fs_util::encode_path_component;
use crate::reconcile::plan_renames;
use crate::state::{self, cache_root};
use crate::tmux::{self, Window};
use std::fs;
use std::process;

/// Reconciles every window in a session against its state files — equivalent to one
/// tick of the amux prototype's polling watcher, run on demand instead of every 2s.
///
/// `session` is required rather than inferred, mirroring why the tmux keybinding
/// passes `#{session_name}` explicitly: `run-shell` does not execute inside a pane,
/// so `$TMUX_PANE` may not resolve there.
pub fn run(session: &str) {
    let dir = match cache_root() {
        Ok(root) => root.join(encode_path_component(session)),
        Err(e) => {
            eprintln!("bergr sync: {e}");
            process::exit(1);
        }
    };

    let mut records = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries {
            let Ok(entry) = entry else { continue };
            if entry.path().extension().is_none_or(|ext| ext != "state") {
                continue;
            }
            if let Some(record) = state::read_record(&entry.path()) {
                records.push(record);
            }
        }
    }
    // else: no state for this session yet — nothing to reconcile

    let Some(windows) = tmux::list_windows(session) else {
        eprintln!("bergr sync: could not list windows for session '{session}'");
        process::exit(1);
    };

    for rename in plan_renames(&records, &windows) {
        if !tmux::rename_window(session, &rename.index, &rename.new_name) {
            eprintln!(
                "bergr sync: failed to rename window {} to '{}'",
                rename.index, rename.new_name
            );
        }
    }

    prune_orphaned(session, &records, &windows);
}

/// Deletes state files for agents whose window no longer exists — the window may
/// have been killed outside of Claude Code's `SessionEnd` hook, which is the only
/// other place state gets cleaned up.
fn prune_orphaned(session: &str, records: &[state::StateRecord], windows: &[Window]) {
    for record in records {
        if is_orphaned(record, windows)
            && let Ok(path) = state::state_path(session, &record.agent)
            && let Err(e) = fs::remove_file(&path)
        {
            eprintln!("bergr sync: failed removing {}: {e}", path.display());
        }
    }
}

/// A record is orphaned when no window's (suffix-stripped) name matches its agent —
/// the window was closed outside of Claude Code's `SessionEnd` hook.
fn is_orphaned(record: &state::StateRecord, windows: &[Window]) -> bool {
    !windows
        .iter()
        .any(|w| crate::reconcile::agent_matches(&w.name, &record.agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{State, StateRecord};

    fn record(agent: &str) -> StateRecord {
        StateRecord {
            agent: agent.to_string(),
            state: State::Working,
            updated_at: "t".to_string(),
            harness: "claude".to_string(),
            session: "s".to_string(),
            window: agent.to_string(),
        }
    }

    fn window(name: &str) -> Window {
        Window {
            index: "0".to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn orphaned_record_has_no_live_window() {
        let windows = [window("impl!")];
        assert!(!is_orphaned(&record("impl"), &windows));
        assert!(is_orphaned(&record("gone"), &windows));
    }

    #[test]
    fn no_orphans_when_every_record_has_a_window() {
        let windows = [window("impl")];
        assert!(!is_orphaned(&record("impl"), &windows));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let windows = [window("Impl!")];
        assert!(!is_orphaned(&record("impl"), &windows));
    }
}
