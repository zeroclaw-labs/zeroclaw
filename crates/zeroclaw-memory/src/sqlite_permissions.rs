use std::path::{Path, PathBuf};

#[cfg(unix)]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    tighten_directory(&dir)
}

#[cfg(unix)]
fn tighten_directory(dir: &std::fs::File) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = dir.metadata()?;
    anyhow::ensure!(
        metadata.is_dir(),
        "SQLite storage parent is not a directory"
    );
    anyhow::ensure!(
        metadata.uid() == effective_uid(),
        "SQLite storage directory is not owned by the current user"
    );
    // Only the admitted directory is changed, even if its pathname is replaced.
    dir.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn ensure_owner_only_dir(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(unix)]
fn validate_file_metadata(metadata: &std::fs::Metadata) -> anyhow::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    anyhow::ensure!(
        metadata.is_file(),
        "SQLite storage entry is not a regular file"
    );
    anyhow::ensure!(
        metadata.uid() == effective_uid(),
        "SQLite storage file is not owned by the current user"
    );
    anyhow::ensure!(
        metadata.nlink() == 1,
        "SQLite storage file has multiple links"
    );
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "SQLite storage file has group or other permissions; repair its permissions before reopening"
    );
    Ok(())
}

#[cfg(unix)]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) => return validate_file_metadata(&metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    // Never open an existing DB/SHM for chmod: closing even an unrelated fd
    // releases this process's POSIX locks, bypassing SQLite's deferred closes.
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => validate_file_metadata(&file.metadata()?),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_file_metadata(&std::fs::symlink_metadata(path)?)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn ensure_owner_only_file(path: &Path) -> anyhow::Result<()> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
pub(crate) fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

/// Inspect existing files without acquiring or closing any SQLite file handles.
#[cfg(unix)]
pub(crate) fn check_sqlite_storage(db_path: &Path) -> anyhow::Result<()> {
    validate_file_metadata(&std::fs::symlink_metadata(db_path)?)?;
    for suffix in ["-wal", "-shm"] {
        match std::fs::symlink_metadata(sqlite_sidecar_path(db_path, suffix)) {
            Ok(metadata) => validate_file_metadata(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn check_sqlite_storage(_db_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Prepare the two auxiliary stores, not arbitrary memory backend paths.
pub(crate) fn prepare_sqlite_storage(
    storage_root: &Path,
    database_name: &str,
) -> anyhow::Result<PathBuf> {
    // A second constructor must not admit a newly created DB until the first
    // constructor has closed its creation handle. This is not a SQLite lock.
    static PREPARATION: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _preparation = PREPARATION.lock();

    std::fs::create_dir_all(storage_root)?;
    // The operator-selected root is trusted and may intentionally be a symlink.
    // Its memory child and SQLite leaves are not granted that exception.
    #[cfg(unix)]
    let storage_root = std::fs::canonicalize(storage_root)?;
    let memory_dir = storage_root.join("memory");
    ensure_owner_only_dir(&memory_dir)?;
    let db_path = memory_dir.join(database_name);
    ensure_owner_only_file(&db_path)?;
    check_sqlite_storage(&db_path)?;
    Ok(db_path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
    use tempfile::TempDir;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn open_store(root: &Path, database: &str) -> anyhow::Result<()> {
        if database == "audit.db" {
            crate::audit::AuditedMemory::new(crate::none::NoneMemory::new("none"), root).map(|_| ())
        } else {
            crate::response_cache::ResponseCache::new(root, 60, 1000).map(|_| ())
        }
    }

    #[test]
    fn sqlite_storage_constructors_reject_symlink_leaves() {
        for database in ["response_cache.db", "audit.db"] {
            for suffix in ["memory", "", "-wal", "-shm"] {
                let root = TempDir::new().unwrap();
                let outside = TempDir::new().unwrap();
                let sentinel = outside.path().join("sentinel");
                std::fs::write(&sentinel, b"unchanged").unwrap();
                std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o666))
                    .unwrap();
                std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o755))
                    .unwrap();
                if suffix == "memory" {
                    symlink(outside.path(), root.path().join("memory")).unwrap();
                } else {
                    let db = prepare_sqlite_storage(root.path(), database).unwrap();
                    if suffix.is_empty() {
                        std::fs::remove_file(&db).unwrap();
                    }
                    symlink(&sentinel, sqlite_sidecar_path(&db, suffix)).unwrap();
                }
                assert!(
                    open_store(root.path(), database).is_err(),
                    "{database} {suffix}"
                );
                assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
                assert_eq!(mode(&sentinel), 0o666);
                assert_eq!(mode(outside.path()), 0o755);
            }
        }
    }

    #[test]
    fn sqlite_storage_rejects_permissive_existing_files_without_chmod() {
        for suffix in ["", "-wal", "-shm"] {
            let root = TempDir::new().unwrap();
            let db = prepare_sqlite_storage(root.path(), "response_cache.db").unwrap();
            let entry = sqlite_sidecar_path(&db, suffix);
            std::fs::write(&entry, b"unchanged").unwrap();
            std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(prepare_sqlite_storage(root.path(), "response_cache.db").is_err());
            assert_eq!(mode(&entry), 0o666);
            assert_eq!(std::fs::read(entry).unwrap(), b"unchanged");
        }
    }

    #[test]
    fn sqlite_storage_rejects_hardlinks_and_special_files() {
        for suffix in ["", "-wal", "-shm"] {
            for kind in ["hardlink", "directory", "fifo", "socket"] {
                let root = TempDir::new().unwrap();
                let db = prepare_sqlite_storage(root.path(), "response_cache.db").unwrap();
                let entry = sqlite_sidecar_path(&db, suffix);
                if suffix.is_empty() {
                    std::fs::remove_file(&entry).unwrap();
                }
                let sentinel = root.path().join("sentinel");
                std::fs::write(&sentinel, b"unchanged").unwrap();
                std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                let _socket = match kind {
                    "hardlink" => {
                        std::fs::hard_link(&sentinel, &entry).unwrap();
                        None
                    }
                    "directory" => {
                        std::fs::create_dir(&entry).unwrap();
                        None
                    }
                    "fifo" => {
                        use std::os::unix::ffi::OsStrExt;
                        let path = std::ffi::CString::new(entry.as_os_str().as_bytes()).unwrap();
                        // SAFETY: path is NUL-terminated and lives through mkfifo.
                        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                        None
                    }
                    "socket" => Some(std::os::unix::net::UnixListener::bind(&entry).unwrap()),
                    _ => unreachable!(),
                };
                assert!(
                    prepare_sqlite_storage(root.path(), "response_cache.db").is_err(),
                    "{suffix} {kind}"
                );
                assert_eq!(mode(&sentinel), 0o600);
                assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged");
            }
        }
    }

    #[test]
    fn sqlite_storage_directory_chmod_stays_on_admitted_handle() {
        let root = TempDir::new().unwrap();
        let original = root.path().join("memory");
        let moved = root.path().join("moved");
        let outside = root.path().join("outside");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dir = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&original)
            .unwrap();
        std::fs::rename(&original, &moved).unwrap();
        symlink(&outside, &original).unwrap();
        tighten_directory(&dir).unwrap();
        assert_eq!(mode(&moved), 0o700);
        assert_eq!(mode(&outside), 0o755);
    }

    #[test]
    fn sqlite_storage_preserves_root_symlinks_and_does_not_precreate_sidecars() {
        let root = TempDir::new().unwrap();
        let alias_parent = TempDir::new().unwrap();
        let alias = alias_parent.path().join("alias");
        symlink(root.path(), &alias).unwrap();
        let db = prepare_sqlite_storage(&alias, "response_cache.db").unwrap();
        assert_eq!(
            db,
            root.path()
                .canonicalize()
                .unwrap()
                .join("memory/response_cache.db")
        );
        assert_eq!(mode(db.parent().unwrap()), 0o700);
        assert_eq!(mode(&db), 0o600);
        assert!(!sqlite_sidecar_path(&db, "-wal").exists());
        assert!(!sqlite_sidecar_path(&db, "-shm").exists());
        open_store(&alias, "response_cache.db").unwrap();
        open_store(&alias, "audit.db").unwrap();
    }

    #[test]
    fn sqlite_storage_nofollow_rejects_database_symlink_after_preparation() {
        let root = TempDir::new().unwrap();
        let db = prepare_sqlite_storage(root.path(), "response_cache.db").unwrap();
        let original = root.path().join("original.db");
        std::fs::rename(&db, &original).unwrap();
        symlink(&original, &db).unwrap();
        let flags = rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
        assert!(rusqlite::Connection::open_with_flags(&db, flags).is_err());
        assert_eq!(std::fs::metadata(&original).unwrap().len(), 0);
        assert_eq!(mode(&original), 0o600);
    }

    #[test]
    fn sqlite_storage_inspection_preserves_live_database_locks() {
        for journal in ["DELETE", "WAL"] {
            let root = TempDir::new().unwrap();
            let db = prepare_sqlite_storage(root.path(), "response_cache.db").unwrap();
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(&format!("PRAGMA journal_mode = {journal}; CREATE TABLE lock_probe (value INTEGER); BEGIN IMMEDIATE;")).unwrap();
            prepare_sqlite_storage(root.path(), "response_cache.db").unwrap();
            check_sqlite_storage(&db).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "sqlite_permissions::tests::sqlite_storage_lock_probe",
                    "--ignored",
                ])
                .env("ZEROCLAW_SQLITE_LOCK_PROBE_PATH", &db)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            // Also prove the child actually ran rather than selecting zero tests.
            assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
            conn.execute_batch("ROLLBACK;").unwrap();
        }
    }

    #[test]
    #[ignore = "subprocess probe for sqlite_storage_inspection_preserves_live_database_locks"]
    fn sqlite_storage_lock_probe() {
        let Some(db) = std::env::var_os("ZEROCLAW_SQLITE_LOCK_PROBE_PATH") else {
            // Broad ignored-test runs do not provide the parent-owned lock fixture.
            return;
        };
        let conn = rusqlite::Connection::open(PathBuf::from(db)).unwrap();
        conn.busy_timeout(std::time::Duration::ZERO).unwrap();
        let error = conn
            .execute("INSERT INTO lock_probe VALUES (1)", [])
            .unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        );
    }
}
