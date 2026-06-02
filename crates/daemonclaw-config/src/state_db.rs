use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

pub struct StateDb {
    path: PathBuf,
}

impl StateDb {
    pub fn open(workspace_dir: &Path) -> Result<Self> {
        let state_dir = workspace_dir.join("state");
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("Failed to create state directory: {}", state_dir.display()))?;
        let path = state_dir.join("state.db");
        let db = Self { path };
        let conn = db.connect()?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;",
        )?;
        Ok(db)
    }

    pub fn connect(&self) -> Result<Connection> {
        Connection::open(&self.path)
            .with_context(|| format!("Failed to open state.db: {}", self.path.display()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_db_creates_with_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let db = StateDb::open(tmp.path()).unwrap();
        assert!(db.path().exists());

        let conn = db.connect().unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn state_db_reopen_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db1 = StateDb::open(tmp.path()).unwrap();
        let conn1 = db1.connect().unwrap();
        conn1
            .execute("CREATE TABLE IF NOT EXISTS test_track (id INTEGER)", [])
            .unwrap();
        drop(conn1);
        drop(db1);

        let db2 = StateDb::open(tmp.path()).unwrap();
        let conn2 = db2.connect().unwrap();
        let count: i64 = conn2
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'test_track'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
