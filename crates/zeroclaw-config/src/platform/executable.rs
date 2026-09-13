//! Safe host executable resolution for Unix launchers.
//!
//! A launcher resolved from `PATH` must not be looked up again after a child
//! process changes its working directory.  This module resolves and validates
//! the executable before command construction and returns the canonical path
//! that should be passed to `Command::new`.

use std::ffi::OsStr;
use std::io;
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};

/// Resolve a host executable using the process `PATH`.
#[cfg(unix)]
pub fn resolve_executable(program: &OsStr) -> io::Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    resolve_executable_with_path(program, std::env::split_paths(&path))
}

/// Resolve a host executable using an injected list of PATH entries.
///
/// Empty and relative entries are ignored deliberately: an empty entry means
/// the current directory, and a relative entry would be interpreted relative
/// to whatever directory the child eventually receives.  Callers therefore
/// cannot accidentally validate one launcher and execute a workspace shadow.
#[cfg(unix)]
pub fn resolve_executable_with_path<I, P>(program: &OsStr, path_entries: I) -> io::Result<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    if program.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "executable name must not be empty",
        ));
    }

    let requested = Path::new(program);
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        if !is_bare_name(requested) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "executable {program:?} must be a bare name or an absolute path; relative paths with components are not allowed"
                ),
            ));
        }

        let mut last_invalid = None;
        for entry in path_entries {
            let entry = entry.as_ref();
            if entry.as_os_str().is_empty() || !entry.is_absolute() {
                continue;
            }

            let candidate = entry.join(requested);
            match validate_and_canonicalize(program, candidate) {
                Ok(resolved) => return Ok(resolved),
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    last_invalid = Some(error);
                }
                Err(_) => {}
            }
        }

        if let Some(error) = last_invalid {
            return Err(io::Error::new(
                error.kind(),
                format!(
                    "executable {program:?} was not valid in the usable absolute PATH entries: {error}"
                ),
            ));
        }

        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "executable {program:?} was not found on PATH (only usable absolute entries are considered)"
            ),
        ));
    };

    validate_and_canonicalize(program, candidate)
}

#[cfg(unix)]
fn is_bare_name(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !path.as_os_str().to_string_lossy().contains('/')
}

#[cfg(unix)]
fn validate_and_canonicalize(program: &OsStr, candidate: PathBuf) -> io::Result<PathBuf> {
    let canonical = candidate.canonicalize().map_err(|error| {
        io::Error::new(
            error.kind(),
            if error.kind() == io::ErrorKind::NotFound {
                format!(
                    "executable {program:?} at {} does not exist",
                    candidate.display()
                )
            } else {
                format!(
                    "executable {program:?} could not be canonicalized at {}: {error}",
                    candidate.display()
                )
            },
        )
    })?;

    let metadata = std::fs::metadata(&canonical).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "executable {program:?} could not be inspected at {}: {error}",
                canonical.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "executable {program:?} resolved to {} which is not a regular file",
                canonical.display()
            ),
        ));
    }

    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "executable {program:?} resolved to {} which is not executable",
                canonical.display()
            ),
        ));
    }

    Ok(canonical)
}

/// Windows does not use the Unix resolver.  This keeps the module available
/// to cross-platform callers without changing Windows command semantics.
#[cfg(not(unix))]
pub fn resolve_executable(program: &OsStr) -> io::Result<PathBuf> {
    Ok(PathBuf::from(program))
}

#[cfg(not(unix))]
pub fn resolve_executable_with_path<I, P>(program: &OsStr, _path_entries: I) -> io::Result<PathBuf>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    resolve_executable(program)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(dir: &Path, name: &str, mode: u32) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    #[test]
    fn rejects_relative_path_and_ignores_relative_path_entries() {
        let dir = tempfile::tempdir().unwrap();
        let _ = executable(dir.path(), "tool", 0o755);

        assert!(resolve_executable_with_path(OsStr::new("./tool"), [dir.path()]).is_err());
        assert!(
            resolve_executable_with_path(
                OsStr::new("tool"),
                [PathBuf::new(), PathBuf::from("."), dir.path().to_path_buf()]
            )
            .is_ok()
        );
    }

    #[test]
    fn skips_non_executable_candidate_for_later_valid_path_entry() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let _invalid = executable(first.path(), "tool", 0o644);
        let valid = executable(second.path(), "tool", 0o755);

        assert_eq!(
            resolve_executable_with_path(OsStr::new("tool"), [first.path(), second.path()])
                .unwrap(),
            valid.canonicalize().unwrap()
        );
    }

    #[test]
    fn resolves_absolute_paths_and_canonicalizes_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = executable(dir.path(), "target", 0o755);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert_eq!(
            resolve_executable_with_path(link.as_os_str(), std::iter::empty::<&Path>()).unwrap(),
            target.canonicalize().unwrap()
        );
    }

    #[test]
    fn rejects_directories_and_non_executables() {
        let dir = tempfile::tempdir().unwrap();
        let non_executable = executable(dir.path(), "tool", 0o644);

        assert!(
            resolve_executable_with_path(dir.path().as_os_str(), std::iter::empty::<&Path>())
                .is_err()
        );
        assert!(
            resolve_executable_with_path(non_executable.as_os_str(), std::iter::empty::<&Path>())
                .is_err()
        );
    }

    #[test]
    fn preserves_resolution_error_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let non_executable = executable(dir.path(), "tool", 0o644);

        assert_eq!(
            resolve_executable_with_path(OsStr::new(""), std::iter::empty::<&Path>())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            resolve_executable_with_path(OsStr::new("./tool"), [dir.path()])
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert_eq!(
            resolve_executable_with_path(non_executable.as_os_str(), std::iter::empty::<&Path>())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            resolve_executable_with_path(OsStr::new("missing-tool"), [dir.path().to_path_buf()])
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
    }
}
