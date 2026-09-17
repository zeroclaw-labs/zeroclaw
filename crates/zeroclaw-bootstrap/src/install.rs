//! Installing exactly the approved artifact.
//!
//! The order here is the security property: the approval token is checked
//! against the plan recomputed *now*, the artifact is fetched into memory, its
//! digest is checked against the plan, and only then is anything written under
//! the install directory. No byte reaches the install path before the digest
//! matches.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use zeroclaw_dist::ArchiveKind;

use crate::error::BootstrapError;
use crate::fetch::{Fetcher, sha256_hex};
use crate::plan::InstallPlan;

/// What an install actually did.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Where the binary was written.
    pub binary_path: PathBuf,
    /// Digest of the archive that was verified.
    pub artifact_digest: String,
    /// Digest of the installed binary, used later as executable identity.
    pub binary_digest: String,
}

/// Checks the approval token against `plan`.
///
/// Separated from [`install`] so the binding is one named, independently
/// testable rule rather than a condition buried in a longer function.
pub fn check_approval(plan: &InstallPlan, approve: Option<&str>) -> Result<(), BootstrapError> {
    let Some(provided) = approve else {
        return Err(BootstrapError::ApprovalMissing);
    };
    let expected = plan.digest();
    if provided.trim() != expected {
        return Err(BootstrapError::ApprovalMismatch {
            expected,
            provided: provided.trim().to_string(),
        });
    }
    Ok(())
}

/// Downloads, verifies, and installs the approved artifact.
pub fn install(
    fetcher: &dyn Fetcher,
    plan: &InstallPlan,
    approve: Option<&str>,
) -> Result<InstallOutcome, BootstrapError> {
    check_approval(plan, approve)?;

    let archive = fetcher.fetch(&plan.source_url)?;

    let actual = sha256_hex(&archive);
    if actual != plan.artifact_digest {
        return Err(BootstrapError::DigestMismatch {
            expected: plan.artifact_digest.clone(),
            actual,
        });
    }

    let binary = extract_binary(&archive, plan.target.archive, plan.target.binary_name)?;

    std::fs::create_dir_all(&plan.install_dir).map_err(|err| {
        BootstrapError::io(
            format!("creating install directory {}", plan.install_dir.display()),
            &err,
        )
    })?;
    write_executable(&plan.binary_path, &binary)?;

    Ok(InstallOutcome {
        binary_path: plan.binary_path.clone(),
        artifact_digest: plan.artifact_digest.clone(),
        binary_digest: sha256_hex(&binary),
    })
}

/// Rejects any archive entry path that could escape the install directory.
///
/// Entries are matched by exact file name, so a nested `evil/zeroclaw` never
/// stands in for the top-level binary either.
fn vet_entry_path(raw: &str) -> Result<&str, BootstrapError> {
    let reject = |reason: &'static str| BootstrapError::UnsafeArchiveEntry {
        entry: raw.to_string(),
        reason,
    };
    if raw.contains('\0') {
        return Err(reject("contains a NUL byte"));
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(reject("is an absolute path"));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => return Err(reject("contains a `..` component")),
            Component::Prefix(_) | Component::RootDir => {
                return Err(reject("contains a filesystem root or drive prefix"));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(raw)
}

/// Pulls the registry-named primary binary out of the verified archive.
fn extract_binary(
    archive: &[u8],
    kind: ArchiveKind,
    binary_name: &str,
) -> Result<Vec<u8>, BootstrapError> {
    match kind {
        ArchiveKind::TarGz => extract_from_tar_gz(archive, binary_name),
        ArchiveKind::Zip => extract_from_zip(archive, binary_name),
    }
}

fn extract_from_tar_gz(archive: &[u8], binary_name: &str) -> Result<Vec<u8>, BootstrapError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|err| BootstrapError::io("reading the tar archive", &err))?;

    for entry in entries {
        let mut entry = entry.map_err(|err| BootstrapError::io("reading a tar entry", &err))?;
        let path = entry
            .path()
            .map_err(|err| BootstrapError::io("decoding a tar entry path", &err))?
            .to_string_lossy()
            .into_owned();
        let vetted = vet_entry_path(&path)?;

        let entry_type = entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(BootstrapError::UnsafeArchiveEntry {
                entry: vetted.to_string(),
                reason: "is a link entry",
            });
        }
        if !entry_type.is_file() {
            continue;
        }
        if Path::new(vetted).file_name().and_then(|n| n.to_str()) != Some(binary_name)
            || Path::new(vetted)
                .parent()
                .is_some_and(|p| !p.as_os_str().is_empty())
        {
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| BootstrapError::io("reading the binary from the tar archive", &err))?;
        return Ok(bytes);
    }

    Err(BootstrapError::BinaryMissingFromArchive {
        expected: binary_name.to_string(),
    })
}

fn extract_from_zip(archive: &[u8], binary_name: &str) -> Result<Vec<u8>, BootstrapError> {
    let cursor = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(cursor)
        .map_err(|err| BootstrapError::io("reading the zip archive", &err))?;

    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|err| BootstrapError::io("reading a zip entry", &err))?;
        let raw = entry.name().to_string();
        let vetted = vet_entry_path(&raw)?;
        if !entry.is_file() {
            continue;
        }
        if Path::new(vetted).file_name().and_then(|n| n.to_str()) != Some(binary_name)
            || Path::new(vetted)
                .parent()
                .is_some_and(|p| !p.as_os_str().is_empty())
        {
            continue;
        }

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|err| BootstrapError::io("reading the binary from the zip archive", &err))?;
        return Ok(bytes);
    }

    Err(BootstrapError::BinaryMissingFromArchive {
        expected: binary_name.to_string(),
    })
}

/// Writes the binary and marks it executable on Unix.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), BootstrapError> {
    std::fs::write(path, bytes)
        .map_err(|err| BootstrapError::io(format!("writing {}", path.display()), &err))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|err| {
            BootstrapError::io(format!("marking {} executable", path.display()), &err)
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vets_ordinary_entry_paths() {
        assert!(vet_entry_path("zeroclaw").is_ok());
        assert!(vet_entry_path("web/dist/index.html").is_ok());
        assert!(vet_entry_path("./zeroclaw").is_ok());
    }

    #[test]
    fn refuses_entries_that_escape_the_install_directory() {
        for hostile in [
            "/etc/passwd",
            "../../../etc/passwd",
            "web/../../zeroclaw",
            "a/../../b",
        ] {
            assert!(
                vet_entry_path(hostile).is_err(),
                "entry `{hostile}` must be refused"
            );
        }
    }
}
