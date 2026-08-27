use crate::name::strip_suffix;
use crate::state::StateRecord;
use crate::tmux::Window;

/// Agent/session names fold ASCII case on the on-disk path (see
/// `fs_util::encode_path_component`), so window-to-record matching must fold
/// ASCII case too, to agree with how state files are keyed.
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
/// Shared by `plan_renames` (below) and `event`'s `SessionEnd` handler, so both
/// agree on how a window's record is identified rather than by re-deriving a path
/// from an agent name.
pub fn window_matches_record(window: &Window, record: &StateRecord) -> bool {
    match &record.window_id {
        Some(id) => &window.id == id,
        None => agent_matches(&window.name, &record.agent),
    }
}

/// Given every state record for a session and every window currently in that
/// session, compute the renames needed to make window names reflect state.
///
/// Windows with no matching record (see `window_matches_record`) are left alone. A
/// rename that would produce the window's current name is omitted — this is what
/// makes `event` and repeated `sync` calls idempotent.
pub fn plan_renames(records: &[StateRecord], windows: &[Window]) -> Vec<Rename> {
    let mut renames = Vec::new();
    for window in windows {
        let mut matching_record = None;
        for record in records {
            if window_matches_record(window, record) {
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
            plan_renames(&records, &windows),
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
            plan_renames(&records, &windows),
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
            plan_renames(&records, &windows),
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
        assert!(plan_renames(&records, &windows).is_empty());
    }

    #[test]
    fn window_with_no_matching_record_is_untouched() {
        let records = vec![record("impl", State::Approval)];
        let windows = vec![window("0", "main"), window("1", "impl")];
        assert_eq!(
            plan_renames(&records, &windows),
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
            plan_renames(&records, &windows),
            vec![Rename {
                index: "1".to_string(),
                new_name: "impl".to_string()
            }]
        );
    }

    #[test]
    fn empty_inputs_produce_no_renames() {
        assert!(plan_renames(&[], &[]).is_empty());
    }

    #[test]
    fn matches_by_window_id_when_name_has_drifted_from_agent() {
        // BERGR_AGENT="impl" in a window actually named "project": name-based
        // matching would miss this record entirely, but window_id still finds it.
        let mut drifted = record("impl", State::Approval);
        drifted.window_id = Some("@1".to_string());
        let windows = vec![window("1", "project")];
        assert_eq!(
            plan_renames(&[drifted], &windows),
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
        assert!(plan_renames(&[record_for_other_window], &windows).is_empty());
    }
}
