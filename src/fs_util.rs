use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::process;

/// Sibling temp file + rename — atomic within a filesystem. The pid in the temp name
/// rules out collisions between concurrent writers without relying on `mktemp`.
///
/// Applies the destination's existing permissions to the temp file *before* writing
/// its contents: some managed files (e.g. `settings.json`) may be `0600`, and if the
/// content were written first under the process umask (typically `0644`), the file
/// would briefly hold the new contents at the looser mode until the chmod landed —
/// exposing them for that window, not just silently loosening the final mode.
pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("{}.tmp", process::id()));
    let mut file = File::create(&tmp)?;
    if let Ok(metadata) = fs::metadata(path) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(contents.as_bytes())?;
    drop(file);
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

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, "{\"k\":1}").unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
