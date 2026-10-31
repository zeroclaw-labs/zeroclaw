//! Rebuildable session discovery index.
//!
//! Event streams remain canonical. This module stores only the latest projected
//! summary required to enumerate and filter sessions quickly; a missing or
//! stale index is rebuilt from committed events.

use super::*;
use crate::session_events::installation::NativeBackend;
use rusqlite::{Connection, params};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;
use zeroclaw_api::session_events::catalog::SessionIndexRecord;

const JSONL_INDEX_FILE: &str = "session-index.jsonl";

pub(crate) fn ensure_sqlite_schema(connection: &Connection) -> Result<(), AppendError> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS session_index (
               stream_id TEXT PRIMARY KEY,
               session_id TEXT,
               creation_source TEXT,
               title TEXT,
               session_key TEXT UNIQUE,
               created_at INTEGER NOT NULL,
               last_activity INTEGER NOT NULL,
               first_user_message_id TEXT,
               first_user_preview TEXT,
               cwd TEXT,
               git_remote TEXT,
               git_branch TEXT,
               git_commit TEXT,
               provider TEXT,
               model TEXT,
               category TEXT,
               project TEXT,
               parent_stream_id TEXT,
               parent_thread_id TEXT,
               parent_agent TEXT,
               owning_agent TEXT,
               closed INTEGER NOT NULL,
               deleted INTEGER NOT NULL,
               head_sequence INTEGER NOT NULL,
               message_count INTEGER NOT NULL,
               running INTEGER NOT NULL,
               metadata_json BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS session_index_activity
               ON session_index(deleted, closed, last_activity DESC);
             CREATE INDEX IF NOT EXISTS session_index_filter
               ON session_index(deleted, category, project, provider, model);",
        )
        .map_err(map_sqlite)?;
    Ok(())
}

pub(crate) fn synchronize<F>(
    data_dir: &Path,
    backend: NativeBackend,
    store: &dyn SessionEventStore,
    streams: &[SessionStreamId],
    project: F,
) -> Result<(), AppendError>
where
    F: Fn(&SessionStreamId) -> Result<Option<SessionIndexRecord>, ReadError>,
{
    match backend {
        NativeBackend::Sqlite => synchronize_sqlite(data_dir, store, streams, project),
        NativeBackend::Jsonl => synchronize_jsonl(data_dir, store, streams, project),
    }
}

pub(crate) fn read_records(
    data_dir: &Path,
    backend: NativeBackend,
) -> Result<Vec<SessionIndexRecord>, ReadError> {
    let mut records = match backend {
        NativeBackend::Sqlite => {
            let store = SqliteEventStore::open(data_dir)
                .map_err(|error| ReadError::StorageUnavailable(error.to_string()))?;
            let connection = store.conn.lock();
            load_sqlite_records(&connection)?
        }
        NativeBackend::Jsonl => load_jsonl_records(
            &data_dir
                .join("sessions/session-events-jsonl")
                .join(JSONL_INDEX_FILE),
        )?,
    }
    .into_values()
    .filter(|record| !record.deleted)
    .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        right
            .last_activity
            .cmp(&left.last_activity)
            .then_with(|| left.stream_id.cmp(&right.stream_id))
    });
    Ok(records)
}

fn synchronize_sqlite<F>(
    data_dir: &Path,
    store: &dyn SessionEventStore,
    streams: &[SessionStreamId],
    project: F,
) -> Result<(), AppendError>
where
    F: Fn(&SessionStreamId) -> Result<Option<SessionIndexRecord>, ReadError>,
{
    let native = SqliteEventStore::open(data_dir)?;
    let connection = native.conn.lock();
    // The table is a disposable projection. If an individual row or its
    // derived metadata cannot be decoded, rebuild the projection from the
    // canonical event streams instead of making the session store unusable.
    let existing = rebuildable(load_sqlite_records(&connection))?;
    let tx = connection.unchecked_transaction().map_err(map_sqlite)?;
    let mut seen = BTreeSet::new();
    for stream in streams {
        seen.insert(stream.as_str().to_owned());
        let head = store
            .head(stream)
            .map_err(crate::session_events::upgrade::storage)?;
        if existing
            .get(stream.as_str())
            .is_some_and(|record| record.head_sequence == head.sequence)
        {
            continue;
        }
        let Some(record) = project(stream).map_err(append_read_error)? else {
            continue;
        };
        upsert_sqlite(&tx, &record)?;
    }
    let mut statement = tx
        .prepare("SELECT stream_id FROM session_index")
        .map_err(map_sqlite)?;
    let stale = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    for stream in stale {
        if !seen.contains(&stream) {
            tx.execute(
                "DELETE FROM session_index WHERE stream_id=?1",
                params![stream],
            )
            .map_err(map_sqlite)?;
        }
    }
    tx.commit().map_err(map_sqlite)
}

fn synchronize_jsonl<F>(
    data_dir: &Path,
    store: &dyn SessionEventStore,
    streams: &[SessionStreamId],
    project: F,
) -> Result<(), AppendError>
where
    F: Fn(&SessionStreamId) -> Result<Option<SessionIndexRecord>, ReadError>,
{
    let path = data_dir
        .join("sessions/session-events-jsonl")
        .join(JSONL_INDEX_FILE);
    let existing = rebuildable(load_jsonl_records(&path))?;
    let mut records = BTreeMap::new();
    for stream in streams {
        if let Some(record) = existing.get(stream.as_str())
            && store.head(stream).map_err(append_read_error)?.sequence == record.head_sequence
        {
            records.insert(stream.as_str().to_owned(), record.clone());
            continue;
        }
        if let Some(record) = project(stream).map_err(append_read_error)? {
            records.insert(stream.as_str().to_owned(), record);
        }
    }
    if records == existing && path.try_exists().map_err(jsonl::storage)? {
        return Ok(());
    }
    let temporary = path.with_extension(format!("jsonl.{}.tmp", EventId::new()));
    let mut file = filesystem::file(&temporary, true).map_err(jsonl::storage)?;
    for record in records.values() {
        let bytes = zeroclaw_api::session_events::canonical_json(record)?;
        file.write_all(&bytes).map_err(jsonl::storage)?;
        file.write_all(b"\n").map_err(jsonl::storage)?;
    }
    file.sync_all().map_err(jsonl::storage)?;
    drop(file);
    fs::rename(&temporary, &path).map_err(jsonl::storage)?;
    filesystem::sync_directory(
        path.parent()
            .ok_or_else(|| AppendError::Corrupt("session index directory missing".into()))?,
    )
}

fn upsert_sqlite(
    tx: &rusqlite::Transaction<'_>,
    record: &SessionIndexRecord,
) -> Result<(), AppendError> {
    let metadata = serde_json::to_vec(&record.metadata)
        .map_err(|error| AppendError::Corrupt(error.to_string()))?;
    tx.execute(
        "INSERT INTO session_index (
            stream_id,session_id,creation_source,title,created_at,last_activity,
            first_user_message_id,first_user_preview,cwd,git_remote,git_branch,git_commit,
            provider,model,category,project,parent_stream_id,parent_thread_id,parent_agent,
            owning_agent,closed,deleted,head_sequence,message_count,running,metadata_json,session_key
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
         ON CONFLICT(stream_id) DO UPDATE SET
            session_id=excluded.session_id,creation_source=excluded.creation_source,title=excluded.title,session_key=excluded.session_key,
            created_at=excluded.created_at,last_activity=excluded.last_activity,
            first_user_message_id=excluded.first_user_message_id,first_user_preview=excluded.first_user_preview,
            cwd=excluded.cwd,git_remote=excluded.git_remote,git_branch=excluded.git_branch,
            git_commit=excluded.git_commit,provider=excluded.provider,model=excluded.model,
            category=excluded.category,project=excluded.project,parent_stream_id=excluded.parent_stream_id,
            parent_thread_id=excluded.parent_thread_id,parent_agent=excluded.parent_agent,
            owning_agent=excluded.owning_agent,closed=excluded.closed,deleted=excluded.deleted,
            head_sequence=excluded.head_sequence,message_count=excluded.message_count,
            running=excluded.running,metadata_json=excluded.metadata_json",
        params![
            record.stream_id.as_str(),
            record.session_id.as_ref().map(ToString::to_string),
            record.creation_source,
            record.title,
            record.created_at,
            record.last_activity,
            record.first_user_message_id.as_ref().map(ToString::to_string),
            record.first_user_preview,
            record.cwd,
            record.git_remote,
            record.git_branch,
            record.git_commit,
            record.provider,
            record.model,
            record.category,
            record.project,
            record.parent_stream_id.as_ref().map(ToString::to_string),
            record.parent_thread_id.as_ref().map(ToString::to_string),
            record.parent_agent,
            record.owning_agent,
            record.closed,
            record.deleted,
            i64::try_from(record.head_sequence).map_err(|_| AppendError::Corrupt("head sequence exceeds SQLite integer".into()))?,
            i64::try_from(record.message_count).map_err(|_| AppendError::Corrupt("message count exceeds SQLite integer".into()))?,
            record.running,
            metadata,
            record.session_key,
        ],
    )
    .map_err(map_sqlite)?;
    Ok(())
}

fn load_sqlite_records(
    connection: &Connection,
) -> Result<BTreeMap<String, SessionIndexRecord>, ReadError> {
    let table_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_index')",
            [],
            |row| row.get(0),
        )
        .map_err(read_error)?;
    if !table_exists {
        return Ok(BTreeMap::new());
    }
    let mut statement = connection
        .prepare("SELECT stream_id,session_id,creation_source,title,created_at,last_activity,first_user_message_id,first_user_preview,cwd,git_remote,git_branch,git_commit,provider,model,category,project,parent_stream_id,parent_thread_id,parent_agent,owning_agent,closed,deleted,head_sequence,message_count,running,metadata_json,session_key FROM session_index")
        .map_err(read_error)?;
    let rows = statement
        .query_map([], |row| {
            let metadata: Vec<u8> = row.get(25)?;
            let head: i64 = row.get(22)?;
            let message_count: i64 = row.get(23)?;
            let head_sequence = u64::try_from(head)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(22, head))?;
            let message_count = usize::try_from(message_count)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(23, message_count))?;
            Ok(SessionIndexRecord {
                stream_id: zeroclaw_api::session_events::SessionStreamId::parse(
                    row.get::<_, String>(0)?,
                )
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                session_id: parse_optional(row.get(1)?),
                session_key: row.get(26)?,
                creation_source: row.get(2)?,
                title: row.get(3)?,
                created_at: row.get(4)?,
                last_activity: row.get(5)?,
                first_user_message_id: parse_optional(row.get(6)?),
                first_user_preview: row.get(7)?,
                cwd: row.get(8)?,
                git_remote: row.get(9)?,
                git_branch: row.get(10)?,
                git_commit: row.get(11)?,
                provider: row.get(12)?,
                model: row.get(13)?,
                category: row.get(14)?,
                project: row.get(15)?,
                parent_stream_id: parse_optional(row.get(16)?),
                parent_thread_id: parse_optional(row.get(17)?),
                parent_agent: row.get(18)?,
                owning_agent: row.get(19)?,
                closed: row.get(20)?,
                deleted: row.get(21)?,
                head_sequence,
                message_count,
                running: row.get(24)?,
                metadata: serde_json::from_slice(&metadata).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        25,
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?,
            })
        })
        .map_err(read_error)?;
    let mut out = BTreeMap::new();
    for row in rows {
        let record = row.map_err(read_error)?;
        out.insert(record.stream_id.as_str().to_owned(), record);
    }
    Ok(out)
}

fn parse_optional<T>(value: Option<String>) -> Option<T>
where
    T: From<String>,
{
    value.map(T::from)
}

fn load_jsonl_records(path: &Path) -> Result<BTreeMap<String, SessionIndexRecord>, ReadError> {
    let bytes = match filesystem::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(read_error_io(error)),
    };
    let mut out = BTreeMap::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let record: SessionIndexRecord =
            serde_json::from_slice(line).map_err(|error| ReadError::Corrupt(error.to_string()))?;
        out.insert(record.stream_id.as_str().to_owned(), record);
    }
    Ok(out)
}

fn read_error_io(error: std::io::Error) -> ReadError {
    ReadError::StorageUnavailable(error.to_string())
}

fn map_sqlite(error: rusqlite::Error) -> AppendError {
    AppendError::StorageUnavailable(error.to_string())
}

fn append_read_error(error: ReadError) -> AppendError {
    match error {
        ReadError::Corrupt(message) => AppendError::Corrupt(message),
        ReadError::UnsupportedSchema(message) => AppendError::UnsupportedSchema(message),
        ReadError::StorageUnavailable(message) => AppendError::StorageUnavailable(message),
        other => AppendError::Corrupt(other.to_string()),
    }
}

fn read_error(error: rusqlite::Error) -> ReadError {
    match error {
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::InvalidColumnType(..) => ReadError::Corrupt(error.to_string()),
        _ => ReadError::StorageUnavailable(error.to_string()),
    }
}

fn rebuildable<T: Default>(result: Result<T, ReadError>) -> Result<T, AppendError> {
    match result {
        Ok(records) => Ok(records),
        Err(ReadError::Corrupt(_)) => Ok(T::default()),
        Err(error) => Err(crate::session_events::upgrade::storage(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_events::catalog::SessionCatalog;
    use crate::session_events::installation::NativeBackend;
    use crate::session_events::scoped::EventStorePool;
    use crate::session_events::upgrade::upgrade_installation;
    use serde_json::json;

    #[test]
    fn sqlite_index_is_rebuilt_from_canonical_events() {
        let directory = tempfile::tempdir().expect("directory");
        upgrade_installation(directory.path(), NativeBackend::Sqlite, |_| "agent".into())
            .expect("upgrade");
        let pool = EventStorePool::default();
        let catalog = SessionCatalog::open(directory.path(), &pool)
            .expect("catalog")
            .expect("native");
        catalog
            .commands()
            .create(
                "gw_indexed",
                "gateway",
                "agent",
                vec![("cwd", json!("/workspace")), ("category", json!("project"))],
            )
            .expect("create");
        let sources = catalog.query().sessions().expect("sources");
        assert!(sources.contains_key("gw_indexed"));
        let connection =
            Connection::open(directory.path().join("sessions/session-events.db")).expect("db");
        let row: (String, String, String) = connection
            .query_row(
                "SELECT session_key,cwd,category FROM session_index WHERE stream_id=?1",
                params![sources["gw_indexed"].as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("index row");
        assert_eq!(
            row,
            ("gw_indexed".into(), "/workspace".into(), "project".into())
        );
        let session = catalog
            .query()
            .summary(&sources["gw_indexed"])
            .expect("summary")
            .expect("session");
        catalog
            .commands()
            .change(&session, vec![("title", json!("Renamed session"))])
            .expect("rename");
        let rows = catalog.query().index_records().expect("updated index");
        assert_eq!(rows[0].creation_source.as_deref(), Some("gateway"));
        assert_eq!(rows[0].title.as_deref(), Some("Renamed session"));
        let columns = connection
            .prepare("PRAGMA table_info(session_index)")
            .expect("schema")
            .query_map([], |row| row.get::<_, String>(1))
            .expect("columns")
            .collect::<Result<Vec<_>, _>>()
            .expect("column names");
        assert!(columns.iter().any(|column| column == "title"));
        assert!(!columns.iter().any(|column| column == "name"));
        assert_eq!(
            catalog
                .query()
                .indexed_summaries()
                .expect("indexed summary")[0]
                .1
                .title(),
            Some("Renamed session")
        );
        connection
            .execute("UPDATE session_index SET metadata_json=x'ff'", [])
            .expect("corrupt disposable row");
        assert_eq!(
            catalog.query().index_records().expect("rebuilt index"),
            rows
        );
    }

    #[test]
    fn jsonl_index_uses_the_same_logical_rows() {
        let directory = tempfile::tempdir().expect("directory");
        upgrade_installation(directory.path(), NativeBackend::Jsonl, |_| "agent".into())
            .expect("upgrade");
        let pool = EventStorePool::default();
        let catalog = SessionCatalog::open(directory.path(), &pool)
            .expect("catalog")
            .expect("native");
        catalog
            .commands()
            .create(
                "gw_jsonl",
                "gateway",
                "agent",
                vec![("project", json!("demo"))],
            )
            .expect("create");
        let sources = catalog.query().sessions().expect("sources");
        assert_eq!(sources.len(), 1);
        let bytes = fs::read(
            directory
                .path()
                .join("sessions/session-events-jsonl/session-index.jsonl"),
        )
        .expect("index");
        assert!(
            String::from_utf8(bytes)
                .expect("utf8")
                .contains("\"session_key\":\"gw_jsonl\"")
        );
        let index = directory
            .path()
            .join("sessions/session-events-jsonl/session-index.jsonl");
        fs::write(&index, "interrupted index write\n").expect("corrupt disposable index");
        let rebuilt = catalog.query().index_records().expect("rebuilt index");
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].creation_source.as_deref(), Some("gateway"));
        fs::remove_file(&index).expect("remove index");
        fs::create_dir(&index).expect("block index read");
        assert!(matches!(
            catalog.query().index_records(),
            Err(ReadError::StorageUnavailable(_))
        ));
    }

    #[test]
    fn legacy_import_indexes_first_user_message_on_each_backend() {
        use crate::session_backend::SessionBackend;
        use crate::session_events::upgrade::legacy_sqlite::SqliteSessionBackend;
        use zeroclaw_api::model_provider::ChatMessage;

        for backend in [NativeBackend::Sqlite, NativeBackend::Jsonl] {
            let directory = tempfile::tempdir().expect("directory");
            let legacy = SqliteSessionBackend::new(directory.path()).expect("legacy");
            legacy
                .append("gw_import", &ChatMessage::assistant("earlier assistant"))
                .expect("assistant");
            legacy
                .append("gw_import", &ChatMessage::user("首条用户消息"))
                .expect("user");
            legacy
                .set_session_title("gw_import", "Migrated title")
                .expect("legacy title");
            drop(legacy);
            upgrade_installation(directory.path(), backend, |_| "agent".into()).expect("upgrade");
            let pool = EventStorePool::default();
            let catalog = SessionCatalog::open(directory.path(), &pool)
                .expect("catalog")
                .expect("native");
            let rows = catalog.query().index_records().expect("migrated index");
            assert_eq!(rows.len(), 1);
            assert!(rows[0].first_user_message_id.is_some());
            assert_eq!(rows[0].first_user_preview.as_deref(), Some("首条用户消息"));
            assert_eq!(rows[0].creation_source.as_deref(), Some("legacy"));
            assert_eq!(rows[0].title.as_deref(), Some("Migrated title"));
            let session = catalog
                .query()
                .summary(&rows[0].stream_id)
                .expect("summary")
                .expect("session");
            assert_eq!(session.title(), Some("Migrated title"));
            assert!(!session.fields.contains_key("name"));
            assert_eq!(rows[0].message_count, 2);
        }
    }
}
