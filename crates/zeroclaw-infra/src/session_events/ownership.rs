//! Exclusive physical execution ownership. Holding this lease proves that a
//! previous process and all of its escaped observation handles have released
//! the family. It never grants policy or changes the installation format.
use super::*;
use std::fs::File;

pub struct FamilyExecutionLease {
    directory: std::path::PathBuf,
    family: String,
    lock: File,
}
impl FamilyExecutionLease {
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
            family: family.into(),
            lock,
        }))
    }
    pub(super) fn matches(&self, data_dir: &Path, family: &str) -> bool {
        self.directory == data_dir.join("sessions") && self.family == family
    }
}
impl Drop for FamilyExecutionLease {
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
            Arc::new(FamilyExecutionLease::acquire(directory.path(), "fixture").expect("lease"));
        let observer = lease.clone();
        drop(lease);
        assert!(FamilyExecutionLease::acquire(directory.path(), "fixture").is_err());
        assert!(FamilyExecutionLease::acquire(directory.path(), "another").is_ok());
        drop(observer);
        assert!(FamilyExecutionLease::acquire(directory.path(), "fixture").is_ok());
    }
}
