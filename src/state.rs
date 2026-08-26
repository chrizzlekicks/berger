use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Approval,
    Done,
    Error,
}

impl State {
    pub fn symbol(self) -> &'static str {
        match self {
            State::Working => "",
            State::Approval => "!",
            State::Done => "\u{2713}",
            State::Error => "\u{2717}",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Approval => "approval",
            State::Done => "done",
            State::Error => "error",
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Maps a hook event name to the state it represents, or `None` if the event should
/// clear state entirely (`SessionEnd`) or is unrecognized.
pub fn state_for_event(hook_event_name: &str) -> Option<State> {
    match hook_event_name {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" => Some(State::Working),
        "PermissionRequest" => Some(State::Approval),
        "PostToolUseFailure" | "StopFailure" => Some(State::Error),
        "Stop" => Some(State::Done),
        "SessionEnd" => None,
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateRecord {
    pub agent: String,
    pub state: State,
    pub updated_at: String,
    pub harness: String,
    pub session: String,
    pub window: String,
}

impl StateRecord {
    pub fn to_kv(&self) -> String {
        format!(
            "agent={}\nstate={}\nupdated_at={}\nharness={}\nsession={}\nwindow={}\n",
            self.agent, self.state, self.updated_at, self.harness, self.session, self.window,
        )
    }

    pub fn from_kv(text: &str) -> Option<StateRecord> {
        let mut agent = None;
        let mut state = None;
        let mut updated_at = None;
        let mut harness = None;
        let mut session = None;
        let mut window = None;

        for line in text.lines() {
            let (key, val) = line.split_once('=')?;
            match key {
                "agent" => agent = Some(val.to_string()),
                "state" => state = Some(parse_state(val)?),
                "updated_at" => updated_at = Some(val.to_string()),
                "harness" => harness = Some(val.to_string()),
                "session" => session = Some(val.to_string()),
                "window" => window = Some(val.to_string()),
                _ => {}
            }
        }

        Some(StateRecord {
            agent: agent?,
            state: state?,
            updated_at: updated_at?,
            harness: harness.unwrap_or_default(),
            session: session?,
            window: window?,
        })
    }
}

fn parse_state(s: &str) -> Option<State> {
    match s {
        "working" => Some(State::Working),
        "approval" => Some(State::Approval),
        "done" => Some(State::Done),
        "error" => Some(State::Error),
        _ => None,
    }
}

/// `$XDG_CACHE_HOME`, defaulting to `$HOME/.cache` per the XDG base dir spec — same
/// fallback the amux prototype used. Errors if neither is set rather than silently
/// resolving to a relative path.
pub fn cache_root() -> io::Result<PathBuf> {
    match env::var("XDG_CACHE_HOME") {
        Ok(xdg) if Path::new(&xdg).is_absolute() => return Ok(PathBuf::from(xdg).join("bergr")),
        _ => {}
    }
    let not_set_err = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            "neither XDG_CACHE_HOME nor HOME is set",
        )
    };
    let home = env::var("HOME").map_err(|_| not_set_err())?;
    if home.is_empty() {
        return Err(not_set_err());
    }
    Ok(PathBuf::from(home).join(".cache").join("bergr"))
}

pub fn state_path(session: &str, agent: &str) -> io::Result<PathBuf> {
    Ok(cache_root()?.join(session).join(format!("{agent}.state")))
}

pub fn read_record(path: &Path) -> Option<StateRecord> {
    let text = fs::read_to_string(path).ok()?;
    StateRecord::from_kv(&text)
}

#[cfg(test)]
mod io_tests {
    use super::*;

    #[test]
    fn write_atomic_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("impl.state");
        let record = StateRecord {
            agent: "impl".to_string(),
            state: State::Approval,
            updated_at: "2026-08-19T12:00:00Z".to_string(),
            harness: "claude".to_string(),
            session: "myproject".to_string(),
            window: "impl!".to_string(),
        };
        crate::fs_util::write_atomic(&path, &record.to_kv()).unwrap();
        assert_eq!(read_record(&path), Some(record));
    }

    #[test]
    fn read_record_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.state");
        assert_eq!(read_record(&path), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_map_to_expected_states() {
        assert_eq!(state_for_event("SessionStart"), Some(State::Working));
        assert_eq!(state_for_event("UserPromptSubmit"), Some(State::Working));
        assert_eq!(state_for_event("PreToolUse"), Some(State::Working));
        assert_eq!(state_for_event("PermissionRequest"), Some(State::Approval));
        assert_eq!(state_for_event("PostToolUseFailure"), Some(State::Error));
        assert_eq!(state_for_event("StopFailure"), Some(State::Error));
        assert_eq!(state_for_event("Stop"), Some(State::Done));
    }

    #[test]
    fn session_end_clears_state() {
        assert_eq!(state_for_event("SessionEnd"), None);
    }

    #[test]
    fn unknown_event_is_none() {
        assert_eq!(state_for_event("SomethingNew"), None);
    }

    #[test]
    fn working_has_no_symbol() {
        assert_eq!(State::Working.symbol(), "");
    }

    #[test]
    fn symbols_match_spec() {
        assert_eq!(State::Approval.symbol(), "!");
        assert_eq!(State::Done.symbol(), "\u{2713}");
        assert_eq!(State::Error.symbol(), "\u{2717}");
    }

    #[test]
    fn kv_round_trips() {
        let record = StateRecord {
            agent: "impl".to_string(),
            state: State::Approval,
            updated_at: "2026-08-19T12:00:00Z".to_string(),
            harness: "claude".to_string(),
            session: "myproject".to_string(),
            window: "impl!".to_string(),
        };
        let text = record.to_kv();
        assert_eq!(StateRecord::from_kv(&text), Some(record));
    }

    #[test]
    fn kv_missing_required_field_is_none() {
        let text = "agent=impl\nstate=working\n";
        assert_eq!(StateRecord::from_kv(text), None);
    }

    #[test]
    fn kv_unknown_state_is_none() {
        let text = "agent=impl\nstate=bogus\nupdated_at=t\nsession=s\nwindow=w\n";
        assert_eq!(StateRecord::from_kv(text), None);
    }

    #[test]
    fn kv_missing_harness_defaults_empty() {
        let text = "agent=impl\nstate=working\nupdated_at=t\nsession=s\nwindow=w\n";
        let record = StateRecord::from_kv(text).unwrap();
        assert_eq!(record.harness, "");
    }
}
