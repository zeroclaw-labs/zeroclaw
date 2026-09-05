use std::path::Path;

#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

#[cfg(unix)]
fn harden_existing_sqlite_sidecars(db_path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(db_path, suffix);
        match std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn harden_existing_sqlite_sidecars(_db_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn harden_sqlite_storage(db_path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        ensure_owner_only_dir(parent)?;
    }
    ensure_owner_only_file(db_path)?;
    harden_existing_sqlite_sidecars(db_path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn sqlite_storage_permissions_harden_existing_sidecars() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("response_cache.db");
        std::fs::write(&db_path, []).unwrap();

        let sidecars = [
            sqlite_sidecar_path(&db_path, "-wal"),
            sqlite_sidecar_path(&db_path, "-shm"),
        ];
        for sidecar in &sidecars {
            std::fs::write(sidecar, []).unwrap();
            std::fs::set_permissions(sidecar, std::fs::Permissions::from_mode(0o666)).unwrap();
        }

        harden_sqlite_storage(&db_path).unwrap();

        for sidecar in &sidecars {
            assert!(sidecar.exists());
            assert_eq!(mode(sidecar), 0o600);
        }
    }
}
