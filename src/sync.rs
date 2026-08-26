use crate::reconcile::plan_renames;
use crate::state::{self, cache_root};
use crate::tmux;

/// Reconciles every window in a session against its state files — equivalent to one
/// tick of the amux prototype's polling watcher, run on demand instead of every 2s.
///
/// `session` is required rather than inferred, mirroring why the tmux keybinding
/// passes `#{session_name}` explicitly: `run-shell` does not execute inside a pane,
/// so `$TMUX_PANE` may not resolve there.
pub fn run(session: &str) {
    let dir = match cache_root() {
        Ok(root) => root.join(session),
        Err(e) => {
            eprintln!("bergr sync: {e}");
            std::process::exit(1);
        }
    };

    let mut records = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
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
        std::process::exit(1);
    };

    for rename in plan_renames(&records, &windows) {
        tmux::rename_window(session, &rename.index, &rename.new_name);
    }
}
