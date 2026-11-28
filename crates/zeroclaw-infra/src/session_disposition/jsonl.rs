//! Private filesystem protocol shared by JSONL readers, writers and migration.

use super::{
    SessionDispositionBackend, SessionDispositionEffect, SessionDispositionIdentity,
    SessionDispositionWitness,
};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

const LOCK_NAME: &str = ".session-disposition.lock";
// A literal NUL cannot begin a valid JSONL record. Reserve even a truncated
// prefix so a damaged tombstone can never be mistaken for an ordinary session.
const MARKER_VERSION: &[u8] = b"\0ZC-SESSION-DISPOSITION\x01";
const MAX_MARKER_BYTES: usize = 4096;

pub(crate) enum Entry {
    Missing,
    Transcript(File),
    Marker(SessionDispositionWitness),
}

pub(crate) fn classify(path: &Path) -> io::Result<Entry> {
    classify_open(path, false)
}

pub(crate) fn classify_append(path: &Path) -> io::Result<Entry> {
    classify_open(path, true)
}

fn classify_open(path: &Path, append: bool) -> io::Result<Entry> {
    classify_file(open_regular(path, append))
}

fn classify_file(opened: io::Result<File>) -> io::Result<Entry> {
    let mut file = match opened {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
        other => other?,
    };
    let mut first = [0];
    let len = file.read(&mut first)?;
    file.seek(SeekFrom::Start(0))?;
    if len == 0 || first[0] != MARKER_VERSION[0] {
        return Ok(Entry::Transcript(file));
    }
    check_private(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(Entry::Marker(decode(&bytes)?))
}

fn decode(bytes: &[u8]) -> io::Result<SessionDispositionWitness> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid JSONL disposition marker",
        )
    };
    let Some(mut remaining) = bytes.strip_prefix(MARKER_VERSION) else {
        return Err(invalid());
    };
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(invalid());
    }

    fn take_field<'a>(remaining: &mut &'a [u8]) -> Option<&'a str> {
        let length = u32::from_le_bytes(remaining.get(..4)?.try_into().ok()?) as usize;
        let (field, rest) = remaining.get(4..)?.split_at_checked(length)?;
        *remaining = rest;
        std::str::from_utf8(field)
            .ok()
            .filter(|field| !field.is_empty())
    }

    let operation_id = take_field(&mut remaining).ok_or_else(invalid)?;
    let backend_key = take_field(&mut remaining).ok_or_else(invalid)?;
    let instance_id = take_field(&mut remaining).ok_or_else(invalid)?;
    if !remaining.is_empty() || !valid_identity(instance_id) {
        return Err(invalid());
    }

    Ok(SessionDispositionWitness {
        operation_id: operation_id.to_owned(),
        backend_key: backend_key.to_owned(),
        identity: SessionDispositionIdentity {
            backend: SessionDispositionBackend::ChatJsonl,
            instance_id: instance_id.to_owned(),
        },
        effect: SessionDispositionEffect::Delete,
    })
}

#[cfg(unix)]
pub(crate) fn encode(witness: &SessionDispositionWitness) -> io::Result<Vec<u8>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid JSONL disposition request",
        )
    };
    if witness.effect != SessionDispositionEffect::Delete
        || witness.identity.backend != SessionDispositionBackend::ChatJsonl
        || !valid_identity(&witness.identity.instance_id)
    {
        return Err(invalid());
    }

    let mut encoded = MARKER_VERSION.to_vec();
    for field in [
        &witness.operation_id,
        &witness.backend_key,
        &witness.identity.instance_id,
    ] {
        if field.is_empty() || encoded.len() + 4 + field.len() > MAX_MARKER_BYTES {
            return Err(invalid());
        }
        encoded.extend_from_slice(&(field.len() as u32).to_le_bytes());
        encoded.extend_from_slice(field.as_bytes());
    }
    Ok(encoded)
}

fn valid_identity(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value)
}

/// The inode is never replaced or removed. Take this only after taking the
/// process-local directory mutex; readers of immutable opened transcripts
/// don't need either lock.
pub(crate) fn lock_directory(dir: &Path, create: bool) -> io::Result<File> {
    let metadata = fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(invalid_path("JSONL sessions path must be a real directory"));
    }
    let path = dir.join(LOCK_NAME);
    let lock = match open_lock_file(&path) {
        Ok(file) => file,
        Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
            match open_nofollow(&path, false, true) {
                Ok(file) => ensure_regular(file)?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_lock_file(&path)?
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    check_private(&lock)?;
    lock.lock()?;
    Ok(lock)
}

pub(crate) fn open_regular(path: &Path, append: bool) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(invalid_path("session path must be a regular file"));
    }
    ensure_regular(open_nofollow(path, append, false)?)
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let file = open_regular(path, false)?;
    #[cfg(windows)]
    {
        // Windows requires write access for an exclusive lock.
        drop(file);
        open_regular_for_sync(path)
    }
    #[cfg(not(windows))]
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn open_regular_for_sync(path: &Path) -> io::Result<File> {
    ensure_regular(nofollow_options().read(true).write(true).open(path)?)
}

pub(crate) fn create_append(path: &Path) -> io::Result<File> {
    open_nofollow(path, true, true)
}

#[cfg(not(unix))]
fn nofollow_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn open_nofollow(path: &Path, append: bool, create_new: bool) -> io::Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{self, Mode, OFlags};
        let mut flags = match (append, create_new) {
            (true, true) => OFlags::WRONLY | OFlags::APPEND,
            (true, false) => OFlags::RDWR | OFlags::APPEND,
            (false, true) => OFlags::RDWR,
            (false, false) => OFlags::RDONLY,
        };
        if create_new {
            flags |= OFlags::CREATE | OFlags::EXCL;
        }
        // Do not follow a symlink exchanged between lstat and open, and do
        // not hang if a special file appears before the same-handle check.
        flags |= OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        Ok(fs::open(path, flags, Mode::RUSR | Mode::WUSR)?.into())
    }
    #[cfg(not(unix))]
    nofollow_options()
        .read(!append || !create_new)
        .write(create_new && !append)
        .append(append)
        .create_new(create_new)
        .open(path)
}

fn ensure_regular(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(invalid_path("session path must be a regular file"));
    }
    Ok(file)
}

fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn check_private(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if file.metadata()?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL disposition file must be private",
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

fn invalid_path(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(unix)]
pub(crate) use anchored::{Directory, Staged};

// Unix advisory files/atomic rename and checked directory fsync make this
// protocol possible. Witnessed writes use only leaf names relative to the
// exact directory that owns the already-held OS lock.
#[cfg(unix)]
mod anchored {
    use super::{
        Entry, LOCK_NAME, SessionDispositionBackend, SessionDispositionIdentity, check_private,
        classify_file, ensure_regular, invalid_path, valid_identity,
    };
    use rustix::fs::{self, AtFlags, Mode, OFlags};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    const IDENTITY_NAME: &str = ".session-disposition.identity";

    pub(crate) struct Directory(File);

    impl Directory {
        pub(crate) fn for_lock(path: &Path, lock: &File) -> io::Result<Self> {
            let directory = Self(
                fs::open(
                    path,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into(),
            );
            let held = lock.metadata()?;
            let actual = directory.open_leaf(LOCK_NAME)?.metadata()?;
            if (held.dev(), held.ino()) != (actual.dev(), actual.ino()) {
                return Err(invalid_path(
                    "JSONL session directory changed while locking",
                ));
            }
            Ok(directory)
        }

        fn open_leaf(&self, name: &str) -> io::Result<File> {
            ensure_regular(
                fs::openat(
                    &self.0,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )?
                .into(),
            )
        }

        pub(crate) fn classify(&self, name: &str) -> io::Result<Entry> {
            classify_file(self.open_leaf(name))
        }

        pub(crate) fn sync(&self) -> io::Result<()> {
            self.0.sync_all()
        }

        pub(crate) fn remove(&self, name: &str) -> io::Result<()> {
            fs::unlinkat(&self.0, name, AtFlags::empty())?;
            Ok(())
        }

        pub(crate) fn read_identity(&self) -> io::Result<Option<SessionDispositionIdentity>> {
            let file = match self.open_leaf(IDENTITY_NAME) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                other => other?,
            };
            check_private(&file)?;
            let mut instance_id = String::new();
            file.take(37).read_to_string(&mut instance_id)?;
            if !valid_identity(&instance_id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid JSONL backend instance identity",
                ));
            }
            Ok(Some(SessionDispositionIdentity {
                backend: SessionDispositionBackend::ChatJsonl,
                instance_id,
            }))
        }

        /// Atomically install without replacing an existing identity.
        pub(crate) fn install_identity(&self) -> io::Result<SessionDispositionIdentity> {
            if let Some(identity) = self.read_identity()? {
                // An earlier installation may have failed at the directory sync.
                self.sync()?;
                return Ok(identity);
            }
            let instance_id = uuid::Uuid::new_v4().hyphenated().to_string();
            let staged = self.stage(instance_id.as_bytes())?;
            fs::linkat(
                &self.0,
                staged.name.as_str(),
                &self.0,
                IDENTITY_NAME,
                AtFlags::empty(),
            )?;
            drop(staged);
            self.sync()?;
            Ok(SessionDispositionIdentity {
                backend: SessionDispositionBackend::ChatJsonl,
                instance_id,
            })
        }

        pub(crate) fn stage(&self, bytes: &[u8]) -> io::Result<Staged<'_>> {
            let name = format!(".session-disposition-{}", uuid::Uuid::new_v4());
            let mut file = File::from(fs::openat(
                &self.0,
                name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )?);
            let staged = Staged {
                dir: self,
                name,
                moved: false,
            };
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(staged)
        }
    }

    pub(crate) struct Staged<'a> {
        dir: &'a Directory,
        name: String,
        moved: bool,
    }

    impl Staged<'_> {
        pub(crate) fn replace(mut self, target: &str) -> io::Result<()> {
            fs::renameat(&self.dir.0, self.name.as_str(), &self.dir.0, target)?;
            self.moved = true;
            Ok(())
        }
    }

    impl Drop for Staged<'_> {
        fn drop(&mut self) {
            if !self.moved {
                let _ = self.dir.remove(&self.name);
            }
        }
    }
}
