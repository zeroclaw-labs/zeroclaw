use anyhow::{Context, Result};
use rusqlite::{Connection, params};
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

    pub fn ensure_devices_table(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS devices (
                 token_hash  TEXT PRIMARY KEY,
                 id          TEXT NOT NULL,
                 name        TEXT,
                 device_type TEXT,
                 paired_at   TEXT NOT NULL,
                 last_seen   TEXT NOT NULL,
                 ip_address  TEXT
             );",
        )
        .context("Failed to create devices table in state.db")?;
        Ok(())
    }

    pub fn ensure_daemon_health_table(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS daemon_health (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 written_at  TEXT NOT NULL,
                 pid         INTEGER,
                 uptime_secs INTEGER,
                 shutdown    TEXT,
                 components  TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_daemon_health_written
                 ON daemon_health(written_at);",
        )
        .context("Failed to create daemon_health table in state.db")?;
        Ok(())
    }

    pub fn ensure_hygiene_state_table(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS hygiene_state (
                 id               INTEGER PRIMARY KEY CHECK (id = 1),
                 last_run_at      TEXT NOT NULL,
                 archived_memory  INTEGER NOT NULL DEFAULT 0,
                 archived_session INTEGER NOT NULL DEFAULT 0,
                 purged_memory    INTEGER NOT NULL DEFAULT 0,
                 purged_session   INTEGER NOT NULL DEFAULT 0,
                 pruned_convo     INTEGER NOT NULL DEFAULT 0
             );",
        )
        .context("Failed to create hygiene_state table in state.db")?;
        Ok(())
    }

    pub fn migrate_devices_from_standalone(&self, workspace_dir: &Path) -> Result<usize> {
        let standalone = workspace_dir.join("devices.db");
        if !standalone.exists() {
            return Ok(0);
        }

        self.ensure_devices_table()?;
        let conn = self.connect()?;

        let existing: i64 = conn
            .query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))
            .unwrap_or(0);
        if existing > 0 {
            let _ = std::fs::rename(&standalone, standalone.with_extension("db.migrated"));
            return Ok(0);
        }

        let src = Connection::open(&standalone)
            .context("Failed to open standalone devices.db")?;

        let has_table: bool = src
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='devices'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(false);
        if !has_table {
            let _ = std::fs::rename(&standalone, standalone.with_extension("db.migrated"));
            return Ok(0);
        }

        let mut stmt = src.prepare(
            "SELECT token_hash, id, name, device_type, paired_at, last_seen, ip_address FROM devices",
        )?;
        let rows: Vec<(String, String, Option<String>, Option<String>, String, String, Option<String>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
            .filter_map(|r| r.ok())
            .collect();

        let count = rows.len();
        for (token_hash, id, name, device_type, paired_at, last_seen, ip_address) in &rows {
            conn.execute(
                "INSERT OR IGNORE INTO devices (token_hash, id, name, device_type, paired_at, last_seen, ip_address)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![token_hash, id, name, device_type, paired_at, last_seen, ip_address],
            )?;
        }

        if count > 0 {
            tracing::info!("📦 Migrated {count} device(s) from standalone devices.db to state.db");
        }
        let _ = std::fs::rename(&standalone, standalone.with_extension("db.migrated"));
        Ok(count)
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

    #[test]
    fn ensure_tables_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let db = StateDb::open(tmp.path()).unwrap();
        db.ensure_devices_table().unwrap();
        db.ensure_devices_table().unwrap();
        db.ensure_daemon_health_table().unwrap();
        db.ensure_daemon_health_table().unwrap();
        db.ensure_hygiene_state_table().unwrap();
        db.ensure_hygiene_state_table().unwrap();

        let conn = db.connect().unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect()
        };
        assert!(tables.contains(&"devices".to_string()));
        assert!(tables.contains(&"daemon_health".to_string()));
        assert!(tables.contains(&"hygiene_state".to_string()));
    }

    #[test]
    fn migrate_devices_from_standalone_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // Create an empty standalone devices.db
        let standalone = tmp.path().join("devices.db");
        let conn = Connection::open(&standalone).unwrap();
        conn.execute_batch(
            "CREATE TABLE devices (
                 token_hash TEXT PRIMARY KEY, id TEXT NOT NULL, name TEXT,
                 device_type TEXT, paired_at TEXT NOT NULL, last_seen TEXT NOT NULL,
                 ip_address TEXT
             )",
        )
        .unwrap();
        drop(conn);

        let db = StateDb::open(tmp.path()).unwrap();
        let migrated = db.migrate_devices_from_standalone(tmp.path()).unwrap();
        assert_eq!(migrated, 0);
        assert!(!standalone.exists());
        assert!(tmp.path().join("devices.db.migrated").exists());
    }

    #[test]
    fn migrate_devices_from_standalone_with_data() {
        let tmp = tempfile::tempdir().unwrap();
        let standalone = tmp.path().join("devices.db");
        let conn = Connection::open(&standalone).unwrap();
        conn.execute_batch(
            "CREATE TABLE devices (
                 token_hash TEXT PRIMARY KEY, id TEXT NOT NULL, name TEXT,
                 device_type TEXT, paired_at TEXT NOT NULL, last_seen TEXT NOT NULL,
                 ip_address TEXT
             )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO devices VALUES ('hash1', 'dev1', 'Phone', 'mobile', '2026-01-01T00:00:00Z', '2026-06-01T00:00:00Z', '1.2.3.4')",
            [],
        )
        .unwrap();
        drop(conn);

        let db = StateDb::open(tmp.path()).unwrap();
        let migrated = db.migrate_devices_from_standalone(tmp.path()).unwrap();
        assert_eq!(migrated, 1);

        let conn = db.connect().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM devices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Idempotent: second call sees .migrated, returns 0
        let migrated2 = db.migrate_devices_from_standalone(tmp.path()).unwrap();
        assert_eq!(migrated2, 0);
    }
}
