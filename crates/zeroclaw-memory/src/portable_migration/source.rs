//! Offline file capture. The original path is never opened by SQLite.

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const SOURCE_SUFFIXES: &[&str] = &[
    "",
    "-wal",
    "-shm",
    "-journal",
    ".migration-marker",
    ".identity.json",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFileDigest {
    pub suffix: String,
    pub present: bool,
    pub physical_identity: Option<String>,
    pub size: Option<u64>,
    pub modified: Option<String>,
    pub sha256: Option<String>,
}

pub(crate) fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(unix)]
pub(crate) fn physical_identity(metadata: &Metadata) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
pub(crate) fn physical_identity(_metadata: &Metadata) -> Result<String> {
    anyhow::bail!(
        "offline kernel migration currently supports Unix physical file identity and owner-only permissions only"
    )
}

fn modified(metadata: &Metadata) -> Result<String> {
    let elapsed = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .context("unsupported source modification timestamp")?;
    Ok(format!(
        "{}.{:09}",
        elapsed.as_secs(),
        elapsed.subsec_nanos()
    ))
}

fn same_file_state(before: &Metadata, after: &Metadata) -> Result<bool> {
    Ok(before.is_file()
        && after.is_file()
        && physical_identity(before)? == physical_identity(after)?
        && before.len() == after.len()
        && modified(before)? == modified(after)?)
}

pub(crate) fn file_digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    digest_reader(&mut file)
}

fn digest_reader(file: &mut File) -> Result<String> {
    let mut buffer = [0_u8; 64 * 1024];
    let mut hash = Sha256::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(crate) fn census(path: &Path) -> Result<Vec<SourceFileDigest>> {
    let mut files = Vec::new();
    for suffix in SOURCE_SUFFIXES {
        let file_path = suffixed(path, suffix);
        let metadata = match std::fs::symlink_metadata(&file_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ensure!(!suffix.is_empty(), "source database does not exist");
                files.push(SourceFileDigest {
                    suffix: (*suffix).into(),
                    present: false,
                    physical_identity: None,
                    size: None,
                    modified: None,
                    sha256: None,
                });
                continue;
            }
            Err(error) => return Err(error).context("inspect source file"),
        };
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "source database and sidecars must be regular non-symlink files"
        );
        ensure!(
            *suffix != "-journal" || metadata.len() == 0,
            "source has a rollback journal; finish or recover the owning process before offline migration"
        );
        let mut file = File::open(&file_path).context("open source file read-only")?;
        ensure!(
            same_file_state(&metadata, &file.metadata()?)?,
            "source file changed before reading"
        );
        let sha256 = digest_reader(&mut file)?;
        ensure!(
            same_file_state(&metadata, &file.metadata()?)?
                && same_file_state(&metadata, &std::fs::symlink_metadata(&file_path)?)?,
            "source file changed while reading; stop all writers before retrying"
        );
        files.push(SourceFileDigest {
            suffix: (*suffix).into(),
            present: true,
            physical_identity: Some(physical_identity(&metadata)?),
            size: Some(metadata.len()),
            modified: Some(modified(&metadata)?),
            sha256: Some(sha256),
        });
    }
    Ok(files)
}

pub(crate) fn verify_unchanged(path: &Path, expected: &[SourceFileDigest]) -> Result<()> {
    ensure!(
        census(path)? == expected,
        "source file set, physical identity, metadata, or content changed; offline migration refused"
    );
    Ok(())
}

pub(crate) fn stable_copy(
    source: &Path,
    private_copy: &Path,
    after_copy: impl FnOnce() -> Result<()>,
) -> Result<Vec<SourceFileDigest>> {
    let before = census(source)?;
    for entry in before
        .iter()
        .filter(|entry| entry.present && matches!(entry.suffix.as_str(), "" | "-wal"))
    {
        let original = suffixed(source, &entry.suffix);
        let destination = suffixed(private_copy, &entry.suffix);
        let mut read = File::open(&original)?;
        let mut write = super::create_private_file(&destination)?;
        let bytes = std::io::copy(&mut read, &mut write)?;
        write.flush()?;
        write.sync_all()?;
        ensure!(
            Some(bytes) == entry.size && Some(file_digest(&destination)?) == entry.sha256,
            "private copy does not match the captured source bytes"
        );
    }
    // Deterministic tests can change the owned source here. Production passes
    // an empty closure; there is no public mutation or bypass seam.
    after_copy()?;
    verify_unchanged(source, &before)?;
    // SHM is deliberately not copied or opened. SQLite builds its disposable
    // WAL index next to the private copy, leaving original read marks intact.
    Ok(before)
}
