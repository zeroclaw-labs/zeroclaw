//! Private, no-follow credential I/O. Never include file contents in errors.
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

#[cfg(test)]
tokio::task_local! { pub(super) static FAIL_REPLACEMENT_NUMBER: std::cell::Cell<usize>; }

#[cfg(test)]
pub(super) struct PersistencePause {
    pub remaining: std::cell::Cell<usize>,
    pub stage: String,
    pub ready: std::path::PathBuf,
}

#[cfg(test)]
tokio::task_local! { pub(super) static PERSISTENCE_PAUSE: PersistencePause; }

#[cfg(test)]
fn pause_persistence(stage: &str) {
    let _ = PERSISTENCE_PAUSE.try_with(|pause| {
        if pause.stage != stage {
            return;
        }
        let count = pause.remaining.get();
        pause.remaining.set(count.saturating_sub(1));
        if count == 1 {
            std::fs::write(&pause.ready, stage).unwrap();
            // Only a synthetic child enables this hook. The parent kills it
            // while its real canonical store guard remains held.
            loop {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    });
}

fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn check_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "Credential path is not a regular file");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        anyhow::ensure!(
            metadata.file_attributes()
                & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                == 0,
            "Credential path is a reparse point"
        );
    }
    Ok(())
}

pub(super) fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "Credential path is a symlink"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Unable to inspect credential path"),
    }
    Ok(())
}

pub(super) fn read(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut file = match options().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Unable to open credential file"),
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .context("Unable to read credential file")?;
    Ok(Some(bytes))
}

pub(super) fn lock_file(path: &Path) -> Result<File> {
    reject_symlink(path)?;
    let file = options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .context("Unable to open credential refresh lock")?;
    check_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
pub(super) fn same_inode(file: &File, path: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    // Inspect the leaf itself. Opening an unknown legacy FIFO/device merely to
    // compare identity could block or trigger I/O before checking its type.
    let actual = match std::fs::symlink_metadata(path) {
        Ok(actual) => actual,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("Unable to inspect credential lock identity"),
    };
    anyhow::ensure!(actual.is_file(), "Credential lock is not a regular file");
    let expected = file.metadata()?;
    Ok(expected.dev() == actual.dev() && expected.ino() == actual.ino())
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    reject_symlink(path)?;
    let temporary = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = options()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .context("Unable to create private credential temporary file")?;
        file.write_all(bytes)
            .context("Unable to write credential temporary file")?;
        file.sync_all()
            .context("Unable to sync credential temporary file")?;
        #[cfg(test)]
        pause_persistence("before-replace");
        reject_symlink(path)?;
        #[cfg(test)]
        if FAIL_REPLACEMENT_NUMBER
            .try_with(|remaining| {
                let count = remaining.get();
                remaining.set(count.saturating_sub(1));
                count == 1
            })
            .unwrap_or(false)
        {
            anyhow::bail!("Synthetic atomic credential replacement failure");
        }
        std::fs::rename(&temporary, path).context("Unable to replace credential file")?;
        #[cfg(test)]
        pause_persistence("after-replace");
        #[cfg(unix)]
        File::open(path.parent().context("Credential file has no directory")?)?
            .sync_all()
            .context("Unable to sync credential directory after replacement")?;
        Ok(())
    })();
    // Cleanup is best effort; the temporary file is private even after a crash.
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
