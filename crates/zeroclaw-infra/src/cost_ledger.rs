//! Backend-native append-only ledger for model usage and cost records.
//!
//! The ledger follows the installation's selected session backend. It is kept
//! separate from the session index and event stream because cost rows are a
//! billing ledger, not replayable session state.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const JSONL_FILE: &str = "costs.jsonl";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Sqlite,
    Jsonl,
}

pub fn resolve_path(workspace_dir: &Path, session_backend: &str) -> Result<PathBuf> {
    let sessions = workspace_dir.join("sessions");
    let manifest = sessions.join("session-format.json");
    let backend = if manifest.exists() {
        let value: serde_json::Value = serde_json::from_slice(
            &fs::read(&manifest).with_context(|| format!("read {}", manifest.display()))?,
        )?;
        match value.get("phase").and_then(serde_json::Value::as_str) {
            Some("ready") => match value.get("backend").and_then(serde_json::Value::as_str) {
                Some("sqlite") => Backend::Sqlite,
                Some("jsonl") => Backend::Jsonl,
                _ => bail!("session format manifest has no supported backend"),
            },
            Some(phase) => bail!("session format is not ready: {phase}"),
            None => bail!("session format manifest has no phase"),
        }
    } else {
        // A tracker can precede lazy session-host initialization. Select the
        // configured backend immediately so its live handles keep the same
        // ledger when the installation manifest is published.
        match crate::session_events::installation::NativeBackend::parse(session_backend)? {
            crate::session_events::installation::NativeBackend::Sqlite => Backend::Sqlite,
            crate::session_events::installation::NativeBackend::Jsonl => Backend::Jsonl,
        }
    };
    Ok(match backend {
        Backend::Sqlite => sessions.join("session-events.db"),
        Backend::Jsonl => sessions.join("session-events-jsonl").join(JSONL_FILE),
    })
}

pub struct CostLedger {
    path: PathBuf,
    backend: Backend,
}

pub enum AppendOutcome {
    Synced,
    AppendedButSyncFailed(anyhow::Error),
}

impl CostLedger {
    pub fn open(path: &Path) -> Result<Self> {
        let backend = if path
            .file_name()
            .is_some_and(|name| name == "session-events.db")
        {
            Backend::Sqlite
        } else {
            Backend::Jsonl
        };
        Self::open_backend(path, backend, true)
    }

    pub fn open_for_backend(
        workspace_dir: &Path,
        backend: crate::session_events::installation::NativeBackend,
    ) -> Result<Self> {
        let (path, backend) = match backend {
            crate::session_events::installation::NativeBackend::Sqlite => (
                workspace_dir.join("sessions/session-events.db"),
                Backend::Sqlite,
            ),
            crate::session_events::installation::NativeBackend::Jsonl => (
                workspace_dir
                    .join("sessions/session-events-jsonl")
                    .join(JSONL_FILE),
                Backend::Jsonl,
            ),
        };
        Self::open_backend(&path, backend, true)
    }

    fn open_backend(path: &Path, backend: Backend, migrate: bool) -> Result<Self> {
        let ledger = Self {
            path: path.to_path_buf(),
            backend,
        };
        ledger.initialize()?;
        if migrate {
            ledger.migrate_legacy()?;
        }
        Ok(ledger)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn initialize(&self) -> Result<()> {
        match self.backend {
            Backend::Jsonl => {
                if let Some(parent) = self.path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("create {}", parent.display()))?;
                }
                if !self.path.exists() {
                    File::create(&self.path)
                        .with_context(|| format!("create {}", self.path.display()))?
                        .sync_all()?;
                }
            }
            Backend::Sqlite => {
                let workspace = self.workspace_dir()?;
                // This also creates the canonical event schema and applies its
                // SQLite publication settings.
                crate::session_events::SqliteEventStore::open(workspace)
                    .map_err(|error| anyhow::Error::msg(error.to_string()))?;
                let conn = Connection::open(&self.path)?;
                conn.execute_batch(
                    "CREATE TABLE IF NOT EXISTS session_cost_records(
                       record_id TEXT PRIMARY KEY,
                       session_id TEXT,
                       agent_alias TEXT,
                       task_id TEXT,
                       model TEXT,
                       usage_timestamp TEXT,
                       input_tokens INTEGER,
                       output_tokens INTEGER,
                       cached_input_tokens INTEGER,
                       cache_creation_input_tokens INTEGER,
                       total_tokens INTEGER,
                       unpriced_tokens INTEGER,
                       cost_usd REAL,
                       pricing_available INTEGER,
                       record_json BLOB NOT NULL
                     );
                     CREATE INDEX IF NOT EXISTS session_cost_records_session
                       ON session_cost_records(session_id);
                     CREATE INDEX IF NOT EXISTS session_cost_records_timestamp
                       ON session_cost_records(usage_timestamp);",
                )?;
            }
        }
        Ok(())
    }

    fn migrate_legacy(&self) -> Result<()> {
        let workspace = self.workspace_dir()?;
        let candidates = [
            workspace.join("state/costs.jsonl"),
            workspace.join(".zeroclaw/costs.db"),
        ];
        if self.has_records()? {
            return Ok(());
        }
        for source in candidates {
            if !source.exists() || source == self.path {
                continue;
            }
            let bytes = fs::read(&source)
                .with_context(|| format!("read legacy cost ledger {}", source.display()))?;
            for line in bytes.split(|byte| *byte == b'\n') {
                if !line.iter().all(u8::is_ascii_whitespace) {
                    for value in
                        serde_json::Deserializer::from_slice(line).into_iter::<serde_json::Value>()
                    {
                        let mut value = value?;
                        // Master's session_id is a tracker UUID. Only its
                        // conversation_id can establish actual session ownership.
                        let fields = value
                            .as_object_mut()
                            .context("legacy cost record is not an object")?;
                        fields.remove("session_id");
                        if let Some(session) = fields.remove("conversation_id") {
                            fields.insert("session_id".into(), session);
                        }
                        let raw = serde_json::to_vec(&value)?;
                        match self.append(&raw, File::sync_all)? {
                            AppendOutcome::Synced => {}
                            AppendOutcome::AppendedButSyncFailed(error) => return Err(error),
                        }
                    }
                }
            }
            archive_legacy(&source)?;
            break;
        }
        Ok(())
    }

    fn has_records(&self) -> Result<bool> {
        match self.backend {
            Backend::Jsonl => Ok(self.path.metadata()?.len() > 0),
            Backend::Sqlite => {
                let conn = Connection::open(&self.path)?;
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_cost_records)",
                    [],
                    |row| row.get(0),
                )?)
            }
        }
    }

    pub fn read_records(&self) -> Result<Vec<Vec<u8>>> {
        match self.backend {
            Backend::Jsonl => {
                let file = File::open(&self.path)?;
                BufReader::new(file)
                    .lines()
                    .map(|line| line.map(|value| value.into_bytes()).map_err(Into::into))
                    .collect()
            }
            Backend::Sqlite => {
                let conn = Connection::open(&self.path)?;
                let mut statement =
                    conn.prepare("SELECT record_json FROM session_cost_records ORDER BY rowid")?;
                let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            }
        }
    }

    pub fn import_records(&self, records: &[Vec<u8>]) -> Result<()> {
        let mut existing = self
            .read_records()?
            .into_iter()
            .filter_map(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
            .filter_map(|value| {
                value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .collect::<HashSet<_>>();
        for raw in records {
            let id = serde_json::from_slice::<serde_json::Value>(raw)?
                .get("id")
                .and_then(serde_json::Value::as_str)
                .context("cost record has no id")?
                .to_owned();
            if existing.contains(&id) {
                continue;
            }
            match self.append(raw, File::sync_all)? {
                AppendOutcome::Synced => {}
                AppendOutcome::AppendedButSyncFailed(error) => return Err(error),
            }
            existing.insert(id);
        }
        Ok(())
    }

    pub fn append(
        &self,
        record_json: &[u8],
        sync_file: fn(&File) -> std::io::Result<()>,
    ) -> Result<AppendOutcome> {
        match self.backend {
            Backend::Jsonl => {
                let mut file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
                    .with_context(|| format!("open {}", self.path.display()))?;
                file.write_all(record_json)?;
                file.write_all(b"\n")?;
                if let Err(error) = sync_file(&file).context("Failed to sync cost storage") {
                    return Ok(AppendOutcome::AppendedButSyncFailed(error));
                }
            }
            Backend::Sqlite => {
                let value: serde_json::Value = serde_json::from_slice(record_json)?;
                let record_id = value
                    .get("id")
                    .and_then(serde_json::Value::as_str)
                    .context("cost record has no id")?;
                let usage = value.get("usage").cloned().unwrap_or_default();
                let text = |object: &serde_json::Value, key: &str| {
                    object
                        .get(key)
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                };
                let integer = |object: &serde_json::Value, key: &str| {
                    object.get(key).and_then(serde_json::Value::as_u64)
                };
                let cost = usage.get("cost_usd").and_then(serde_json::Value::as_f64);
                let pricing = usage
                    .get("pricing_available")
                    .and_then(serde_json::Value::as_bool)
                    .map(i64::from);
                let conn = Connection::open(&self.path)?;
                conn.execute(
                    "INSERT OR IGNORE INTO session_cost_records(
                       record_id, session_id, agent_alias, task_id,
                       model, usage_timestamp, input_tokens, output_tokens,
                       cached_input_tokens, cache_creation_input_tokens, total_tokens,
                       unpriced_tokens, cost_usd, pricing_available, record_json
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
                    params![
                        record_id,
                        text(&value, "session_id"),
                        text(&value, "agent_alias"),
                        text(&value, "task_id"),
                        text(&usage, "model"),
                        text(&usage, "timestamp"),
                        integer(&usage, "input_tokens"),
                        integer(&usage, "output_tokens"),
                        integer(&usage, "cached_input_tokens"),
                        integer(&usage, "cache_creation_input_tokens"),
                        integer(&usage, "total_tokens"),
                        integer(&usage, "unpriced_tokens"),
                        cost,
                        pricing,
                        record_json,
                    ],
                )?;
            }
        }
        Ok(AppendOutcome::Synced)
    }

    fn workspace_dir(&self) -> Result<&Path> {
        match self.backend {
            Backend::Sqlite => self
                .path
                .parent()
                .and_then(Path::parent)
                .context("session database parent missing"),
            Backend::Jsonl => self
                .path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .context("cost ledger workspace missing"),
        }
    }
}

fn archive_legacy(path: &Path) -> Result<()> {
    let mut backup = path.as_os_str().to_os_string();
    backup.push(".backup");
    let mut candidate = PathBuf::from(&backup);
    let mut index = 1;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{}.{}", backup.to_string_lossy(), index));
        index += 1;
    }
    fs::rename(path, candidate)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn record(id: &str) -> Vec<u8> {
        serde_json::json!({
            "id": id,
            "usage": {
                "model": "test/model",
                "input_tokens": 1,
                "output_tokens": 1,
                "cached_input_tokens": 0,
                "cache_creation_input_tokens": 0,
                "total_tokens": 2,
                "cost_usd": 0.1,
                "pricing_available": true,
                "unpriced_tokens": 0,
                "timestamp": "2026-11-03T00:00:00Z"
            },
            "session_id": "session"
        })
        .to_string()
        .into_bytes()
    }

    fn legacy_record(id: &str, session_id: Option<&str>) -> Vec<u8> {
        let mut value: serde_json::Value = serde_json::from_slice(&record(id)).unwrap();
        value["session_id"] = "old-tracker".into();
        if let Some(session) = session_id {
            value["conversation_id"] = session.into();
        }
        serde_json::to_vec(&value).unwrap()
    }

    #[test]
    fn jsonl_ledger_round_trips_and_migrates_legacy_file() {
        let dir = TempDir::new().unwrap();
        let legacy = dir.path().join("state/costs.jsonl");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        let original = [
            legacy_record("a", None),
            legacy_record("b", Some("session")),
        ]
        .join(&b'\n');
        fs::write(&legacy, &original).unwrap();
        let path = dir.path().join("sessions/session-events-jsonl/costs.jsonl");
        let ledger = CostLedger::open(&path).unwrap();
        let records = ledger.read_records().unwrap();
        assert_eq!(records.len(), 2);
        for (index, record) in records.iter().enumerate() {
            let value: serde_json::Value = serde_json::from_slice(record).unwrap();
            if index == 0 {
                assert!(value.get("session_id").is_none());
            } else {
                assert_eq!(value["session_id"], "session");
            }
            assert!(value.get("conversation_id").is_none());
            assert_eq!(value["usage"]["cost_usd"], 0.1);
        }
        assert!(!legacy.exists());
        assert_eq!(
            fs::read(dir.path().join("state/costs.jsonl.backup")).unwrap(),
            original
        );
        ledger.append(&record("c"), File::sync_all).unwrap();
        let records = ledger.read_records().unwrap();
        assert_eq!(records.len(), 3);
        let value: serde_json::Value = serde_json::from_slice(&records[2]).unwrap();
        assert_eq!(value["session_id"], "session");
    }

    #[test]
    fn sqlite_ledger_uses_a_separate_table() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sessions/session-events.db");
        let ledger = CostLedger::open(&path).unwrap();
        ledger.append(&record("a"), File::sync_all).unwrap();
        assert_eq!(ledger.read_records().unwrap().len(), 1);
        let conn = Connection::open(path).unwrap();
        let table: String = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type='table' AND name='session_cost_records'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table, "session_cost_records");
        let session: String = conn
            .query_row("SELECT session_id FROM session_cost_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(session, "session");
        let mut statement = conn
            .prepare("PRAGMA table_info(session_cost_records)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            columns
                .iter()
                .filter(|name| name.ends_with("session_id"))
                .count(),
            1
        );
    }

    #[test]
    fn imported_records_are_idempotent_across_backends() {
        let dir = TempDir::new().unwrap();
        let source = CostLedger::open_for_backend(
            dir.path(),
            crate::session_events::installation::NativeBackend::Sqlite,
        )
        .unwrap();
        source.append(&record("a"), File::sync_all).unwrap();
        let target = CostLedger::open_for_backend(
            dir.path(),
            crate::session_events::installation::NativeBackend::Jsonl,
        )
        .unwrap();
        let records = source.read_records().unwrap();
        target.import_records(&records).unwrap();
        target.import_records(&records).unwrap();
        assert_eq!(target.read_records().unwrap(), records);
    }

    #[test]
    fn sqlite_backend_migrates_legacy_jsonl_and_archives_it() {
        let dir = TempDir::new().unwrap();
        let legacy = dir.path().join("state/costs.jsonl");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(
            &legacy,
            [
                legacy_record("legacy", None),
                legacy_record("attributed", Some("session")),
            ]
            .join(&b'\n'),
        )
        .unwrap();
        let ledger = CostLedger::open_for_backend(
            dir.path(),
            crate::session_events::installation::NativeBackend::Sqlite,
        )
        .unwrap();
        let records = ledger.read_records().unwrap();
        assert_eq!(records.len(), 2);
        let value: serde_json::Value = serde_json::from_slice(&records[0]).unwrap();
        assert!(value.get("session_id").is_none());
        assert_eq!(value["id"], "legacy");
        assert_eq!(value["usage"]["cost_usd"], 0.1);
        let value: serde_json::Value = serde_json::from_slice(&records[1]).unwrap();
        assert_eq!(value["session_id"], "session");
        assert!(value.get("conversation_id").is_none());
        let conn = Connection::open(ledger.path()).unwrap();
        let session: Option<String> = conn
            .query_row(
                "SELECT session_id FROM session_cost_records WHERE record_id='legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(session.is_none());
        let session: String = conn
            .query_row(
                "SELECT session_id FROM session_cost_records WHERE record_id='attributed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session, "session");
        assert!(!legacy.exists());
        assert!(dir.path().join("state/costs.jsonl.backup").exists());
    }
}
