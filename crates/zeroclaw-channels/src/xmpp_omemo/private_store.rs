//! Linux-only gate for existing legacy state. Never creates or repairs identity files.
//! The exclusive SQLite lock also excludes the unmodified Python SQLite writer.
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

pub struct PrivateStore {
    connection: Connection,
    // Released after the connection closes; protects against other native openers.
    _file: File,
}
fn private(metadata: &fs::Metadata, uid: u32, mode: u32) -> Result<()> {
    ensure!(metadata.uid() == uid, "State belongs to another user");
    ensure!(
        metadata.mode() & 0o777 == mode,
        "Unexpected state permissions"
    );
    Ok(())
}
impl PrivateStore {
    pub fn open_existing(directory: &Path) -> Result<Self> {
        Self::open(directory, false)
    }
    pub fn open_tls_only(directory: &Path) -> Result<Self> {
        Self::open(directory, true)
    }
    fn open(directory: &Path, tls_only: bool) -> Result<Self> {
        ensure!(directory.is_absolute(), "Absolute state directory required");
        let uid = fs::metadata("/proc/self")?.uid();
        let mut walked = std::path::PathBuf::new();
        for part in directory.components() {
            ensure!(
                !matches!(part, Component::ParentDir | Component::CurDir),
                "Noncanonical state path"
            );
            walked.push(part);
            let metadata = fs::symlink_metadata(&walked)?;
            ensure!(
                !metadata.file_type().is_symlink() && metadata.is_dir(),
                "Symlink or nondirectory state ancestor"
            );
            // Root-owned ancestors such as /var and /var/lib are allowed; writable
            // shared ancestors (/tmp, /var/tmp) must have the sticky bit.
            ensure!(
                metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0,
                "Unprotected writable state ancestor"
            );
            ensure!(
                metadata.uid() == 0 || metadata.uid() == uid,
                "Untrusted state ancestor owner"
            );
        }
        private(&fs::symlink_metadata(directory)?, uid, 0o700)?;
        let path = directory.join("omemo.sqlite3");
        let before = fs::symlink_metadata(&path)?;
        ensure!(
            before.is_file() && !before.file_type().is_symlink() && before.nlink() == 1,
            "Invalid state database file"
        );
        private(&before, uid, 0o600)?;
        // Linux O_NOFOLLOW. No unsafe code or file creation is required.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x20000)
            .open(&path)?;
        let actual = file.metadata()?;
        ensure!(
            actual.ino() == before.ino() && actual.dev() == before.dev(),
            "Database changed during open"
        );
        file.try_lock()
            .context("Native state owner is already active")?;
        for suffix in ["-journal", "-wal", "-shm"] {
            let sidecar = directory.join(format!("omemo.sqlite3{suffix}"));
            match fs::symlink_metadata(sidecar) {
                Ok(m) => {
                    ensure!(
                        m.is_file() && !m.file_type().is_symlink() && m.nlink() == 1,
                        "Invalid SQLite sidecar"
                    );
                    private(&m, uid, 0o600)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(std::time::Duration::ZERO)?;
        let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
        ensure!(
            mode == "delete",
            "Unsupported journal mode; explicit quiesced migration required"
        );
        connection.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; SELECT count(*) FROM sqlite_master; COMMIT;")?;
        let after = fs::symlink_metadata(&path)?;
        ensure!(
            after.ino() == actual.ino() && after.dev() == actual.dev(),
            "Database replaced during SQLite open"
        );
        let check: String = connection.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        ensure!(check == "ok", "Database integrity check failed");
        let columns = |table: &str| -> Result<Vec<(String, String)>> {
            Ok(connection
                .prepare(&format!("PRAGMA table_info({table})"))?
                .query_map([], |r| Ok((r.get(1)?, r.get(2)?)))?
                .collect::<std::result::Result<_, _>>()?)
        };
        if tls_only {
            let objects: Vec<(String,String)> = connection.prepare("SELECT type,name FROM sqlite_master WHERE name NOT LIKE 'sqlite_autoindex_%' ORDER BY name")?
                .query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<std::result::Result<_,_>>()?;
            ensure!(
                objects == vec![("table".into(), "seen".into())],
                "Not an exact TLS-only database"
            );
            let schema: Vec<(String, String, bool, i64)> = connection
                .prepare("PRAGMA table_info(seen)")?
                .query_map([], |r| Ok((r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?)))?
                .collect::<std::result::Result<_, _>>()?;
            ensure!(
                schema
                    == vec![
                        ("sender".into(), "TEXT".into(), false, 1),
                        ("id".into(), "TEXT".into(), false, 2)
                    ],
                "Not the known TLS-only replay schema"
            );
        } else {
            ensure!(
                columns("state")?
                    == vec![
                        ("key".into(), "TEXT".into()),
                        ("value".into(), "TEXT".into())
                    ],
                "Unsupported state schema"
            );
        }
        ensure!(
            columns("seen")?
                == vec![
                    ("sender".into(), "TEXT".into()),
                    ("id".into(), "TEXT".into())
                ],
            "Unsupported replay schema"
        );
        Ok(Self {
            connection,
            _file: file,
        })
    }
    /// Creates only a missing channel directory. Existing directories, even
    /// empty ones, belong to the migration/recovery path and are never repaired.
    pub fn create_new(directory: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        ensure!(directory.is_absolute(), "Absolute state directory required");
        let mut ancestor = directory.parent().context("Missing state parent")?;
        loop {
            match fs::symlink_metadata(ancestor) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .context("Missing existing state ancestor")?
                }
                Err(error) => return Err(error.into()),
            }
        }
        let uid = fs::metadata("/proc/self")?.uid();
        let mut walked = std::path::PathBuf::new();
        for component in ancestor.components() {
            ensure!(
                !matches!(component, Component::CurDir | Component::ParentDir),
                "Noncanonical state path"
            );
            walked.push(component);
            let meta = fs::symlink_metadata(&walked)?;
            ensure!(
                meta.is_dir()
                    && !meta.file_type().is_symlink()
                    && (meta.uid() == 0 || meta.uid() == uid)
                    && (meta.mode() & 0o022 == 0 || meta.mode() & 0o1000 != 0),
                "Untrusted state creation ancestor"
            );
        }
        ensure!(
            !directory
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir)),
            "Noncanonical state path"
        );
        let parent = directory.parent().context("Missing state parent")?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        fs::DirBuilder::new().mode(0o700).create(directory)?;
        let path = directory.join("omemo.sqlite3");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));")?;
        drop(conn);
        file.sync_all()?;
        drop(file);
        File::open(directory)?.sync_all()?;
        Self::open_existing(directory)
    }
    pub fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn creates_only_missing_private_state_and_refuses_existing_empty_directory() -> Result<()> {
        let root = fixture();
        let path = root.join("new-channel");
        let mut owner = PrivateStore::create_new(&path)?;
        let count: i64 = owner
            .connection()
            .query_row("SELECT count(*) FROM state", [], |r| r.get(0))?;
        assert_eq!(count, 0);
        assert_eq!(fs::metadata(&path)?.mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(path.join("omemo.sqlite3"))?.mode() & 0o777,
            0o600
        );
        assert!(PrivateStore::create_new(&path).is_err());
        drop(owner);
        let partial = root.join("partial");
        fs::create_dir(&partial)?;
        assert!(PrivateStore::create_new(&partial).is_err());
        assert!(!partial.join("omemo.sqlite3").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    #[test]
    fn refuses_state_creation_through_symlink_ancestor() -> Result<()> {
        let root = fixture();
        let target = root.join("target");
        fs::create_dir(&target)?;
        let link = root.join("link");
        symlink(&target, &link)?;
        assert!(PrivateStore::create_new(&link.join("channel")).is_err());
        assert!(!target.join("channel").exists());
        fs::remove_dir_all(root)?;
        Ok(())
    }
    fn fixture() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "omemo-private-store-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let db = path.join("omemo.sqlite3");
        let c = Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE state(key TEXT PRIMARY KEY,value TEXT NOT NULL);CREATE TABLE seen(sender TEXT,id TEXT,PRIMARY KEY(sender,id));").unwrap();
        drop(c);
        fs::set_permissions(db, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
    #[test]
    fn excludes_native_and_unmodified_sqlite_clients_until_close() {
        let path = fixture();
        let mut owner = PrivateStore::open_existing(&path).unwrap();
        assert!(PrivateStore::open_existing(&path).is_err());
        let old = Connection::open(path.join("omemo.sqlite3")).unwrap();
        old.busy_timeout(std::time::Duration::ZERO).unwrap();
        assert!(
            old.query_row("SELECT count(*) FROM state", [], |r| r.get::<_, i64>(0))
                .is_err()
        );
        owner
            .connection()
            .execute("INSERT INTO state VALUES('fixture','true')", [])
            .unwrap();
        drop(owner);
        assert_eq!(
            old.query_row("SELECT count(*) FROM state", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(old);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn rejects_permissions_symlinks_and_hardlinks_without_repairing() {
        let path = fixture();
        let db = path.join("omemo.sqlite3");
        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(PrivateStore::open_existing(&path).is_err());
        assert_eq!(fs::metadata(&db).unwrap().mode() & 0o777, 0o644);
        fs::set_permissions(&db, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&db, path.join("copy")).unwrap();
        assert!(PrivateStore::open_existing(&path).is_err());
        fs::remove_file(path.join("copy")).unwrap();
        symlink(&db, path.join("omemo.sqlite3-wal")).unwrap();
        assert!(PrivateStore::open_existing(&path).is_err());
        fs::remove_file(path.join("omemo.sqlite3-wal")).unwrap();
        let link = path.with_extension("link");
        symlink(&path, &link).unwrap();
        assert!(PrivateStore::open_existing(&link).is_err());
        fs::remove_file(link).unwrap();
        assert!(PrivateStore::open_existing(&path).is_ok());
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn refuses_busy_python_transaction_without_changing_its_data() {
        let path = fixture();
        let old = Connection::open(path.join("omemo.sqlite3")).unwrap();
        old.execute_batch("BEGIN IMMEDIATE;INSERT INTO state VALUES('uncommitted','true');")
            .unwrap();
        assert!(PrivateStore::open_existing(&path).is_err());
        old.execute_batch("COMMIT;").unwrap();
        drop(old);
        let mut native = PrivateStore::open_existing(&path).unwrap();
        assert_eq!(
            native
                .connection()
                .query_row("SELECT value FROM state WHERE key='uncommitted'", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "true"
        );
        drop(native);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn refuses_wal_and_unknown_schema_without_migrating() {
        let path = fixture();
        let db = path.join("omemo.sqlite3");
        let old = Connection::open(&db).unwrap();
        old.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        drop(old);
        assert!(PrivateStore::open_existing(&path).is_err());
        let old = Connection::open(&db).unwrap();
        assert_eq!(
            old.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
        old.execute_batch("PRAGMA journal_mode=DELETE;ALTER TABLE state ADD COLUMN unknown TEXT;")
            .unwrap();
        drop(old);
        assert!(PrivateStore::open_existing(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }
}
