use std::process::Command;

/// A single tmux window: `{window_index}:{window_name}`, as returned by
/// `tmux list-windows -F '#{window_index}:#{window_name}'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    pub index: String,
    pub name: String,
}

pub fn current_session() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "#S"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

pub fn current_window_name() -> Option<String> {
    let out = Command::new("tmux")
        .args(["display-message", "-p", "#W"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn list_windows_args(session: &str) -> Vec<String> {
    vec![
        "list-windows".to_string(),
        "-t".to_string(),
        session.to_string(),
        "-F".to_string(),
        "#{window_index}:#{window_name}".to_string(),
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

fn parse_window_list(text: &str) -> Vec<Window> {
    text.lines()
        .filter_map(|line| {
            let (index, name) = line.split_once(':')?;
            Some(Window {
                index: index.to_string(),
                name: name.to_string(),
            })
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_window_list_output() {
        let text = "0:main\n1:impl!\n2:plan\n";
        assert_eq!(
            parse_window_list(text),
            vec![
                Window {
                    index: "0".to_string(),
                    name: "main".to_string()
                },
                Window {
                    index: "1".to_string(),
                    name: "impl!".to_string()
                },
                Window {
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
    fn list_windows_args_targets_the_given_session() {
        assert_eq!(
            list_windows_args("myproject"),
            vec![
                "list-windows",
                "-t",
                "myproject",
                "-F",
                "#{window_index}:#{window_name}"
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
}
