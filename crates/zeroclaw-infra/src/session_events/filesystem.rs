//! Plaintext session content uses private local filesystem objects. Do not
//! follow a symlink for event files; successful commits still require fsync.
use super::*;
use std::fs::{File, OpenOptions};
use std::io::Read;

/// Sync control-file publication using the same platform contract as config
/// publication. Windows cannot flush directory handles (ERROR_ACCESS_DENIED);
/// files must be synced before rename. JSONL commits use their stricter path.
pub(super) fn sync_directory(path: &Path) -> Result<(), AppendError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(jsonl::storage)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .map_err(jsonl::storage)?;
        match directory.sync_all() {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(5) => Ok(()),
            Err(error) => Err(jsonl::storage(error)),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(AppendError::StorageUnavailable(
            "session publication unsupported on this platform".into(),
        ))
    }
}

/// Read access never creates a missing object or follows a substituted link.
/// Validate the opened descriptor, so a FIFO cannot become a blocking reader.
pub(super) fn reader(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("session file is not a regular file"));
    }
    Ok(file)
}

pub(super) fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader(path)?.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn directory(path: &Path) -> Result<(), AppendError> {
    fs::create_dir_all(path).map_err(jsonl::storage)?;
    let metadata = fs::symlink_metadata(path).map_err(jsonl::storage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AppendError::StorageUnavailable(
            "session directory is not a real directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(jsonl::storage)?;
    }
    Ok(())
}
pub(super) fn file(path: &Path, exclusive: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("session file is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
