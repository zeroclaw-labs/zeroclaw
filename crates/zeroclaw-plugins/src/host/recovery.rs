//! Filesystem ownership for package publication and recovery.
//!
//! The retained root is the authority, not its ambient spelling. A persistent
//! lock serializes cooperating hosts; a transaction owns one moved directory
//! generation and its OS lease until publication, restoration, or deletion.
use super::PluginError;
use cap_std::fs::{Dir, OpenOptions};
use std::fs::File;
use std::path::{Path, PathBuf};

const LOCK: &str = ".zeroclaw-package-lock-v1";
const LEASE: &str = "lease";
pub(super) const PACKAGE: &str = "package";
/// What a claimed generation is renamed to inside its transaction once a
/// replacement is published in its place. `publish` only ever moves
/// [`PACKAGE`], so a superseded generation can be deleted but never put back.
pub(super) const SUPERSEDED: &str = "superseded";

pub(super) struct Root {
    pub dir: Dir,
    identity: same_file::Handle,
    lock: std::sync::OnceLock<File>,
    path: PathBuf,
    // cap-std's Windows operations reconstruct paths. Pin every canonical
    // ancestor without FILE_SHARE_DELETE, not just the package root, so those
    // reconstructions cannot be redirected by renaming an ancestor.
    #[cfg(windows)]
    ancestors: Vec<Dir>,
}

pub(super) struct Guard<'a>(&'a File);
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        // Closing the retained descriptor also releases the lock. There is no
        // mutation in this destructor, and unlock errors cannot confer ownership.
        let _ = self.0.unlock();
    }
}

fn identity(dir: &Dir) -> std::io::Result<same_file::Handle> {
    same_file::Handle::from_file(dir.try_clone()?.into_std_file())
}

impl Root {
    pub fn open(path: &Path) -> Result<Self, PluginError> {
        #[cfg(windows)]
        let ancestors = pin_ancestors(path)?;
        let dir = Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let identity = identity(&dir)?;
        let root = Self {
            dir,
            identity,
            lock: std::sync::OnceLock::new(),
            path: path.to_path_buf(),
            #[cfg(windows)]
            ancestors,
        };
        root.check()?;
        Ok(root)
    }

    pub fn check(&self) -> Result<(), PluginError> {
        #[cfg(windows)]
        for ancestor in &self.ancestors {
            // Retained, non-delete-sharing capabilities are the actual locks.
            ancestor.dir_metadata()?;
        }
        if same_file::Handle::from_path(&self.path)? != self.identity {
            return Err(PluginError::NamespaceChanged(
                self.path.display().to_string(),
            ));
        }
        if let Some(lock) = self.lock.get()
            && (!self.dir.symlink_metadata(LOCK)?.is_file()
                || same_file::Handle::from_file(self.dir.open(LOCK)?.into_std())?
                    != same_file::Handle::from_file(lock.try_clone()?)?)
        {
            return Err(PluginError::NamespaceChanged(
                "package coordination lock replaced".into(),
            ));
        }
        Ok(())
    }

    pub fn lock(&self) -> Result<Guard<'_>, PluginError> {
        self.check()?;
        if self.lock.get().is_none() {
            let lock = match self.dir.open_with(
                LOCK,
                OpenOptions::new().read(true).write(true).create_new(true),
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !self.dir.symlink_metadata(LOCK)?.is_file() {
                        return Err(PluginError::NamespaceChanged(
                            "package lock is not a regular file".into(),
                        ));
                    }
                    self.dir
                        .open_with(LOCK, OpenOptions::new().read(true).write(true))?
                }
                Err(error) => return Err(error.into()),
            }
            .into_std();
            // Concurrent initialization retains the winner's handle; never a
            // second mutable authority. Hosts still coordinate through OS locks.
            let _ = self.lock.set(lock);
        }
        let lock = self
            .lock
            .get()
            .ok_or_else(|| std::io::Error::other("package lock unavailable"))?;
        lock.lock()?;
        let guard = Guard(lock);
        self.check()?;
        Ok(guard)
    }

    pub fn transaction(&self, name: &str, kind: &str) -> Result<Transaction, PluginError> {
        self.check()?;
        let mut random = [0_u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .map_err(|_| std::io::Error::other("package transaction randomness unavailable"))?;
        let suffix: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let entry = format!(".{name}.{kind}-v1-{suffix}");
        self.dir.create_dir(&entry)?;
        let dir = self.dir.open_dir(&entry)?;
        let lease = dir
            .open_with(
                LEASE,
                OpenOptions::new().read(true).write(true).create_new(true),
            )?
            .into_std();
        lease.lock()?;
        Ok(Transaction { entry, dir, lease })
    }

    pub fn reopen_transaction(&self, entry: String) -> Result<Option<Transaction>, PluginError> {
        if !self.dir.symlink_metadata(&entry)?.is_dir() {
            return Ok(None);
        }
        let dir = self.dir.open_dir(&entry)?;
        match dir.symlink_metadata(LEASE) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let lease = dir
            .open_with(LEASE, OpenOptions::new().read(true).write(true))?
            .into_std();
        match lease.try_lock() {
            Ok(()) => Ok(Some(Transaction { entry, dir, lease })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    pub fn retained_path(&self, tx: &Transaction) -> String {
        self.path
            .join(&tx.entry)
            .join(PACKAGE)
            .display()
            .to_string()
    }
}

pub(super) struct Transaction {
    pub entry: String,
    pub dir: Dir,
    lease: File,
}

impl Transaction {
    /// Claim by rename, then compare the generation actually moved. A changed
    /// occupant has no delete authority and must be restored/refused by caller.
    pub fn claim(&self, root: &Root, name: &str) -> Result<(), PluginError> {
        root.check()?;
        rename_new(&root.dir, name, &self.dir, PACKAGE)?;
        Ok(())
    }

    pub fn publish(&self, root: &Root, name: &str) -> Result<(), PluginError> {
        root.check()?;
        rename_new(&self.dir, PACKAGE, &root.dir, name)?;
        Ok(())
    }

    /// Mark the claimed generation superseded, inside this transaction.
    pub fn retire(&self, root: &Root) -> Result<(), PluginError> {
        root.check()?;
        rename_new(&self.dir, PACKAGE, &self.dir, SUPERSEDED)?;
        Ok(())
    }

    pub fn finish(self, root: &Root) -> Result<(), PluginError> {
        // Never recursively remove the transaction name: only empty-directory
        // removal is allowed after its owned payload has gone.
        drop(self.lease);
        self.dir.remove_file(LEASE)?;
        drop(self.dir);
        root.check()?;
        root.dir.remove_dir(self.entry)?;
        Ok(())
    }
}

/// Atomic no-clobber directory move. Only single entry names reach this helper.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_new(from: &Dir, name: &str, to: &Dir, dest: &str) -> std::io::Result<()> {
    rustix::fs::renameat_with(from, name, to, dest, rustix::fs::RenameFlags::NOREPLACE)?;
    Ok(())
}

#[cfg(windows)]
fn rename_new(from: &Dir, name: &str, to: &Dir, dest: &str) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::MoveFileW;
    use windows::core::PCWSTR;

    for entry in [name, dest] {
        if entry.is_empty() || matches!(entry, "." | "..") || entry.contains(['\\', '/', ':', '\0'])
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "package move requires one entry name",
            ));
        }
    }
    // Root retains EVERY canonical ancestor without FILE_SHARE_DELETE, and
    // both parent Dir capabilities stay open throughout path lookup and move.
    // Thus these handle-derived paths cannot be redirected by ancestor rename.
    let source = winx::file::get_file_path(&from.try_clone()?.into_std_file())?.join(name);
    let destination = winx::file::get_file_path(&to.try_clone()?.into_std_file())?.join(dest);
    let mut source: Vec<u16> = source.as_os_str().encode_wide().collect();
    let mut destination: Vec<u16> = destination.as_os_str().encode_wide().collect();
    if source.contains(&0) || destination.contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "package move path contains NUL",
        ));
    }
    source.push(0);
    destination.push(0);
    // SAFETY: owned UTF-16 buffers each have one terminating NUL, no interior
    // NUL, and live through this synchronous call. Neither pointer escapes.
    // MoveFileW refuses ANY existing destination; never use a replacing fallback.
    unsafe { MoveFileW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr())) }
        .map_err(std::io::Error::other)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
fn rename_new(_: &Dir, _: &str, _: &Dir, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-clobber directory move unavailable",
    ))
}

/// Delete contents through the held generation. Never recursively delete by
/// its original name; final unlink is empty-only, so a replacement with bytes
/// cannot be consumed by that final operation.
pub(super) fn clear_owned(dir: &Dir) -> Result<(), PluginError> {
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        if entry.file_type()?.is_dir() {
            let child = dir.open_dir(&name)?;
            clear_owned(&child)?;
            drop(child);
            dir.remove_dir(&name)?;
        } else {
            dir.remove_file(&name)?;
        }
    }
    Ok(())
}

pub(super) fn is_transaction(entry: &str, name: &str, kind: &str) -> bool {
    entry
        .strip_prefix(&format!(".{name}.{kind}-v1-"))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// The package a `kind` transaction entry belongs to, when `entry` is one.
pub(super) fn transaction_package<'a>(entry: &'a str, kind: &str) -> Option<&'a str> {
    let (package, _) = entry
        .strip_prefix('.')?
        .rsplit_once(&format!(".{kind}-v1-"))?;
    (crate::instance::validate_package_name(package).is_ok()
        && is_transaction(entry, package, kind))
    .then_some(package)
}

#[cfg(test)]
pub(super) fn pause(step: &str) {
    use std::io::Write;
    if std::env::var("ZC_RECOVERY_BARRIER")
        .is_ok_and(|steps| steps.split(',').any(|candidate| candidate == step))
    {
        println!("BARRIER:{step}");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        // Only an explicit "continue" moves on. A closed pipe means the test is
        // killing this process, and a thread woken by that must not run past
        // the barrier while the kill completes.
        if std::io::stdin().read_line(&mut line).is_err() || line.trim() != "continue" {
            loop {
                std::thread::park();
            }
        }
    }
}

#[cfg(windows)]
fn pin_ancestors(path: &Path) -> Result<Vec<Dir>, PluginError> {
    let canonical = std::fs::canonicalize(path)?;
    let mut chain: Vec<_> = canonical.ancestors().collect();
    chain.reverse();
    let mut held = Vec::new();
    for path in chain {
        held.push(Dir::open_ambient_dir(path, cap_std::ambient_authority())?);
    }
    if std::fs::canonicalize(path)? != canonical {
        return Err(PluginError::NamespaceChanged(path.display().to_string()));
    }
    Ok(held)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn move_refuses_existing_files_and_directories_without_clobbering() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        for destination in ["file", "empty-dir", "full-dir"] {
            for source_file in [true, false] {
                let source = format!("source-{destination}-{source_file}");
                let dest = format!("dest-{destination}-{source_file}");
                if source_file {
                    root.dir.write(&source, b"source bytes").unwrap();
                } else {
                    root.dir.create_dir(&source).unwrap();
                    root.dir
                        .write(Path::new(&source).join("keep"), b"source bytes")
                        .unwrap();
                }
                if destination == "file" {
                    root.dir.write(&dest, b"destination bytes").unwrap();
                } else {
                    root.dir.create_dir(&dest).unwrap();
                    if destination == "full-dir" {
                        root.dir
                            .write(Path::new(&dest).join("keep"), b"destination bytes")
                            .unwrap();
                    }
                }
                assert!(rename_new(&root.dir, &source, &root.dir, &dest).is_err());
                let source_path = if source_file {
                    PathBuf::from(&source)
                } else {
                    Path::new(&source).join("keep")
                };
                assert_eq!(root.dir.read(source_path).unwrap(), b"source bytes");
                if destination == "file" {
                    assert_eq!(root.dir.read(&dest).unwrap(), b"destination bytes");
                } else if destination == "full-dir" {
                    assert_eq!(
                        root.dir.read(Path::new(&dest).join("keep")).unwrap(),
                        b"destination bytes"
                    );
                } else {
                    assert!(
                        root.dir
                            .open_dir(&dest)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .next()
                            .is_none()
                    );
                }
                let fresh = format!("fresh-{destination}-{source_file}");
                rename_new(&root.dir, &source, &root.dir, &fresh).unwrap();
                assert!(!root.dir.exists(&source));
            }
        }
        for bad in ["../escape", "name:stream", "a\\b", "nul\0name"] {
            assert!(rename_new(&root.dir, bad, &root.dir, "dest").is_err());
        }
    }
}
