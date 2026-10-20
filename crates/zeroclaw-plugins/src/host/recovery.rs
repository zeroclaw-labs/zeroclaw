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
/// How long `lock` waits for another holder of the package lock before it gives
/// up and names the lock file.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const LEASE: &str = "lease";
pub(super) const PACKAGE: &str = "package";
/// The mark a transaction holds once it is committed to deleting the
/// generation it claimed: a remove that judged the package incomplete, or an
/// update that published the replacement in its place. Recovery finishes the
/// delete of a marked claim and never puts it back.
pub(super) const DELETING: &str = "deleting";

pub(super) struct Root {
    pub dir: Dir,
    identity: same_file::Handle,
    lock: std::sync::OnceLock<File>,
    /// Held for as long as a [`Guard`] lives. The package lock is a lock on
    /// one open file, which a second lock through that same file takes at
    /// once, so two threads sharing this root would otherwise both hold it.
    held: std::sync::Mutex<()>,
    path: PathBuf,
    // cap-std's Windows operations reconstruct paths. Pin every canonical
    // ancestor without FILE_SHARE_DELETE, not just the package root, so those
    // reconstructions cannot be redirected by renaming an ancestor.
    #[cfg(windows)]
    ancestors: Vec<Dir>,
}

pub(super) struct Guard<'a> {
    file: &'a File,
    // Released after `drop` has unlocked the file, so the next thread in this
    // process takes the file lock only once this one has let it go.
    _thread: std::sync::MutexGuard<'a, ()>,
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        // Closing the retained descriptor also releases the lock. There is no
        // mutation in this destructor, and unlock errors cannot confer ownership.
        let _ = self.file.unlock();
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
            held: std::sync::Mutex::new(()),
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
        self.lock_within(LOCK_WAIT)
    }

    /// Take the package lock, waiting at most `wait` for another holder. Any
    /// account that can read the lock file can hold it, so a holder that never
    /// lets go makes this fail, naming the file, rather than block install and
    /// remove without end.
    fn lock_within(&self, wait: std::time::Duration) -> Result<Guard<'_>, PluginError> {
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
                    // An exclusive lock over NFS needs a descriptor open for
                    // writing. A lock file this user cannot write, such as one
                    // another user created, is opened for reading only, which
                    // a local filesystem locks just as well.
                    match self
                        .dir
                        .open_with(LOCK, OpenOptions::new().read(true).write(true))
                    {
                        Ok(file) => file,
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::PermissionDenied
                                    | std::io::ErrorKind::ReadOnlyFilesystem
                            ) =>
                        {
                            self.dir.open_with(LOCK, OpenOptions::new().read(true))?
                        }
                        Err(error) => return Err(error.into()),
                    }
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
        let started = std::time::Instant::now();
        let mut pause = std::time::Duration::from_millis(10);
        // Another thread of this process holding the lock through this root
        // is waited for first, within the same bound. A thread that panicked
        // while holding it unlocked the file as its guard dropped.
        let thread = loop {
            match self.held.try_lock() {
                Ok(thread) => break thread,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) if started.elapsed() < wait => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(std::time::Duration::from_millis(250));
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(PluginError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "another operation in this process did not release the package lock at {} within {wait:?}; try again once it finishes",
                            self.path.join(LOCK).display()
                        ),
                    )));
                }
            }
        };
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < wait => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(std::time::Duration::from_millis(250));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(PluginError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        format!(
                            "another process did not release the package lock at {} within {wait:?}; try again once it finishes",
                            self.path.join(LOCK).display()
                        ),
                    )));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        let guard = Guard {
            file: lock,
            _thread: thread,
        };
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
        #[cfg(test)]
        pause("transaction-entry");
        let dir = self.dir.open_dir(&entry)?;
        let lease = dir
            .open_with(
                LEASE,
                OpenOptions::new().read(true).write(true).create_new(true),
            )?
            .into_std();
        lease.lock()?;
        Ok(Transaction {
            entry,
            dir,
            lease: Lease(lease),
        })
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
            Ok(()) => Ok(Some(Transaction {
                entry,
                dir,
                lease: Lease(lease),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Remove a transaction entry no process holds, if it is empty: what a
    /// process stopped between creating the entry and its lease, or while
    /// finishing it, leaves. The removal is empty-only, so a live transaction,
    /// which holds its lease file, or a claimed package is never touched.
    pub fn remove_empty_entry(&self, entry: &str) -> Result<bool, PluginError> {
        self.check()?;
        Ok(self.dir.remove_dir(entry).is_ok())
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
    lease: Lease,
}

/// A transaction's held lease. Closing a locked file releases the lock only
/// once nothing refers to that open file any more, and a child process that
/// another thread is spawning refers to it until its exec closes it. A
/// recovery looking right after would take the transaction for a live one, so
/// the lease is unlocked before it is closed.
struct Lease(File);

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
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

    /// Commit to deleting the claimed generation. A process that takes this
    /// transaction over after a stop finds the mark and finishes the delete
    /// rather than putting the generation back.
    pub fn mark_deleting(&self) -> Result<(), PluginError> {
        self.dir
            .open_with(DELETING, OpenOptions::new().write(true).create_new(true))?;
        Ok(())
    }

    /// Whether an earlier remove or update committed to deleting this claim.
    /// The mark is a regular file this protocol created; anything else by that
    /// name is an error rather than a commitment.
    pub fn is_deleting(&self) -> Result<bool, PluginError> {
        match self.dir.symlink_metadata(DELETING) {
            Ok(metadata) if metadata.is_file() => Ok(true),
            Ok(_) => Err(std::io::Error::other("the delete mark is not a regular file").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// Finish a delete an earlier remove or update committed to: whatever is
    /// left of the claimed generation, through a handle on it, then the mark.
    pub fn finish_delete(&self) -> Result<(), PluginError> {
        match self.dir.open_dir(PACKAGE) {
            Ok(package) => {
                clear_owned(&package)?;
                drop(package);
                self.dir.remove_dir(PACKAGE)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.remove_mark()
    }

    pub fn remove_mark(&self) -> Result<(), PluginError> {
        match self.dir.remove_file(DELETING) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
            _ => Ok(()),
        }
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

/// Move a package without replacing any existing destination. Refuse when the
/// platform or filesystem cannot enforce that atomically: checking an entry's
/// type before a replacing rename does not pin the entry against substitution.
/// Only single entry names reach this helper.
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn rename_new(from: &Dir, name: &str, to: &Dir, dest: &str) -> std::io::Result<()> {
    use rustix::io::Errno;

    #[cfg(test)]
    pause("rename-before-move");
    let rename =
        || rustix::fs::renameat_with(from, name, to, dest, rustix::fs::RenameFlags::NOREPLACE);
    #[cfg(test)]
    let result = FORCE_RENAME_ERROR
        .with(std::cell::Cell::get)
        .map_or_else(rename, Err);
    #[cfg(not(test))]
    let result = rename();
    match result {
        Err(errno)
            if [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP].contains(&errno) =>
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!("atomic no-replace package move unavailable ({errno})"),
            ))
        }
        result => result.map_err(std::io::Error::from),
    }
}

/// No atomic no-replace primitive is wired for other Unix platforms.
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn rename_new(_: &Dir, _: &str, _: &Dir, _: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace package move unavailable",
    ))
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_vendor = "apple")
))]
thread_local! {
    /// Override the atomic primitive's result without changing its error path.
    pub(super) static FORCE_RENAME_ERROR: std::cell::Cell<Option<rustix::io::Errno>> =
        const { std::cell::Cell::new(None) };
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

#[cfg(not(any(unix, windows)))]
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
        #[cfg(test)]
        pause("entry-deleted");
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
        // killing this process: wait for the kill rather than run past the
        // barrier, and fail instead of hanging the test if no kill comes.
        if std::io::stdin().read_line(&mut line).is_err() || line.trim() != "continue" {
            std::thread::sleep(std::time::Duration::from_secs(30));
            std::process::exit(101);
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

#[cfg(all(test, unix))]
mod lease_release_tests {
    use super::*;

    /// Dropping a transaction releases its lease at once, even while its open
    /// file is still shared, here through a duplicate descriptor, as a child
    /// being spawned shares it until its exec.
    #[test]
    fn a_dropped_transaction_releases_its_lease_while_its_file_is_shared() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        let tx = root.transaction("race", "installing").unwrap();
        let entry = tx.entry.clone();
        let shared = tx.lease.0.try_clone().unwrap();
        drop(tx);
        assert!(root.reopen_transaction(entry).unwrap().is_some());
        drop(shared);
    }
}

#[cfg(test)]
mod lock_wait_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A lock another holder keeps makes the waiter give up after its wait,
    /// naming the lock file, instead of blocking without end. Once the holder
    /// lets go, the waiter takes it.
    #[test]
    fn a_held_package_lock_is_given_up_after_the_wait_and_named() {
        let temp = tempfile::tempdir().unwrap();
        let holder = Root::open(temp.path()).unwrap();
        let waiter = Root::open(temp.path()).unwrap();
        let held = holder.lock().unwrap();
        let started = Instant::now();
        let Err(error) = waiter.lock_within(Duration::from_millis(300)) else {
            panic!("a held package lock was taken twice");
        };
        assert!(started.elapsed() >= Duration::from_millis(300));
        let message = error.to_string();
        assert!(
            message.contains(LOCK) && message.contains("did not release"),
            "{message}"
        );
        drop(held);
        drop(waiter.lock_within(Duration::from_millis(300)).unwrap());
    }

    /// Two threads sharing one root do not both hold the package lock. The
    /// lock is a lock on one open file, which a second lock through that same
    /// file would take at once, so the second thread waits and then gives up.
    #[test]
    fn a_second_thread_sharing_the_root_waits_for_the_package_lock() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        let held = root.lock().unwrap();
        let second = std::thread::scope(|scope| {
            scope
                .spawn(|| root.lock_within(Duration::from_millis(300)).map(drop))
                .join()
                .unwrap()
        });
        let Err(error) = second else {
            panic!("a second thread took a package lock this root already holds");
        };
        assert!(error.to_string().contains("did not release"), "{error}");
        drop(held);
        drop(root.lock_within(Duration::from_millis(300)).unwrap());
    }
}

#[cfg(all(test, unix))]
mod lock_descriptor_tests {
    use super::*;

    /// An existing lock file this user can write is held through a writable
    /// descriptor, which an exclusive lock over NFS needs.
    #[test]
    fn a_writable_lock_file_is_locked_through_a_writable_descriptor() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(LOCK), b"").unwrap();
        let root = Root::open(temp.path()).unwrap();
        let _guard = root.lock().unwrap();
        let flags = rustix::fs::fcntl_getfl(root.lock.get().unwrap()).unwrap();
        assert!(flags.contains(rustix::fs::OFlags::RDWR), "{flags:?}");
    }
}

#[cfg(all(
    test,
    any(target_os = "linux", target_os = "android", target_vendor = "apple")
))]
mod rename_tests {
    use super::*;

    struct ForcedError(Option<rustix::io::Errno>);
    impl ForcedError {
        fn set(errno: rustix::io::Errno) -> Self {
            Self(FORCE_RENAME_ERROR.with(|error| error.replace(Some(errno))))
        }
    }
    impl Drop for ForcedError {
        fn drop(&mut self) {
            FORCE_RENAME_ERROR.with(|error| error.set(self.0));
        }
    }

    #[test]
    fn unsupported_moves_preserve_source_and_each_destination() {
        use rustix::io::Errno;
        for errno in [Errno::INVAL, Errno::NOSYS, Errno::NOTSUP, Errno::OPNOTSUPP] {
            let _forced = ForcedError::set(errno);
            for destination in ["free", "file", "empty", "full", "link"] {
                let temp = tempfile::tempdir().unwrap();
                let root = Root::open(temp.path()).unwrap();
                let dir = &root.dir;
                dir.create_dir("source").unwrap();
                dir.write("source/keep", b"source bytes").unwrap();
                let source_id = same_file::Handle::from_path(temp.path().join("source")).unwrap();
                match destination {
                    "free" => {}
                    "file" => dir.write(destination, b"destination bytes").unwrap(),
                    "empty" | "full" => {
                        dir.create_dir(destination).unwrap();
                        if destination == "full" {
                            dir.write("full/keep", b"destination bytes").unwrap();
                        }
                    }
                    "link" => {
                        std::os::unix::fs::symlink("source", temp.path().join(destination))
                            .unwrap();
                    }
                    _ => unreachable!(),
                }
                let destination_id = (destination != "free")
                    .then(|| same_file::Handle::from_path(temp.path().join(destination)).unwrap());
                let error = rename_new(dir, "source", dir, destination).unwrap_err();
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported,
                    "{errno:?}/{destination}: {error}"
                );
                assert_eq!(dir.read("source/keep").unwrap(), b"source bytes");
                assert_eq!(
                    same_file::Handle::from_path(temp.path().join("source")).unwrap(),
                    source_id
                );
                if let Some(destination_id) = destination_id {
                    assert_eq!(
                        same_file::Handle::from_path(temp.path().join(destination)).unwrap(),
                        destination_id
                    );
                }
                match destination {
                    "free" => assert!(!dir.exists(destination)),
                    "file" => assert_eq!(dir.read(destination).unwrap(), b"destination bytes"),
                    "empty" => assert!(
                        dir.open_dir(destination)
                            .unwrap()
                            .entries()
                            .unwrap()
                            .next()
                            .is_none()
                    ),
                    "full" => assert_eq!(dir.read("full/keep").unwrap(), b"destination bytes"),
                    "link" => assert_eq!(
                        std::fs::read_link(temp.path().join(destination)).unwrap(),
                        Path::new("source")
                    ),
                    _ => unreachable!(),
                }
            }
        }
    }

    #[test]
    fn ordinary_atomic_move_errors_keep_errno_and_bytes() {
        use rustix::io::Errno;
        for errno in [Errno::ACCESS, Errno::XDEV, Errno::NOENT] {
            let _forced = ForcedError::set(errno);
            let temp = tempfile::tempdir().unwrap();
            let root = Root::open(temp.path()).unwrap();
            root.dir.create_dir("source").unwrap();
            root.dir.write("source/keep", b"source bytes").unwrap();
            let error = rename_new(&root.dir, "source", &root.dir, "free").unwrap_err();
            assert_eq!(error.raw_os_error(), Some(errno.raw_os_error()));
            assert_eq!(root.dir.read("source/keep").unwrap(), b"source bytes");
            assert!(!root.dir.exists("free"));
        }
    }

    #[test]
    fn unsupported_claim_and_restore_retain_exact_directory_generations() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        root.dir.create_dir("source").unwrap();
        root.dir.write("source/keep", b"source bytes").unwrap();
        let source_id = same_file::Handle::from_path(temp.path().join("source")).unwrap();
        let tx = root.transaction("source", "recovering").unwrap();
        {
            let _forced = ForcedError::set(rustix::io::Errno::NOSYS);
            let claim_result = tx.claim(&root, "source");
            assert!(
                matches!(
                    &claim_result,
                    Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
                ),
                "claim result: {claim_result:?}"
            );
            assert_eq!(root.dir.read("source/keep").unwrap(), b"source bytes");
            assert_eq!(
                same_file::Handle::from_path(temp.path().join("source")).unwrap(),
                source_id
            );
            assert!(!tx.dir.exists(PACKAGE));
        }
        tx.claim(&root, "source").unwrap();
        let claimed = tx.dir.open_dir(PACKAGE).unwrap();
        assert_eq!(identity(&claimed).unwrap(), source_id);
        let _forced = ForcedError::set(rustix::io::Errno::NOSYS);
        assert!(matches!(
            tx.publish(&root, "source"),
            Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(!root.dir.exists("source"));
        assert_eq!(tx.dir.read("package/keep").unwrap(), b"source bytes");
        assert_eq!(identity(&claimed).unwrap(), source_id);
        root.dir.write("source", b"destination sentinel").unwrap();
        let destination_id = same_file::Handle::from_path(temp.path().join("source")).unwrap();
        assert!(matches!(
            tx.publish(&root, "source"),
            Err(PluginError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert_eq!(root.dir.read("source").unwrap(), b"destination sentinel");
        assert_eq!(
            same_file::Handle::from_path(temp.path().join("source")).unwrap(),
            destination_id
        );
        assert_eq!(tx.dir.read("package/keep").unwrap(), b"source bytes");
        assert_eq!(identity(&claimed).unwrap(), source_id);
    }

    #[test]
    fn atomic_claim_refuses_conflicts_then_restores_exact_generation() {
        let temp = tempfile::tempdir().unwrap();
        let root = Root::open(temp.path()).unwrap();
        root.dir.create_dir("source").unwrap();
        root.dir.write("source/keep", b"source bytes").unwrap();
        let source_id = same_file::Handle::from_path(temp.path().join("source")).unwrap();
        let tx = root.transaction("source", "recovering").unwrap();
        tx.claim(&root, "source").unwrap();
        assert!(!root.dir.exists("source"));
        let claimed = tx.dir.open_dir(PACKAGE).unwrap();
        assert_eq!(identity(&claimed).unwrap(), source_id);
        root.dir.create_dir("source").unwrap();
        let destination_id = same_file::Handle::from_path(temp.path().join("source")).unwrap();
        assert!(tx.publish(&root, "source").is_err());
        assert_eq!(
            same_file::Handle::from_path(temp.path().join("source")).unwrap(),
            destination_id
        );
        assert!(
            root.dir
                .open_dir("source")
                .unwrap()
                .entries()
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(tx.dir.read("package/keep").unwrap(), b"source bytes");
        assert_eq!(identity(&claimed).unwrap(), source_id);
        root.dir.remove_dir("source").unwrap();
        tx.publish(&root, "source").unwrap();
        assert_eq!(root.dir.read("source/keep").unwrap(), b"source bytes");
        assert_eq!(
            same_file::Handle::from_path(temp.path().join("source")).unwrap(),
            source_id
        );
        assert!(!tx.dir.exists(PACKAGE));
        drop(claimed);
        tx.finish(&root).unwrap();
    }
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
