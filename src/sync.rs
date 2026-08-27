use crate::fs_util::encode_path_component;
use crate::reconcile::plan_renames;
use crate::state::{self, cache_root};
use crate::tmux::{self, Window};
use std::fs;
use std::process;

/// Reconciles every window in a session against its state files — one on-demand
/// tick of what amux's polling watcher did every 2s.
///
/// `session` is required rather than inferred: `run-shell` doesn't execute inside
/// a pane, so `$TMUX_PANE` may not resolve there.
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

/// A record is orphaned when its window is closed, not merely renamed. A name-based
/// check alone can't tell those apart (a `BERGR_AGENT`-driven rename also stops
/// matching by name), so we prefer the stable `window_id` when the record has one;
/// older records without it fall back to the name-based check.
fn is_orphaned(record: &state::StateRecord, windows: &[Window]) -> bool {
    match &record.window_id {
        Some(id) => !windows.iter().any(|w| &w.id == id),
        None => !windows
            .iter()
            .any(|w| crate::reconcile::agent_matches(&w.name, &record.agent)),
    }
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
            window_id: None,
        }
    }

    fn record_with_window_id(agent: &str, window_id: &str) -> StateRecord {
        StateRecord {
            window_id: Some(window_id.to_string()),
            ..record(agent)
        }
    }

    fn window(id: &str, name: &str) -> Window {
        Window {
            id: id.to_string(),
            index: "0".to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn orphaned_record_has_no_live_window() {
        let windows = [window("@1", "impl!")];
        assert!(!is_orphaned(&record("impl"), &windows));
        assert!(is_orphaned(&record("gone"), &windows));
    }

    #[test]
    fn no_orphans_when_every_record_has_a_window() {
        let windows = [window("@1", "impl")];
        assert!(!is_orphaned(&record("impl"), &windows));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let windows = [window("@1", "Impl!")];
        assert!(!is_orphaned(&record("impl"), &windows));
    }

    #[test]
    fn window_id_retains_record_when_window_is_only_renamed() {
        // BERGR_AGENT drifted the window away from a name-based match, but the
        // window itself (tracked by id) is still alive — must not be pruned.
        let windows = [window("@1", "project")];
        assert!(!is_orphaned(&record_with_window_id("impl", "@1"), &windows));
    }

    #[test]
    fn window_id_orphans_record_when_window_is_actually_closed() {
        let windows = [window("@2", "other")];
        assert!(is_orphaned(&record_with_window_id("impl", "@1"), &windows));
    }
}
