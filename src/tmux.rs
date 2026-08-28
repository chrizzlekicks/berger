use std::process::Command;

/// A single tmux window: `{window_id}:{window_index}:{window_name}`, as returned by
/// `tmux list-windows -F '#{window_id}:#{window_index}:#{window_name}'`.
///
/// `id` (tmux's `@N` form) is stable for the window's lifetime, unlike `index`, which
/// shifts when windows are moved or the session is renumbered, and unlike `name`,
/// which changes on every rename — the only field that can tell "renamed" apart from
/// "closed".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub id: String,
    pub index: String,
    pub name: String,
}

fn strip_line_ending(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

pub fn current_session() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = strip_line_ending(String::from_utf8(out.stdout).ok()?);
    if name.is_empty() { None } else { Some(name) }
}

pub fn current_window_id() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "#{window_id}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let id = strip_line_ending(String::from_utf8(out.stdout).ok()?);
    if id.is_empty() { None } else { Some(id) }
}

/// The tmux server's own PID, stable for the server's lifetime and distinct
/// across restarts — unlike `window_id` (`@N`), which a fresh server hands out
/// starting from `@0` again, so the same id can mean a different window after a
/// reboot or `tmux kill-server`.
pub fn server_pid() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "#{pid}"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let pid = strip_line_ending(String::from_utf8(out.stdout).ok()?);
    if pid.is_empty() { None } else { Some(pid) }
}

fn list_windows_args(session: &str) -> Vec<String> {
    vec![
        "list-windows".to_string(),
        "-t".to_string(),
        session.to_string(),
        "-F".to_string(),
        "#{window_id}:#{window_index}:#{window_name}".to_string(),
    ]
}

fn rename_window_args(session: &str, index: &str, new_name: &str) -> Vec<String> {
    vec![
        "rename-window".to_string(),
        "-t".to_string(),
        format!("{session}:{index}"),
        new_name.to_string(),
    ]
}

/// `window_id` (tmux's `@N` form) is unique across the whole server, so it needs
/// no session qualifier — unlike `index`, which is only unique within a session
/// and can shift when windows move.
fn rename_window_by_id_args(window_id: &str, new_name: &str) -> Vec<String> {
    vec![
        "rename-window".to_string(),
        "-t".to_string(),
        window_id.to_string(),
        new_name.to_string(),
    ]
}

fn parse_window_list(text: &str) -> Vec<Window> {
    let mut windows = Vec::new();
    for line in text.lines() {
        let Some((id, rest)) = line.split_once(':') else {
            continue;
        };
        let Some((index, name)) = rest.split_once(':') else {
            continue;
        };
        windows.push(Window {
            id: id.to_string(),
            index: index.to_string(),
            name: name.to_string(),
        });
    }
    windows
}

pub fn list_windows(session: &str) -> Option<Vec<Window>> {
    let out = Command::new("tmux")
        .args(list_windows_args(session))
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    Some(parse_window_list(&text))
}

pub fn rename_window(session: &str, index: &str, new_name: &str) -> bool {
    Command::new("tmux")
        .args(rename_window_args(session, index, new_name))
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Renames by `window_id` rather than `(session, index)` — the caller already
/// has the id from an earlier lookup, so this avoids a second, separately-timed
/// tmux call that could target a different window if the active window changed
/// in between (see `event::rename_current_window`).
pub fn rename_window_by_id(window_id: &str, new_name: &str) -> bool {
    Command::new("tmux")
        .args(rename_window_by_id_args(window_id, new_name))
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_window_list_output() {
        let text = "@0:0:main\n@1:1:impl!\n@2:2:plan\n";
        assert_eq!(
            parse_window_list(text),
            vec![
                Window {
                    id: "@0".to_string(),
                    index: "0".to_string(),
                    name: "main".to_string()
                },
                Window {
                    id: "@1".to_string(),
                    index: "1".to_string(),
                    name: "impl!".to_string()
                },
                Window {
                    id: "@2".to_string(),
                    index: "2".to_string(),
                    name: "plan".to_string()
                },
            ]
        );
    }

    #[test]
    fn parses_empty_window_list() {
        assert!(parse_window_list("").is_empty());
    }

    #[test]
    fn window_name_containing_colon_is_kept_intact() {
        // id and index are colon-free, so splitting on the first two colons only
        // must leave any further colons in the name untouched.
        let windows = parse_window_list("@1:1:foo:bar");
        assert_eq!(windows[0].name, "foo:bar");
    }

    #[test]
    fn list_windows_args_targets_the_given_session() {
        assert_eq!(
            list_windows_args("myproject"),
            vec![
                "list-windows",
                "-t",
                "myproject",
                "-F",
                "#{window_id}:#{window_index}:#{window_name}"
            ]
        );
    }

    #[test]
    fn rename_window_args_targets_session_and_index() {
        assert_eq!(
            rename_window_args("myproject", "1", "impl!"),
            vec!["rename-window", "-t", "myproject:1", "impl!"]
        );
    }

    #[test]
    fn rename_window_by_id_args_targets_the_id_alone() {
        assert_eq!(
            rename_window_by_id_args("@3", "impl!"),
            vec!["rename-window", "-t", "@3", "impl!"]
        );
    }
}
