use crate::name::strip_suffix;
use crate::state::StateRecord;
use crate::tmux::Window;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub index: String,
    pub new_name: String,
}

/// Given every state record for a session and every window currently in that
/// session, compute the renames needed to make window names reflect state.
///
/// A window matches a record when its name, with any suffix stripped, equals the
/// record's agent. Windows with no matching record are left alone. A rename that
/// would produce the window's current name is omitted — this is what makes `event`
/// and repeated `sync` calls idempotent.
pub fn plan_renames(records: &[StateRecord], windows: &[Window]) -> Vec<Rename> {
    windows
        .iter()
        .filter_map(|window| {
            let base = strip_suffix(&window.name);
            let record = records.iter().find(|r| r.agent == base)?;
            let new_name = format!("{base}{}", record.state.symbol());
            if new_name == window.name {
                return None;
            }
            Some(Rename {
                index: window.index.clone(),
                new_name,
            })
        })
        .collect()
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
        }
    }

    fn window(index: &str, name: &str) -> Window {
        Window {
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
}
