//! Physical binding of a legacy CLI state path to its native event family.
//! A temporary sidecar fences incomplete imports. After archival, the backup
//! itself fences legacy saves and events own the permanent family identity.
use super::*;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use zeroclaw_api::session_events::canonical_json;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliStateBinding {
    pub version: u32,
    pub data_dir: PathBuf,
    pub family: String,
}

/// Lock order: CLI path lease -> installation format -> physical store.
/// Legacy saves hold this same lease, closing the first-cutover write race.
pub struct CliStateGuard {
    path: PathBuf,
    lock: File,
    binding: Option<CliStateBinding>,
}
fn sibling(path: &Path, suffix: &str) -> Result<PathBuf, AppendError> {
    let name = path
        .file_name()
        .ok_or_else(|| AppendError::InvalidReference("CLI state path has no filename".into()))?;
    let mut name = OsString::from(name);
    name.push(suffix);
    Ok(path.with_file_name(name))
}
impl CliStateGuard {
    /// Fail immediately for a bound native path. If the pointer races this
    /// check, acquisition serializes with publication and checks again.
    pub fn acquire_legacy(path: &Path) -> Result<Self, AppendError> {
        for suffix in [".native-session", ".backup"] {
            match fs::symlink_metadata(sibling(path, suffix)?) {
                Ok(_) => {
                    return Err(AppendError::InvalidTransition(
                        "legacy CLI state writer is fenced by native family binding".into(),
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(jsonl::storage(error)),
            }
        }
        let guard = Self::acquire(path)?;
        guard.require_legacy()?;
        Ok(guard)
    }
    pub fn acquire(path: &Path) -> Result<Self, AppendError> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir().map_err(jsonl::storage)?.join(path)
        };
        let parent = path
            .parent()
            .ok_or_else(|| AppendError::InvalidReference("CLI state parent missing".into()))?;
        fs::create_dir_all(parent).map_err(jsonl::storage)?;
        let canonical_parent = fs::canonicalize(parent).map_err(jsonl::storage)?;
        let name = path
            .file_name()
            .ok_or_else(|| AppendError::InvalidReference("CLI state filename missing".into()))?;
        let path = canonical_parent.join(name);
        // Refuse an aliased state file rather than giving one object two leases.
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(AppendError::StorageUnavailable(
                    "CLI state is not a regular file".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(jsonl::storage(error)),
        }
        let lock =
            filesystem::file(&sibling(&path, ".session-lock")?, false).map_err(jsonl::storage)?;
        lock.lock().map_err(jsonl::storage)?;
        let pointer = sibling(&path, ".native-session")?;
        let binding = match filesystem::reader(&pointer) {
            Ok(file) => {
                let mut raw = Vec::new();
                file.take(16 * 1024 + 1)
                    .read_to_end(&mut raw)
                    .map_err(jsonl::storage)?;
                if raw.len() > 16 * 1024 {
                    return Err(AppendError::Corrupt(
                        "CLI state binding exceeds limit".into(),
                    ));
                }
                let binding: CliStateBinding = serde_json::from_slice(&raw)
                    .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                if binding.version != 1
                    || !binding.data_dir.is_absolute()
                    || binding.family.is_empty()
                    || binding.family.len() > 512
                {
                    return Err(AppendError::Corrupt(
                        "invalid CLI state family binding".into(),
                    ));
                }
                Some(binding)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(jsonl::storage(error)),
        };
        Ok(Self {
            path,
            lock,
            binding,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn binding(&self) -> Option<&CliStateBinding> {
        self.binding.as_ref()
    }
    pub fn has_backup(&self) -> Result<bool, AppendError> {
        match fs::symlink_metadata(sibling(&self.path, ".backup")?) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(jsonl::storage(error)),
        }
    }
    pub fn require_legacy(&self) -> Result<(), AppendError> {
        if self.binding.is_some() || self.has_backup()? {
            return Err(AppendError::InvalidTransition(
                "legacy CLI state writer is fenced by native family binding".into(),
            ));
        }
        Ok(())
    }
    pub fn bind(&mut self, binding: CliStateBinding) -> Result<(), AppendError> {
        if binding.version != 1
            || !binding.data_dir.is_absolute()
            || binding.family.is_empty()
            || binding.family.len() > 512
        {
            return Err(AppendError::InvalidReference(
                "invalid CLI state family binding".into(),
            ));
        }
        if let Some(existing) = &self.binding {
            if existing != &binding {
                return Err(AppendError::InvalidTransition(
                    "CLI state family binding cannot change".into(),
                ));
            }
            return Ok(());
        }
        if self.has_backup()? {
            self.binding = Some(binding);
            return Ok(());
        }
        let bytes = canonical_json(&binding)?;
        if bytes.len() > 16 * 1024 {
            return Err(AppendError::InvalidReference(
                "CLI state binding exceeds limit".into(),
            ));
        }
        let temporary = sibling(
            &self.path,
            &format!(".native-session-{}.tmp", EventId::new()),
        )?;
        let mut file = filesystem::file(&temporary, true).map_err(jsonl::storage)?;
        file.write_all(&bytes).map_err(jsonl::storage)?;
        file.sync_all().map_err(jsonl::storage)?;
        fs::rename(&temporary, sibling(&self.path, ".native-session")?)
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| AppendError::InvalidReference("CLI state parent missing".into()))?;
        jsonl::sync_directory(parent)
            .map_err(|error| AppendError::CommitOutcomeUnknown(error.to_string()))?;
        self.binding = Some(binding);
        Ok(())
    }

    /// The caller has verified this path's family in the current event store.
    /// Keep no permanent per-legacy-session routing record after archival.
    pub fn finish_archive(&self) -> Result<(), AppendError> {
        let backup = sibling(&self.path, ".backup")?;
        match filesystem::reader(&backup) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(jsonl::storage(error)),
        }
        match fs::remove_file(sibling(&self.path, ".native-session")?) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(jsonl::storage(error)),
        }
        jsonl::sync_directory(
            self.path
                .parent()
                .ok_or_else(|| AppendError::InvalidReference("CLI state parent missing".into()))?,
        )
    }
}
impl Drop for CliStateGuard {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archived_source_fences_legacy_saves_without_a_permanent_path_locator() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("state.json");
        fs::write(&path, b"historical source").expect("source");
        let mut lease = CliStateGuard::acquire(&path).expect("lease");
        let binding = CliStateBinding {
            version: 1,
            data_dir: directory.path().into(),
            family: "fixture".into(),
        };
        lease
            .bind(binding.clone())
            .expect("fence incomplete import");
        assert!(sibling(&path, ".native-session").expect("marker").exists());
        super::super::upgrade::archive_path(&path, "backup").expect("archive");
        lease.finish_archive().expect("remove temporary marker");
        assert!(!sibling(&path, ".native-session").expect("marker").exists());
        drop(lease);
        assert!(CliStateGuard::acquire_legacy(&path).is_err());
        let mut lease = CliStateGuard::acquire(&path).expect("native reopen");
        lease.bind(binding).expect("verified family binding");
        assert!(!sibling(&path, ".native-session").expect("marker").exists());
        assert!(lease.require_legacy().is_err());
    }

    #[test]
    fn legacy_admission_rechecks_a_backup_published_before_lease_acquisition() {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("state.json");
        let lease = CliStateGuard::acquire(&path).expect("lease without marker");
        fs::write(sibling(&path, ".backup").expect("backup"), b"source").expect("backup");
        assert!(
            lease.require_legacy().is_err(),
            "a stale marker check cannot reopen a legacy writer"
        );
    }
}
