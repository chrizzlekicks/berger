use crate::init::legacy_amux_cache_root;
use crate::state::cache_root;
use std::fs;
use std::path::Path;

/// Clears berger's own cache root and the legacy amux cache tree, if present — the
/// latter so a migration cleans up after itself rather than leaving stale state
/// files around indefinitely.
pub fn run() {
    match cache_root() {
        Ok(root) => remove_if_present(&root),
        Err(e) => eprintln!("berger reset: {e}"),
    }

    remove_if_present(&legacy_amux_cache_root());
}

fn remove_if_present(path: &Path) {
    if path.exists() {
        if let Err(e) = fs::remove_dir_all(path) {
            eprintln!("berger reset: could not remove {}: {e}", path.display());
        } else {
            println!("berger reset: removed {}", path.display());
        }
    }
}
