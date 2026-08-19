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

    let records: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "state"))
            .filter_map(|e| state::read_record(&e.path()))
            .collect(),
        Err(_) => Vec::new(), // no state for this session yet — nothing to reconcile
    };

    let Some(windows) = tmux::list_windows(session) else {
        eprintln!("bergr sync: could not list windows for session '{session}'");
        std::process::exit(1);
    };

    for rename in plan_renames(&records, &windows) {
        tmux::rename_window(session, &rename.index, &rename.new_name);
    }
}
