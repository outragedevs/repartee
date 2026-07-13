use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub fn create_dir_all(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    restrict_path(path, mode)
}

/// Write `contents` to `path` **atomically**: a reader (or the next start) sees
/// either the old file or the new one, never a half-written one.
///
/// `std::fs::write` truncates and then writes, so a crash or an ENOSPC partway
/// through leaves a truncated file behind. That is not hypothetical for the
/// thing this mostly writes: every field of `AppConfig` is `#[serde(default)]`,
/// so a truncated `config.toml` still parses — silently, into defaults — and the
/// next `save_config` persists the loss. Every server, alias and ignore, gone.
///
/// Temp file in the **same directory** (a rename across filesystems is not
/// atomic and would fail), fsync'd before the rename so the rename cannot land
/// ahead of the bytes, and the mode is applied to the temp file before it is
/// moved into place — the target is never briefly world-readable.
pub fn write_file(path: &Path, contents: impl AsRef<[u8]>, mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent, 0o700)?;
    }
    write_file_via(path, &temp_path_for(path), contents.as_ref(), mode)
}

/// The temp path [`write_file`] stages through. Same directory, and tagged with
/// the pid so two repartee processes writing the same file cannot collide on it.
fn temp_path_for(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}.tmp-{}", std::process::id()))
}

/// The body of [`write_file`], with the staging path injected so a test can
/// force the failure that the atomicity exists for.
fn write_file_via(path: &Path, temp: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    let write_temp = || -> io::Result<()> {
        let mut file = std::fs::File::create(temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        restrict_path(temp, mode)
    };
    if let Err(e) = write_temp() {
        // Never leave our staging file behind for a write that did not happen.
        let _ = std::fs::remove_file(temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(temp, path) {
        let _ = std::fs::remove_file(temp);
        return Err(e);
    }
    Ok(())
}

pub fn restrict_path(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms)?;
    }

    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_file_creates_parent_and_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("file.txt");

        write_file(&path, "hello", 0o600).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[cfg(unix)]
    #[test]
    fn create_dir_all_applies_requested_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private");

        create_dir_all(&path, 0o700).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn a_failed_write_leaves_the_original_file_intact() {
        // The point of the temp+rename. `std::fs::write` truncates first, so a
        // failure partway through left a truncated `config.toml` — which still
        // parses, because every field is `#[serde(default)]`, silently into
        // defaults. Staging elsewhere means a failure cannot touch the target.
        //
        // The failure is forced by pre-creating a *directory* where the staging
        // file wants to go: `File::create` cannot open it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "servers = 'all of them'").unwrap();
        let temp = dir.path().join("blocked");
        std::fs::create_dir(&temp).unwrap();

        let result = write_file_via(&path, &temp, b"new contents", 0o600);

        assert!(result.is_err(), "the staged write could not even start");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "servers = 'all of them'",
            "the target must be untouched by a write that failed"
        );
    }

    #[test]
    fn a_successful_write_replaces_the_target_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "old").unwrap();

        write_file(&path, "new", 0o600).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name != "config.toml")
            .collect();
        assert!(
            leftovers.is_empty(),
            "the staging file must not survive the write: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_file_applies_requested_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.txt");

        write_file(&path, "secret", 0o600).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
