//! Shared host-side plumbing for model providers backed by a CLI agent that
//! speaks the Agent Client Protocol (ACP) over stdio.
//!
//! A provider built on this module owns its argv policy, environment
//! passthrough rules, and authentication method selection. This module owns
//! what is common to every such provider:
//!
//! - [`resolve_binary_path`]: absolute-path or bare-name lookup restricted to
//!   absolute `PATH` directories, so a workspace-relative decoy is never run;
//! - [`validate_working_directory`] / [`validate_acp_stdout_limit`]: the
//!   explicit, canonical ACP session boundary and transport budget;
//! - [`env`]: the process-runtime environment allowlist;
//! - [`process`]: spawning, bounded stderr draining, process-tree containment,
//!   and teardown on every exit path;
//! - [`protocol`]: the bounded one-shot JSON-RPC exchange.
//!
//! Public errors never include child stderr, raw ACP frames, or other
//! child-controlled free text.

pub(crate) mod env;
pub(crate) mod process;
pub(crate) mod protocol;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(not(windows))]
fn executable_candidates(path: &Path, _path_ext_env: Option<&OsStr>) -> Vec<PathBuf> {
    vec![path.to_path_buf()]
}

#[cfg(windows)]
fn executable_candidates(path: &Path, path_ext_env: Option<&OsStr>) -> Vec<PathBuf> {
    let mut candidates = vec![path.to_path_buf()];
    if path.extension().is_some() {
        return candidates;
    }
    let Some(path_extensions) = path_ext_env.and_then(OsStr::to_str) else {
        return candidates;
    };
    for extension in path_extensions.split(';').filter(|extension| {
        extension.starts_with('.')
            && extension.len() > 1
            && extension[1..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
    }) {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(extension);
        candidates.push(PathBuf::from(candidate));
    }
    candidates
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !path.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `path` is a live NUL-terminated CString and `access` does not retain it.
    unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetBinaryTypeW;

    if !path.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    if path
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
        })
    {
        return true;
    }
    let wide_path: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut binary_type = 0_u32;
    // SAFETY: `wide_path` is NUL-terminated and `binary_type` is valid for this call.
    unsafe { GetBinaryTypeW(wide_path.as_ptr(), &mut binary_type) != 0 }
}

#[cfg(all(not(unix), not(windows)))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// First canonical, executable candidate for `path` (honouring `PATHEXT` on
/// Windows).
pub(crate) fn first_executable_candidate(
    path: &Path,
    path_ext_env: Option<&OsStr>,
) -> Option<PathBuf> {
    executable_candidates(path, path_ext_env)
        .into_iter()
        .find_map(|candidate| {
            let canonical = std::fs::canonicalize(candidate).ok()?;
            is_executable_file(&canonical).then_some(canonical)
        })
}

/// Resolve the CLI binary for provider family `family`.
///
/// `configured` (or `default_binary` when unset) must be an absolute path or
/// a bare executable name. Bare names resolve only from absolute `PATH`
/// directories; empty and relative entries are ignored.
pub(crate) fn resolve_binary_path(
    family: &str,
    default_binary: &str,
    configured: Option<&str>,
    path_env: Option<&OsStr>,
    path_ext_env: Option<&OsStr>,
) -> anyhow::Result<PathBuf> {
    let binary = configured.unwrap_or(default_binary);
    let path = Path::new(binary);
    if path.is_absolute() {
        first_executable_candidate(path, path_ext_env)
    } else {
        let mut components = path.components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            anyhow::bail!(
                "{family} binary_path must be an absolute path or a bare executable name"
            );
        }

        let safe_directories: Vec<PathBuf> = path_env
            .into_iter()
            .flat_map(std::env::split_paths)
            .filter(|directory| directory.is_absolute())
            .collect();
        if safe_directories.is_empty() {
            anyhow::bail!("{family} PATH contains no absolute executable directories");
        }
        safe_directories
            .into_iter()
            .find_map(|directory| first_executable_candidate(&directory.join(path), path_ext_env))
    }
    .ok_or_else(|| {
        anyhow::Error::msg(format!(
            "{family} binary was not found or is not executable"
        ))
    })
}

/// Validate and canonicalize the required ACP session working directory.
pub(crate) fn validate_working_directory(family: &str, value: &str) -> anyhow::Result<PathBuf> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!(
            "{family} requires an explicit working_directory for the ACP session boundary"
        );
    }
    let path = Path::new(trimmed);
    if !path.is_absolute() {
        anyhow::bail!("{family} working_directory must be an absolute path");
    }
    let canonical = std::fs::canonicalize(path).map_err(|_| {
        anyhow::Error::msg(format!(
            "{family} working_directory does not exist or is inaccessible"
        ))
    })?;
    if !canonical.is_dir() {
        anyhow::bail!("{family} working_directory must identify a directory");
    }
    Ok(canonical)
}

/// Validate the aggregate ACP stdout budget, defaulting when unset.
pub(crate) fn validate_acp_stdout_limit(
    family: &str,
    value: Option<usize>,
) -> anyhow::Result<usize> {
    let limit = value.unwrap_or(protocol::DEFAULT_ACP_STDOUT_LIMIT_BYTES);
    if !(protocol::MIN_ACP_STDOUT_LIMIT_BYTES..=protocol::MAX_ACP_STDOUT_LIMIT_BYTES)
        .contains(&limit)
    {
        anyhow::bail!(
            "{family} max_acp_stdout_bytes must be between {} and {} bytes",
            protocol::MIN_ACP_STDOUT_LIMIT_BYTES,
            protocol::MAX_ACP_STDOUT_LIMIT_BYTES
        );
    }
    Ok(limit)
}
