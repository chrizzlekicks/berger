use crate::state::cache_root;
use std::fs;
use std::path::Path;

/// Clears berger's own cache root, if present.
pub fn run() {
    match cache_root() {
        Ok(root) => remove_if_present(&root),
        Err(e) => eprintln!("berger reset: {e}"),
    }
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
