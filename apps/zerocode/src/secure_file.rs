//! Durable, owner-only file writes shared by the credential and
//! configuration paths.
//!
//! Both paths put user secrets on disk: the enrollment materials under
//! `tls/`, and the configuration file, which carries a hand-written
//! `[connection.wss] auth_token` when the operator chooses the persistent
//! path over `ZEROCLAW_AUTH_TOKEN`. They share one implementation so the
//! guarantees do not drift apart: created `0600` with no world-readable
//! window, fsynced before anything claims the content, and on the atomic
//! path published by rename so a crash cannot leave a truncated file.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Write `bytes` to `path` and fsync them, so the content survives power loss
/// before anything claims it. `private` writes `0600` on Unix, with no
/// world-readable window.
pub(crate) fn write_durable(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if private {
            options.mode(0o600);
        }
    }
    #[cfg(not(unix))]
    {
        // No mode bits here; the directory ACL is the guard.
        let _ = private;
    }
    let mut f = options
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("writing {}", path.display()))?;
    f.sync_all()
        .with_context(|| format!("syncing {}", path.display()))?;
    Ok(())
}

/// fsync a directory so the entries created, renamed, or removed inside it are
/// durable, not just the file contents. Unix opens the directory and syncs the
/// handle. No other platform offers a portable equivalent, so this is a
/// documented no-op there.
pub(crate) fn sync_dir_where_supported(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let handle = std::fs::File::open(dir)
            .with_context(|| format!("opening {} to sync it", dir.display()))?;
        handle
            .sync_all()
            .with_context(|| format!("syncing {}", dir.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Replace `path` with `bytes` in one step: stage a sibling created `0600`,
/// fsync it, rename it over the target, then fsync the directory.
///
/// The rename is what makes a reader see either the whole previous file or the
/// whole new one, never a truncated middle. The creation mode only covers a
/// file this call creates, so an existing file and its directory are repaired
/// afterwards: a config file that was already `0644` does not become owner-only
/// just because the next write is.
///
/// On platforms without Unix mode bits the permission work is a no-op and the
/// directory ACL is the guard, matching the enrollment path's stance.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staged = staging_path(path);
    // A staged file left by an interrupted earlier write is stale by
    // definition; the create-truncate below replaces it.
    write_durable(&staged, bytes, true)?;
    std::fs::rename(&staged, path).with_context(|| format!("publishing {}", path.display()))?;
    restrict_to_owner(path, parent)?;
    sync_dir_where_supported(parent)
}

/// Tighten an existing file to `0600` and its directory to `0700`.
pub(crate) fn restrict_to_owner(path: &Path, dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("restricting {}", path.display()))?;
        }
        if dir.exists() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .with_context(|| format!("restricting {}", dir.display()))?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, dir);
    }
    Ok(())
}

fn staging_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_private_atomic_leaves_no_staging_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        write_private_atomic(&path, b"first = 1\n").expect("the first write publishes");
        write_private_atomic(&path, b"second = 2\n").expect("the second write publishes");

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second = 2\n");
        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["config.toml".to_string()], "{entries:?}");
    }

    #[cfg(unix)]
    #[test]
    fn write_private_atomic_repairs_a_world_readable_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "stale = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();

        write_private_atomic(&path, b"fresh = true\n").expect("the write publishes");

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(tmp.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "the file must end up owner-only");
        assert_eq!(dir_mode, 0o700, "the directory must end up owner-only");
    }
}
