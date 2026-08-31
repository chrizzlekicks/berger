use crate::fs_util::encode_session_component;
use crate::reconcile::{plan_renames, window_matches_record};
use crate::state::{self, cache_root};
use crate::tmux::{self, Window};
use std::path::PathBuf;
use std::process;

/// Reconciles every window in a session against its state files — one on-demand
/// tick of what amux's polling watcher did every 2s.
///
/// `session` is required rather than inferred: `run-shell` doesn't execute inside
/// a pane, so `$TMUX_PANE` may not resolve there.
pub fn run(session: &str) {
    let dir = match cache_root() {
        Ok(root) => root.join(encode_session_component(session)),
        Err(e) => {
            eprintln!("berger sync: {e}");
            process::exit(1);
        }
    };

    let records = state::read_session_records(&dir);

    let Some(windows) = tmux::list_windows(session) else {
        eprintln!("berger sync: could not list windows for session '{session}'");
        process::exit(1);
    };

    let server_pid = tmux::server_pid();
    let record_refs: Vec<_> = records.iter().map(|(_, r)| r.clone()).collect();
    for rename in plan_renames(&record_refs, &windows, server_pid.as_deref()) {
        if !tmux::rename_window(session, &rename.index, &rename.new_name) {
            eprintln!(
                "berger sync: failed to rename window {} to '{}'",
                rename.index, rename.new_name
            );
        }
    }

    // Only prune when the current server's pid is actually known: pruning is
    // destructive (deletes state files), and an unreadable pid means a
    // window_id match can't be verified — better to skip this pass than risk
    // treating live windows as orphaned because of a transient tmux hiccup.
    if let Some(pid) = server_pid.as_deref() {
        prune_orphaned(&records, &windows, Some(pid));
    }
}

/// Deletes state files for agents whose window no longer exists — the window may
/// have been killed outside of Claude Code's `SessionEnd` hook, which is the only
/// other place state gets cleaned up. Prunes by the path each record was actually
/// read from, not a recomputed one, so a name/agent mismatch can't leave orphans
/// stuck forever.
fn prune_orphaned(
    records: &[(PathBuf, state::StateRecord)],
    windows: &[Window],
    current_server_pid: Option<&str>,
) {
    for (path, record) in records {
        if is_orphaned(record, windows, current_server_pid)
            && let Err(e) = state::remove_state_file(path)
        {
            eprintln!("berger sync: failed removing {}: {e}", path.display());
        }
    }
}

/// A record is orphaned when no live window matches it (see
/// `reconcile::window_matches_record`) — i.e. its window is closed, not merely
/// renamed. A name-based check alone can't tell those apart (a `BERGER_AGENT`-driven
/// rename also stops matching by name), which is why that shared check prefers the
/// stable `window_id` when the record has one. A record whose `window_id` collides
/// with a live window only because a restarted tmux server reused the id, but whose
/// stripped name also happens to match, is retained rather than pruned — the same
/// coincidental-name risk `window_matches_record`'s fallback accepts, inherited
/// from `legacy/amux`.
fn is_orphaned(
    record: &state::StateRecord,
    windows: &[Window],
    current_server_pid: Option<&str>,
) -> bool {
    !windows
        .iter()
        .any(|w| window_matches_record(w, record, current_server_pid))
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
            server_pid: None,
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
        assert!(!is_orphaned(&record("impl"), &windows, None));
        assert!(is_orphaned(&record("gone"), &windows, None));
    }

    #[test]
    fn no_orphans_when_every_record_has_a_window() {
        let windows = [window("@1", "impl")];
        assert!(!is_orphaned(&record("impl"), &windows, None));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let windows = [window("@1", "Impl!")];
        assert!(!is_orphaned(&record("impl"), &windows, None));
    }

    #[test]
    fn window_id_retains_record_when_window_is_only_renamed() {
        // BERGER_AGENT drifted the window away from a name-based match, but the
        // window itself (tracked by id) is still alive — must not be pruned.
        let windows = [window("@1", "project")];
        assert!(!is_orphaned(
            &record_with_window_id("impl", "@1"),
            &windows,
            None
        ));
    }

    #[test]
    fn window_id_orphans_record_when_window_is_actually_closed() {
        let windows = [window("@2", "other")];
        assert!(is_orphaned(
            &record_with_window_id("impl", "@1"),
            &windows,
            None
        ));
    }

    #[test]
    fn window_id_orphans_record_when_server_pid_mismatches_and_name_does_not_match() {
        // The window id matches, but a restarted tmux server reused it for an
        // unrelated live window — the recorded server_pid disagrees, and the
        // window's name doesn't match either, so this must be pruned.
        let mut stale = record_with_window_id("impl", "@1");
        stale.server_pid = Some("111".to_string());
        let windows = [window("@1", "unrelated")];
        assert!(is_orphaned(&stale, &windows, Some("222")));
    }

    #[test]
    fn window_id_falls_back_to_name_when_server_pid_mismatches() {
        // Same as above, but the reused window happens to share a stripped name
        // with the stale record's agent — this is retained (matches by name),
        // the coincidental-name risk inherited from legacy/amux.
        let mut stale = record_with_window_id("impl", "@1");
        stale.server_pid = Some("111".to_string());
        let windows = [window("@1", "impl")];
        assert!(!is_orphaned(&stale, &windows, Some("222")));
    }

    #[test]
    fn prune_orphaned_deletes_by_the_path_the_record_was_read_from() {
        // The path a record is pruned by must match where it was actually read
        // from, not one recomputed from `record.agent` — otherwise a state file
        // whose name has drifted from its `agent` field is never pruned.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stale-name.state");
        std::fs::write(
            &path,
            "agent=impl\nstate=working\nharness=claude\nsession=s\n",
        )
        .unwrap();

        let records = vec![(path.clone(), record("impl"))];
        prune_orphaned(&records, &[], None);

        assert!(!path.exists());
    }
}
