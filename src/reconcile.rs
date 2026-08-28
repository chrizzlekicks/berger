use crate::name::strip_suffix;
use crate::state::StateRecord;
use crate::tmux::Window;

/// Agent names fold ASCII case on the on-disk path (see
/// `fs_util::encode_path_component`), so window-to-record matching must fold
/// ASCII case too, to agree with how state files are keyed. Session names are
/// no longer folded this way (see `fs_util::encode_session_component`) — two
/// live tmux sessions differing only by case are genuinely distinct.
pub fn agent_matches(window_name: &str, agent: &str) -> bool {
    strip_suffix(window_name).eq_ignore_ascii_case(agent)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub index: String,
    pub new_name: String,
}

/// True when `record` is the one `window` represents: matched by the record's
/// stable `window_id` when it has one — this is what lets a window whose name has
/// drifted away from its agent (e.g. via `BERGR_AGENT`) still be found. Records with
/// no `window_id` (written before that field existed) fall back to matching by
/// name, with any suffix stripped.
///
/// A `window_id` match additionally requires `record.server_pid` to match
/// `current_server_pid` when the record has one: tmux hands out window ids
/// starting from `@0` again on every fresh server, so after a restart (reboot,
/// `tmux kill-server`) a live window can carry an id a stale record also used for
/// a since-closed window. A pid mismatch means the id can't be trusted, so the
/// record simply doesn't match this window — it does not fall back to the
/// name-based check, which would risk pairing it with an unrelated window that
/// happens to share a stripped name.
///
/// Shared by `plan_renames` (below) and `event`'s `SessionEnd` handler, so both
/// agree on how a window's record is identified rather than by re-deriving a path
/// from an agent name.
pub fn window_matches_record(
    window: &Window,
    record: &StateRecord,
    current_server_pid: Option<&str>,
) -> bool {
    match &record.window_id {
        Some(id) => {
            &window.id == id
                && match &record.server_pid {
                    Some(recorded) => current_server_pid == Some(recorded.as_str()),
                    None => true,
                }
        }
        None => agent_matches(&window.name, &record.agent),
    }
}

/// Given every state record for a session and every window currently in that
/// session, compute the renames needed to make window names reflect state.
///
/// Windows with no matching record (see `window_matches_record`) are left alone. A
/// rename that would produce the window's current name is omitted — this is what
/// makes `event` and repeated `sync` calls idempotent.
pub fn plan_renames(
    records: &[StateRecord],
    windows: &[Window],
    current_server_pid: Option<&str>,
) -> Vec<Rename> {
    let mut renames = Vec::new();
    for window in windows {
        let mut matching_record = None;
        for record in records {
            if window_matches_record(window, record, current_server_pid) {
                matching_record = Some(record);
                break;
            }
        }
        let Some(record) = matching_record else {
            continue;
        };
        let base = strip_suffix(&window.name);
        let new_name = format!("{base}{}", record.state.symbol());
        if new_name == window.name {
            continue;
        }
        renames.push(Rename {
            index: window.index.clone(),
            new_name,
        });
    }
    renames
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::State;

    fn record(agent: &str, state: State) -> StateRecord {
        StateRecord {
            agent: agent.to_string(),
            state,
            updated_at: "t".to_string(),
            harness: "claude".to_string(),
            session: "s".to_string(),
            window: agent.to_string(),
            window_id: None,
            server_pid: None,
        }
    }

    fn window(index: &str, name: &str) -> Window {
        Window {
            id: format!("@{index}"),
            index: index.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn renames_window_to_match_state() {
        let records = vec![record("impl", State::Approval)];
        let windows = vec![window("1", "impl")];
        assert_eq!(
            plan_renames(&records, &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl!".to_string()
            }]
        );
    }

    #[test]
    fn matches_window_regardless_of_existing_suffix() {
        let records = vec![record("impl", State::Done)];
        let windows = vec![window("1", "impl\u{2717}")]; // stale: was error, now done
        assert_eq!(
            plan_renames(&records, &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl\u{2713}".to_string()
            }]
        );
    }

    #[test]
    fn matches_window_regardless_of_case() {
        let records = vec![record("impl", State::Approval)];
        let windows = vec![window("1", "Impl")];
        assert_eq!(
            plan_renames(&records, &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "Impl!".to_string()
            }]
        );
    }

    #[test]
    fn no_rename_when_name_already_correct() {
        let records = vec![record("impl", State::Approval)];
        let windows = vec![window("1", "impl!")];
        assert!(plan_renames(&records, &windows, None).is_empty());
    }

    #[test]
    fn window_with_no_matching_record_is_untouched() {
        let records = vec![record("impl", State::Approval)];
        let windows = vec![window("0", "main"), window("1", "impl")];
        assert_eq!(
            plan_renames(&records, &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl!".to_string()
            }]
        );
    }

    #[test]
    fn working_state_strips_any_existing_suffix() {
        let records = vec![record("impl", State::Working)];
        let windows = vec![window("1", "impl!")];
        assert_eq!(
            plan_renames(&records, &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl".to_string()
            }]
        );
    }

    #[test]
    fn empty_inputs_produce_no_renames() {
        assert!(plan_renames(&[], &[], None).is_empty());
    }

    #[test]
    fn matches_by_window_id_when_name_has_drifted_from_agent() {
        // BERGR_AGENT="impl" in a window actually named "project": name-based
        // matching would miss this record entirely, but window_id still finds it.
        let mut drifted = record("impl", State::Approval);
        drifted.window_id = Some("@1".to_string());
        let windows = vec![window("1", "project")];
        assert_eq!(
            plan_renames(&[drifted], &windows, None),
            vec![Rename {
                index: "1".to_string(),
                new_name: "project!".to_string()
            }]
        );
    }

    #[test]
    fn window_id_takes_priority_over_a_coincidental_name_match() {
        let mut record_for_other_window = record("impl", State::Approval);
        record_for_other_window.window_id = Some("@2".to_string());
        let windows = vec![window("1", "impl")];
        assert!(plan_renames(&[record_for_other_window], &windows, None).is_empty());
    }

    #[test]
    fn window_id_match_requires_agreeing_server_pid() {
        // A fresh tmux server hands out window ids starting from @0 again, so
        // after a restart a live window can carry an id a stale record (from the
        // previous server) also used. A mismatched server_pid means the id can't
        // be trusted — this must not fall back to matching by name either.
        let mut stale = record("impl", State::Approval);
        stale.window_id = Some("@1".to_string());
        stale.server_pid = Some("111".to_string());
        let windows = vec![window("1", "impl")];
        assert!(plan_renames(&[stale], &windows, Some("222")).is_empty());
    }

    #[test]
    fn window_id_match_fails_closed_when_current_server_pid_unreadable() {
        // The record has a server_pid, but the current one couldn't be read
        // (tmux::server_pid() failed). Unable to verify the id is trustworthy —
        // must not assume it still is.
        let mut stale = record("impl", State::Approval);
        stale.window_id = Some("@1".to_string());
        stale.server_pid = Some("111".to_string());
        let windows = vec![window("1", "impl")];
        assert!(plan_renames(&[stale], &windows, None).is_empty());
    }

    #[test]
    fn window_id_match_succeeds_when_server_pid_agrees() {
        let mut record = record("impl", State::Approval);
        record.window_id = Some("@1".to_string());
        record.server_pid = Some("111".to_string());
        let windows = vec![window("1", "impl")];
        assert_eq!(
            plan_renames(&[record], &windows, Some("111")),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl!".to_string()
            }]
        );
    }
}
