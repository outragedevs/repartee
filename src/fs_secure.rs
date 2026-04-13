use std::io;
use std::path::Path;

pub fn create_dir_all(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    restrict_path(path, mode)
}

pub fn write_file(path: &Path, contents: impl AsRef<[u8]>, mode: u32) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all(parent, 0o700)?;
    }
    std::fs::write(path, contents)?;
    restrict_path(path, mode)
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
