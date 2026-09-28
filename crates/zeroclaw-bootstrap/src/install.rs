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
/// Install `bytes` as the executable at `path`, atomically.
///
/// The bytes go to a fresh temporary file in the same directory, which is
/// marked executable and flushed, then renamed over `path`. A reader therefore
/// sees either the old binary or the complete new one, never a truncated file,
/// and a running old binary is replaced rather than rewritten in place (no
/// `ETXTBSY`). A destination that is a symbolic link or not a regular file is
/// refused before anything is written, so the install cannot be redirected to
/// overwrite a file elsewhere.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), BootstrapError> {
    refuse_unsafe_destination(path)?;
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "zeroclaw".to_string());
    let (tmp_path, file) = create_unique_temp(dir, &name)?;

    let written = finish_temp(file, bytes).and_then(|()| std::fs::rename(&tmp_path, path));
    if let Err(err) = written {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(BootstrapError::io(
            format!("installing {}", path.display()),
            &err,
        ));
    }
    Ok(())
}

/// Refuse a destination that exists but is not a regular file. A missing
/// destination is fine: that is a first install.
fn refuse_unsafe_destination(path: &Path) -> Result<(), BootstrapError> {
    let refuse = |reason: &str| BootstrapError::UnsafeInstallTarget {
        path: path.display().to_string(),
        reason: reason.to_string(),
    };
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(refuse("a symbolic link")),
        Ok(meta) if !meta.is_file() => Err(refuse("not a regular file")),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(BootstrapError::io(
            format!("inspecting {}", path.display()),
            &err,
        )),
    }
}

/// Create a new, exclusively owned temporary file next to the destination.
/// `create_new` fails rather than following anything already at the name.
fn create_unique_temp(
    dir: &Path,
    name: &str,
) -> Result<(std::path::PathBuf, std::fs::File), BootstrapError> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for attempt in 0..16u32 {
        let candidate = dir.join(format!(
            ".{name}.{}.{nanos}.{attempt}.tmp",
            std::process::id()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(file) => return Ok((candidate, file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(BootstrapError::io(
                    format!("creating a temporary file in {}", dir.display()),
                    &err,
                ));
            }
        }
    }
    Err(BootstrapError::Io {
        context: format!("creating a temporary file in {}", dir.display()),
        reason: "no free temporary name".to_string(),
    })
}

/// Write, mark executable, and flush the temporary file before it is renamed.
fn finish_temp(mut file: std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    }
    file.sync_all()
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

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_fresh_install_is_executable_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().expect("temp");
        let dest = dir.path().join("zeroclaw");
        write_executable(&dest, b"new binary").expect("install");
        assert_eq!(std::fs::read(&dest).expect("read"), b"new binary");
        assert_eq!(entries(dir.path()), vec!["zeroclaw".to_string()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dest).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    /// Replacement goes through a rename, so the old file is swapped out whole
    /// rather than rewritten in place (no truncated reads, no ETXTBSY).
    #[cfg(unix)]
    #[test]
    fn replacing_a_binary_swaps_in_a_new_file_rather_than_rewriting_it() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().expect("temp");
        let dest = dir.path().join("zeroclaw");
        write_executable(&dest, b"old binary").expect("first install");
        let old_inode = std::fs::metadata(&dest).expect("meta").ino();

        write_executable(&dest, b"new binary").expect("replace");

        assert_eq!(std::fs::read(&dest).expect("read"), b"new binary");
        assert_ne!(
            std::fs::metadata(&dest).expect("meta").ino(),
            old_inode,
            "the destination must be a new file, not the old one rewritten"
        );
        assert_eq!(entries(dir.path()), vec!["zeroclaw".to_string()]);
    }

    /// A symlink at the destination must not redirect the write to its target.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_destination_is_refused_and_its_target_untouched() {
        let dir = tempfile::tempdir().expect("temp");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"do not overwrite").expect("victim");
        let dest = dir.path().join("zeroclaw");
        std::os::unix::fs::symlink(&victim, &dest).expect("symlink");

        let err = write_executable(&dest, b"new binary").expect_err("must refuse");

        assert!(
            matches!(err, BootstrapError::UnsafeInstallTarget { .. }),
            "unexpected error: {err}"
        );
        assert_eq!(std::fs::read(&victim).expect("read"), b"do not overwrite");
        assert!(
            std::fs::symlink_metadata(&dest)
                .expect("meta")
                .file_type()
                .is_symlink(),
            "the symlink itself must be left as it was"
        );
        assert_eq!(
            entries(dir.path()),
            vec!["victim".to_string(), "zeroclaw".to_string()],
            "no temporary file may be left behind"
        );
    }

    #[test]
    fn a_directory_at_the_destination_is_refused() {
        let dir = tempfile::tempdir().expect("temp");
        let dest = dir.path().join("zeroclaw");
        std::fs::create_dir(&dest).expect("dir");
        let err = write_executable(&dest, b"new binary").expect_err("must refuse");
        assert!(
            matches!(err, BootstrapError::UnsafeInstallTarget { .. }),
            "unexpected error: {err}"
        );
        assert!(dest.is_dir());
    }

    /// A replacement that cannot complete must leave the existing binary
    /// exactly as it was and clean up after itself.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_replacement_leaves_the_existing_binary_intact() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("temp");
        let dest = dir.path().join("zeroclaw");
        write_executable(&dest, b"old binary").expect("first install");

        // Make the directory unwritable so the replacement fails before the
        // rename. Root ignores directory permissions, so skip there.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod");
        let probe = dir.path().join("probe");
        if std::fs::write(&probe, b"x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
                .expect("chmod back");
            eprintln!("skipping: directory permissions are not enforced for this user");
            return;
        }

        let result = write_executable(&dest, b"new binary");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");

        assert!(result.is_err(), "the replacement must fail");
        assert_eq!(std::fs::read(&dest).expect("read"), b"old binary");
        assert_eq!(entries(dir.path()), vec!["zeroclaw".to_string()]);
    }
}
