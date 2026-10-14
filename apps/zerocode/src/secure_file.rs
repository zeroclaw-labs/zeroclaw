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

/// Replace `path` with `bytes` in one step: stage a fresh sibling created
/// `0600`, fsync it, rename it over the target, then fsync the directory.
///
/// The rename is what makes a reader see either the whole previous file or the
/// whole new one, never a truncated middle. The published file is always the
/// freshly staged `0600` one; the directory is tightened to `0700` afterwards,
/// and the file is set to `0600` again in case an unusual umask narrowed it
/// further.
///
/// On platforms without Unix mode bits the permission work is a no-op and the
/// directory ACL is the guard, matching the enrollment path's stance.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staged = write_staged(path, bytes)?;
    if let Err(e) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(e).with_context(|| format!("publishing {}", path.display()));
    }
    restrict_to_owner(path, parent)?;
    sync_dir_where_supported(parent)
}

/// Create `path` holding `bytes`, owner-only and durable, only when nothing
/// exists there yet, and report whether this call created it.
///
/// The content is staged in a fresh sibling and published with a hard link,
/// which fails instead of replacing an entry that appeared in the meantime.
/// A default written by one caller therefore never overwrites a file another
/// caller has just saved, and no reader sees a partial file. On a filesystem
/// without hard links the staged file is renamed into place if the target is
/// still absent.
pub(crate) fn create_private_if_absent(path: &Path, bytes: &[u8]) -> Result<bool> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staged = write_staged(path, bytes)?;
    let created = match std::fs::hard_link(&staged, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&staged);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists || path.exists() => {
            let _ = std::fs::remove_file(&staged);
            false
        }
        Err(_) => {
            if let Err(e) = std::fs::rename(&staged, path) {
                let _ = std::fs::remove_file(&staged);
                return Err(e).with_context(|| format!("publishing {}", path.display()));
            }
            true
        }
    };
    if created {
        restrict_to_owner(path, parent)?;
        sync_dir_where_supported(parent)?;
    }
    Ok(created)
}

/// Stage `bytes` in a sibling of `path` that this call alone created, and
/// return its path.
///
/// Each candidate name is opened with `create_new`, which refuses any
/// existing entry, a symlink included. A link planted at a staging name
/// therefore cannot redirect the write, and concurrent writers never share or
/// truncate one staging file. A failed write removes its own staging file.
fn write_staged(path: &Path, bytes: &[u8]) -> Result<std::path::PathBuf> {
    let file_name = path.file_name().unwrap_or_default();
    let pid = std::process::id();
    for attempt in 0u32..64 {
        let mut name = std::ffi::OsString::from(".");
        name.push(file_name);
        name.push(format!(".{pid}.{attempt}.tmp"));
        let staged = path.with_file_name(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&staged) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| format!("creating {}", staged.display()));
            }
        };
        if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = std::fs::remove_file(&staged);
            return Err(e).with_context(|| format!("writing {}", staged.display()));
        }
        return Ok(staged);
    }
    anyhow::bail!("no free staging name next to {}", path.display())
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
    fn write_private_atomic_does_not_follow_a_planted_staging_link() {
        let tmp = tempfile::TempDir::new().unwrap();
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, "untouched\n").unwrap();
        let path = tmp.path().join("config.toml");
        let pid = std::process::id();
        for attempt in 0..4 {
            std::os::unix::fs::symlink(
                &victim,
                tmp.path().join(format!(".config.toml.{pid}.{attempt}.tmp")),
            )
            .unwrap();
        }
        std::os::unix::fs::symlink(&victim, tmp.path().join("config.toml.tmp")).unwrap();

        write_private_atomic(&path, b"fresh = true\n").expect("the write publishes");

        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh = true\n");
        assert!(
            !std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the published file must be the staged file, not a link"
        );
    }

    #[test]
    fn create_private_if_absent_never_replaces_an_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "saved = true\n").unwrap();

        let created = create_private_if_absent(&path, b"default = true\n").unwrap();

        assert!(!created, "an existing file must be reported, not replaced");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "saved = true\n");
        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["config.toml".to_string()], "{entries:?}");
    }

    #[test]
    fn create_private_if_absent_creates_a_missing_file_owner_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let created = create_private_if_absent(&path, b"default = true\n").unwrap();

        assert!(created);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "default = true\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a created file must be owner-only");
        }
        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["config.toml".to_string()], "{entries:?}");
    }

    #[test]
    fn concurrent_writers_each_publish_a_whole_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = std::sync::Arc::new(tmp.path().join("config.toml"));
        let payloads: Vec<String> = (0..8)
            .map(|writer| format!("writer = {writer}\n{}\n", "x".repeat(64 * 1024)))
            .collect();
        let handles: Vec<_> = payloads
            .iter()
            .cloned()
            .map(|payload| {
                let path = std::sync::Arc::clone(&path);
                std::thread::spawn(move || write_private_atomic(&path, payload.as_bytes()))
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .unwrap()
                .expect("every concurrent write publishes");
        }

        let published = std::fs::read_to_string(path.as_ref()).unwrap();
        assert!(
            payloads.contains(&published),
            "the published file must be one writer's whole payload"
        );
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
