use crate::fs_util::encode_path_component;
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
    pub window_id: Option<String>,
}

impl StateRecord {
    pub fn to_kv(&self) -> String {
        let mut kv = format!(
            "agent={}\nstate={}\nupdated_at={}\nharness={}\nsession={}\nwindow={}\n",
            self.agent, self.state, self.updated_at, self.harness, self.session, self.window,
        );
        if let Some(id) = &self.window_id {
            kv.push_str(&format!("window_id={id}\n"));
        }
        kv
    }

    pub fn from_kv(text: &str) -> Option<StateRecord> {
        let mut agent = None;
        let mut state = None;
        let mut updated_at = None;
        let mut harness = None;
        let mut session = None;
        let mut window = None;
        let mut window_id = None;

        for line in text.lines() {
            let (key, val) = line.split_once('=')?;
            match key {
                "agent" => agent = Some(val.to_string()),
                "state" => state = Some(parse_state(val)?),
                "updated_at" => updated_at = Some(val.to_string()),
                "harness" => harness = Some(val.to_string()),
                "session" => session = Some(val.to_string()),
                "window" => window = Some(val.to_string()),
                "window_id" => window_id = Some(val.to_string()),
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
            window_id,
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
    if let Some(xdg) = env::var_os("XDG_CACHE_HOME") {
        let path = PathBuf::from(xdg);

        if path.has_root() {
            return Ok(path.join("bergr"));
        }
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.has_root())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "neither XDG_CACHE_HOME nor an absolute HOME is set",
            )
        })?;

    Ok(home.join(".cache").join("bergr"))
}

pub fn state_path(session: &str, agent: &str) -> io::Result<PathBuf> {
    Ok(cache_root()?
        .join(encode_path_component(session))
        .join(format!("{}.state", encode_path_component(agent))))
}

pub fn read_record(path: &Path) -> Option<StateRecord> {
    let text = fs::read_to_string(path).ok()?;
    StateRecord::from_kv(&text)
}

/// Deletes a state file, treating it already being gone as success rather than an
/// error — both `event` (on `SessionEnd`) and `sync` prune state files, so either
/// one may find the other already got there first.
pub fn remove_state_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod io_tests {
    use super::*;

    #[test]
    fn remove_state_file_ignores_missing_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gone.state");
        assert!(remove_state_file(&path).is_ok());
    }

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
            window_id: Some("@1".to_string()),
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
            window_id: None,
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

    #[test]
    fn state_path_has_single_filename_component_for_agent_with_slash() {
        let dir = tempfile::tempdir().unwrap();
        let cache_root = dir.path().to_path_buf();
        let path = cache_root
            .join("session")
            .join(format!("{}.state", encode_path_component("feature/foo")));
        assert_eq!(path.parent().unwrap(), cache_root.join("session"));
        assert_eq!(path.file_name().unwrap(), "feature%2ffoo.state");
    }

    #[test]
    fn state_path_keeps_leading_slash_session_inside_cache_root() {
        let session = "/workspace/project";
        let path = state_path(session, "impl").unwrap();
        let cache_root = cache_root().unwrap();
        assert!(path.starts_with(&cache_root));
        assert_eq!(
            path,
            cache_root
                .join(encode_path_component(session))
                .join("impl.state")
        );
    }
}
