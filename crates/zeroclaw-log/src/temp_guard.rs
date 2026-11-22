//! Scope guard for sibling temp files that are written and then renamed over
//! their destination.
//!
//! Any writer that creates a temp file next to its target and has fallible
//! steps before the final `rename` must own the temp file through this guard,
//! so an early return removes it instead of leaking it into the log directory.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Removes the temp file on drop unless [`TempFileCleanup::disarm`] was called.
///
/// Keep it alive until the rename, and let the file handle (or a writer that
/// owns it) go out of scope before the guard does, so the file is closed when
/// it is removed.
pub(crate) struct TempFileCleanup {
    path: PathBuf,
    armed: bool,
}

/// Create `path` exclusively (`create_new`, mode `0o600` on Unix) and return
/// the handle together with an armed guard. Nothing fallible runs between the
/// creation and the guard, so the file is never left without an owner.
///
/// `label` names the caller in the error context (`creating <label> temp ...`).
pub(crate) fn create_exclusive_temp(path: &Path, label: &str) -> Result<(File, TempFileCleanup)> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(path)
        .with_context(|| format!("creating {label} temp {}", path.display()))?;
    Ok((file, TempFileCleanup::new(path.to_path_buf())))
}

impl TempFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Call right after the rename succeeded: the file now is the destination.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_cleanup_removes_only_uncommitted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "pending").unwrap();
        {
            let _cleanup = TempFileCleanup::new(path.clone());
        }
        assert!(!path.exists(), "uncommitted temp file must be removed");

        fs::write(&path, "committed").unwrap();
        {
            let mut cleanup = TempFileCleanup::new(path.clone());
            cleanup.disarm();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "committed");
    }

    #[test]
    fn temp_creation_collision_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "owned elsewhere").unwrap();

        assert!(create_exclusive_temp(&path, "migration").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "owned elsewhere");
    }
}
