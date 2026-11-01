//! Exclusive physical execution ownership. Holding this lease proves that a
//! previous process and all of its escaped observation handles have released
//! the family. It never grants policy or changes the installation format.
use super::*;
use std::fs::File;

pub struct SessionExecutionLease {
    directory: std::path::PathBuf,
    session_id: String,
    lock: File,
}
impl SessionExecutionLease {
    pub fn acquire(data_dir: &Path, family: &str) -> Result<Self, AppendError> {
        Self::try_acquire(data_dir, family)?.ok_or_else(|| {
            AppendError::StorageUnavailable("session_family_execution_owner_busy".into())
        })
    }
    pub fn try_acquire(data_dir: &Path, family: &str) -> Result<Option<Self>, AppendError> {
        let directory = data_dir.join("sessions");
        filesystem::directory(&directory)?;
        let locks = directory.join("execution-owners");
        filesystem::directory(&locks)?;
        let name = format!("{}.lock", Digest::from_bytes(family.as_bytes()).as_str());
        let lock = filesystem::file(&locks.join(name), false).map_err(jsonl::storage)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
            Err(std::fs::TryLockError::Error(error)) => return Err(jsonl::storage(error)),
        }
        Ok(Some(Self {
            directory,
            session_id: family.into(),
            lock,
        }))
    }
    pub(super) fn matches(&self, data_dir: &Path, family: &str) -> bool {
        self.directory == data_dir.join("sessions") && self.session_id == family
    }
}
impl Drop for SessionExecutionLease {
    fn drop(&mut self) {
        let _ = self.lock.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_handles_keep_other_execution_owners_out() {
        let directory = tempfile::TempDir::new().expect("directory");
        let lease =
            Arc::new(SessionExecutionLease::acquire(directory.path(), "fixture").expect("lease"));
        let observer = lease.clone();
        drop(lease);
        assert!(SessionExecutionLease::acquire(directory.path(), "fixture").is_err());
        assert!(SessionExecutionLease::acquire(directory.path(), "another").is_ok());
        drop(observer);
        assert!(SessionExecutionLease::acquire(directory.path(), "fixture").is_ok());
    }
}
