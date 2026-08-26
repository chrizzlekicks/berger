use std::fs;
use std::io;
use std::path::Path;
use std::process;

/// Sibling temp file + rename — atomic within a filesystem. The pid in the temp name
/// rules out collisions between concurrent writers without relying on `mktemp`.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", process::id()));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session1").join("impl.state");
        write_atomic(&path, "agent=impl\n").unwrap();
        assert!(path.exists());
    }

    #[test]
    fn write_atomic_leaves_no_visible_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("impl.state");
        write_atomic(&path, "agent=impl\nstate=working\n").unwrap();
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("impl.state")]);
    }
}
