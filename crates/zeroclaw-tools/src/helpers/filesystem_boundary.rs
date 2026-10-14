use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{ambient_authority, fs::Dir};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
pub(crate) enum FilesystemBoundaryError {
    Denied { key: &'static str, path: PathBuf },
    Io(io::Error),
}

impl FilesystemBoundaryError {
    pub(crate) fn is_denied(&self) -> bool {
        matches!(self, Self::Denied { .. })
    }

    pub(crate) fn localization(&self) -> Option<(&'static str, String)> {
        match self {
            Self::Denied { key, path } => Some((key, path.display().to_string())),
            Self::Io(_) => None,
        }
    }
}

impl std::fmt::Display for FilesystemBoundaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied { key, path } => write!(formatter, "{key}: {}", path.display()),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for FilesystemBoundaryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Denied { .. } => None,
            Self::Io(error) => Some(error),
        }
    }
}

impl From<io::Error> for FilesystemBoundaryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn classify_nofollow(error: io::Error, path: &Path) -> FilesystemBoundaryError {
    if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
        || error.kind() == io::ErrorKind::NotADirectory
    {
        FilesystemBoundaryError::Denied {
            key: "tool-filesystem-boundary-error-symlink",
            path: path.to_path_buf(),
        }
    } else {
        FilesystemBoundaryError::Io(error)
    }
}

/// Open an absolute canonical directory without following any component that
/// is replaced by a symlink while the path is being acquired.
pub(crate) fn open_absolute_dir_nofollow(path: &Path) -> Result<Dir, FilesystemBoundaryError> {
    if !path.is_absolute() {
        return Err(FilesystemBoundaryError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capability root must be absolute",
        )));
    }

    let mut anchor = PathBuf::new();
    let mut names: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => anchor.push(prefix.as_os_str()),
            Component::RootDir => anchor.push(component.as_os_str()),
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(FilesystemBoundaryError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "capability root must be canonical",
                )));
            }
        }
    }
    if anchor.as_os_str().is_empty() {
        return Err(FilesystemBoundaryError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "capability root has no filesystem anchor",
        )));
    }

    let mut current = Dir::open_ambient_dir(anchor, ambient_authority())?;
    for name in names {
        current = current
            .open_dir_nofollow(&name)
            .map_err(|error| classify_nofollow(error, Path::new(&name)))?;
    }
    Ok(current)
}

pub(crate) fn open_dir_nofollow(parent: &Dir, name: &Path) -> Result<Dir, FilesystemBoundaryError> {
    parent
        .open_dir_nofollow(name)
        .map_err(|error| classify_nofollow(error, name))
}

pub(crate) fn create_dir_path_nofollow(
    root: &Dir,
    relative: &Path,
) -> Result<Dir, FilesystemBoundaryError> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(FilesystemBoundaryError::Denied {
                key: "tool-filesystem-boundary-error-contained",
                path: relative.to_path_buf(),
            });
        };
        match current.symlink_metadata(name) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(FilesystemBoundaryError::Denied {
                    key: "tool-filesystem-boundary-error-symlink",
                    path: relative.to_path_buf(),
                });
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(FilesystemBoundaryError::Denied {
                    key: "tool-filesystem-boundary-error-not-directory",
                    path: relative.to_path_buf(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => current.create_dir(name)?,
            Err(error) => return Err(error.into()),
        }
        current = open_dir_nofollow(&current, Path::new(name))?;
    }
    Ok(current)
}

pub(crate) fn open_file_nofollow(
    parent: &Dir,
    name: &Path,
) -> Result<cap_std::fs::File, FilesystemBoundaryError> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    options.follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let file = parent
        .open_with(name, &options)
        .map_err(|error| classify_nofollow(error, name))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(FilesystemBoundaryError::Denied {
            key: "tool-filesystem-boundary-error-not-regular",
            path: name.to_path_buf(),
        });
    }
    Ok(file)
}

fn regular_destination_metadata(
    parent: &Dir,
    destination: &Path,
) -> Result<Option<cap_std::fs::Metadata>, FilesystemBoundaryError> {
    match parent.symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(_) => Err(FilesystemBoundaryError::Denied {
            key: "tool-filesystem-boundary-error-not-regular",
            path: destination.to_path_buf(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn copy_file_atomic(
    parent: &Dir,
    destination: &Path,
    input: &mut impl Read,
    source_permissions: Option<cap_std::fs::Permissions>,
) -> Result<(), FilesystemBoundaryError> {
    let destination_metadata = regular_destination_metadata(parent, destination)?;
    static TMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = TMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".zeroclaw-write-{}-{sequence}.tmp", std::process::id());
    let permissions =
        source_permissions.or_else(|| destination_metadata.map(|metadata| metadata.permissions()));
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    options.follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Foundation::GENERIC_WRITE;
        use windows_sys::Win32::Storage::FileSystem::DELETE;
        options.access_mode(GENERIC_WRITE | DELETE);
    }
    let mut output = parent.open_with(&temp_name, &options)?;
    if let Some(permissions) = permissions
        && let Err(error) = output.set_permissions(permissions)
    {
        drop(output);
        let _ = parent.remove_file(&temp_name);
        return Err(error.into());
    }
    let result = io::copy(input, &mut output)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all());
    if let Err(error) = result {
        drop(output);
        let _ = parent.remove_file(&temp_name);
        return Err(error.into());
    }
    // Recheck after copying: staging may take long enough for the leaf to change.
    // This does not make the subsequent rename conditional on inode identity.
    let publication = regular_destination_metadata(parent, destination).and_then(|_| {
        replace_open_file(parent, Path::new(&temp_name), destination, &output)
            .map_err(FilesystemBoundaryError::from)
    });
    if let Err(error) = publication {
        drop(output);
        let _ = parent.remove_file(&temp_name);
        return Err(error);
    }
    drop(output);
    Ok(())
}

#[cfg(not(windows))]
fn replace_open_file(
    parent: &Dir,
    source: &Path,
    destination: &Path,
    _source_file: &cap_std::fs::File,
) -> io::Result<()> {
    parent.rename(source, parent, destination)
}

#[cfg(windows)]
fn replace_open_file(
    parent: &Dir,
    _source: &Path,
    destination: &Path,
    source_file: &cap_std::fs::File,
) -> io::Result<()> {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut components = destination.components();
    let Some(Component::Normal(file_name)) = components.next() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement requires a child file name",
        ));
    };
    if components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic replacement requires a child file name",
        ));
    }

    let wide_name: Vec<u16> = file_name.encode_wide().collect();
    let byte_len = wide_name
        .len()
        .checked_mul(size_of::<u16>())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
    let name_end = offset_of!(FILE_RENAME_INFORMATION, FileName)
        .checked_add(byte_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
    let info_len = name_end.max(size_of::<FILE_RENAME_INFORMATION>());
    let word_len = info_len.div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; word_len];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut io_status = IO_STATUS_BLOCK::default();

    // SAFETY: `storage` is pointer-aligned and sized for the fixed header plus
    // the complete UTF-16 child name. Both handles and `io_status` remain live
    // for the call, and the kernel does not retain either pointer.
    let status = unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = u32::try_from(byte_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
        std::ptr::copy_nonoverlapping(
            wide_name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            wide_name.len(),
        );
        NtSetInformationFile(
            source_file.as_raw_handle(),
            &mut io_status,
            info.cast(),
            u32::try_from(info_len).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "file name is too long")
            })?,
            FileRenameInformation,
        )
    };
    if status < 0 {
        let error = unsafe { RtlNtStatusToDosError(status) };
        Err(io::Error::from_raw_os_error(error as i32))
    } else {
        Ok(())
    }
}

pub(crate) fn write_file_atomic(
    parent: &Dir,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), FilesystemBoundaryError> {
    copy_file_atomic(parent, destination, &mut io::Cursor::new(bytes), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_file_atomic_replaces_existing_file_through_open_parent() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("existing.txt");
        std::fs::write(&destination, b"old")?;
        let parent = Dir::open_ambient_dir(root.path(), ambient_authority())?;

        write_file_atomic(&parent, Path::new("existing.txt"), b"new")?;

        assert_eq!(std::fs::read(destination)?, b"new");
        let names = std::fs::read_dir(root.path())?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        assert_eq!(names, [OsString::from("existing.txt")]);
        Ok(())
    }

    #[test]
    fn write_file_atomic_creates_and_replaces_one_character_name() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("x");
        let parent = Dir::open_ambient_dir(root.path(), ambient_authority())?;

        write_file_atomic(&parent, Path::new("x"), b"first")?;
        assert_eq!(std::fs::read(&destination)?, b"first");

        write_file_atomic(&parent, Path::new("x"), b"second")?;
        assert_eq!(std::fs::read(destination)?, b"second");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_rejects_fifo_with_either_permission_source() -> anyhow::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        let root = tempfile::tempdir()?;
        let parent = Dir::open_ambient_dir(root.path(), ambient_authority())?;
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.path().join("pipe"))
                .status()?
                .success()
        );
        let permissions = parent.symlink_metadata("pipe")?.permissions();
        for source_permissions in [None, Some(permissions)] {
            let error = copy_file_atomic(
                &parent,
                Path::new("pipe"),
                &mut io::Cursor::new(b"replacement"),
                source_permissions,
            )
            .unwrap_err();
            assert!(error.is_denied());
            assert!(
                std::fs::symlink_metadata(root.path().join("pipe"))?
                    .file_type()
                    .is_fifo()
            );
            assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        }
        Ok(())
    }

    #[test]
    fn atomic_copy_propagates_read_error_without_replacing_destination() -> anyhow::Result<()> {
        struct FailedRead;
        impl Read for FailedRead {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fixture read failure",
                ))
            }
        }
        let root = tempfile::tempdir()?;
        let parent = Dir::open_ambient_dir(root.path(), ambient_authority())?;
        parent.write("output", b"old")?;
        let error =
            copy_file_atomic(&parent, Path::new("output"), &mut FailedRead, None).unwrap_err();
        assert!(matches!(error, FilesystemBoundaryError::Io(ref error)
            if error.kind() == io::ErrorKind::UnexpectedEof));
        assert_eq!(parent.read("output")?, b"old");
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn atomic_copy_rechecks_destination_and_cleans_staging_file() -> anyhow::Result<()> {
        use std::os::unix::fs::FileTypeExt;

        struct ChangeDestination<'a>(&'a Dir, &'a Path);
        impl Read for ChangeDestination<'_> {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                self.0.remove_file("output")?;
                assert!(
                    std::process::Command::new("mkfifo")
                        .arg(self.1.join("output"))
                        .status()?
                        .success()
                );
                Ok(0)
            }
        }

        let root = tempfile::tempdir()?;
        let parent = Dir::open_ambient_dir(root.path(), ambient_authority())?;
        parent.write("output", b"old")?;
        let error = copy_file_atomic(
            &parent,
            Path::new("output"),
            &mut ChangeDestination(&parent, root.path()),
            None,
        )
        .unwrap_err();
        assert!(error.is_denied());
        assert!(
            std::fs::symlink_metadata(root.path().join("output"))?
                .file_type()
                .is_fifo()
        );
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }
}
