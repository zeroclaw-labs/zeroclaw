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

pub(crate) fn copy_file_atomic(
    parent: &Dir,
    destination: &Path,
    input: &mut impl Read,
    source_permissions: Option<cap_std::fs::Permissions>,
) -> io::Result<()> {
    static TMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = TMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp_name = format!(".zeroclaw-write-{}-{sequence}.tmp", std::process::id());
    let permissions = match source_permissions {
        Some(permissions) => Some(permissions),
        None => match parent.symlink_metadata(destination) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        },
    };
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
        return Err(error);
    }
    let result = io::copy(input, &mut output)
        .and_then(|_| output.flush())
        .and_then(|_| output.sync_all());
    if let Err(error) = result {
        drop(output);
        let _ = parent.remove_file(&temp_name);
        return Err(error);
    }
    if let Err(error) = replace_open_file(parent, Path::new(&temp_name), destination, &output) {
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
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
    };

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
    let info_len = offset_of!(FILE_RENAME_INFO, FileName)
        .checked_add(byte_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
    let word_len = info_len.div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; word_len];
    let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();

    // SAFETY: `storage` is pointer-aligned and sized for the fixed header plus
    // the complete UTF-16 child name. Both handles remain live for the call.
    let succeeded = unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = u32::try_from(byte_len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file name is too long"))?;
        std::ptr::copy_nonoverlapping(
            wide_name.as_ptr(),
            std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
            wide_name.len(),
        );
        SetFileInformationByHandle(
            source_file.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            u32::try_from(info_len).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "file name is too long")
            })?,
        )
    };
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn write_file_atomic(parent: &Dir, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    copy_file_atomic(parent, destination, &mut io::Cursor::new(bytes), None)
}
