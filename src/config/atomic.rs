//! Atomic, 0600 file writes for credential-grade files.
//!
//! mkdir 0700 -> temp 0600 in the same dir -> rename -> re-chmod 0600. The
//! final chmod is deliberate and load-bearing: `rename(2)` can land the file
//! with the temp's mode under some filesystems / restrictive umasks, and we
//! never want a token file to be group/world-readable. We re-chmod on every
//! write, not just on create.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::error::CliError;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const SECRET_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;

/// Atomically write `contents` to `path` with 0600 perms (parent dir 0700).
pub fn write_atomic_0600(path: &Path, contents: &str) -> Result<(), CliError> {
    let dir = path
        .parent()
        .ok_or_else(|| CliError::Runtime(format!("path has no parent: {}", path.display())))?;
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        // Best-effort dir mode (don't fail the write if chmod on the dir errors).
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(DIR_MODE));
    }

    // Temp file in the SAME dir so the rename is atomic (same filesystem).
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| CliError::Runtime(format!("temp file: {e}")))?;
    #[cfg(unix)]
    {
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(SECRET_MODE))?;
    }
    tmp.write_all(contents.as_bytes())?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;

    // Atomic rename into place.
    tmp.persist(path)
        .map_err(|e| CliError::Runtime(format!("persist {}: {}", path.display(), e.error)))?;

    // rename can drop/alter mode on some FS — re-assert 0600.
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(SECRET_MODE))?;
    }
    Ok(())
}

/// Atomically write a pretty-printed JSON value with 0600 perms.
pub fn write_json_atomic_0600(path: &Path, value: &serde_json::Value) -> Result<(), CliError> {
    let mut s = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::Runtime(format!("serialize json: {e}")))?;
    s.push('\n');
    write_atomic_0600(path, &s)
}

/// Read a secret file, refusing it if it is group/world readable or writable.
/// Returns `None` when the file is absent. Returns `Err(Permission)` on bad
/// perms so a leaked token surfaces loudly instead of being silently trusted.
/// On non-unix the perm check is inert.
pub fn read_secret_file(path: &Path) -> Result<Option<String>, CliError> {
    if !path.exists() {
        return Ok(None);
    }
    #[cfg(unix)]
    {
        let meta = fs::metadata(path)?;
        let mode = meta.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(CliError::Permission(format!(
                "Refusing to read {}: file is accessible to group/other (mode {:o}). Run: chmod 600 {}",
                path.display(),
                mode & 0o777,
                path.display()
            )));
        }
    }
    let contents = fs::read_to_string(path)?;
    Ok(Some(contents))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_roundtrips_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("secret.json");
        write_atomic_0600(&p, "hello\n").unwrap();
        let got = read_secret_file(&p).unwrap();
        assert_eq!(got.as_deref(), Some("hello\n"));
        #[cfg(unix)]
        {
            let mode = fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn read_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.json");
        assert!(read_secret_file(&p).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn loose_perms_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("loose.json");
        fs::write(&p, "x").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
        let err = read_secret_file(&p).unwrap_err();
        assert!(matches!(err, CliError::Permission(_)));
    }
}
