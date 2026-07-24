//! PostgreSQL-backed session persistence.
//!
//! The backend stores ordered messages in `sessions`; that table is the source
//! of truth for message content and timestamps. `session_metadata` is the source
//! of truth for names, routing attribution, activity counters, and turn state.
//! Connections are synchronous and pooled with `r2d2`.

use std::fmt::Display;
use std::path::Path;

use chrono::{DateTime, Utc};
use postgres::{NoTls, Row};
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;
use zeroclaw_api::model_provider::ChatMessage;

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState, TimestampedMessage,
};

type PgManager = PostgresConnectionManager<NoTls>;
type PgPool = Pool<PgManager>;

/// Keeps the final PostgreSQL pool drop off a Tokio runtime thread.
///
/// `postgres::Client::drop` drains its connection with an internal Tokio
/// runtime. The session API is already called through `spawn_blocking`, but the
/// shared backend handle itself can be released by an async owner.
struct DropOnThread<T: Send + 'static>(Option<T>);

impl<T: Send + 'static> DropOnThread<T> {
    fn new(value: T) -> Self {
        Self(Some(value))
    }
}

impl<T: Send + 'static> Drop for DropOnThread<T> {
    fn drop(&mut self) {
        let Some(value) = self.0.take() else {
            return;
        };
        let slot = std::mem::ManuallyDrop::new(value);
        if std::thread::Builder::new()
            .name("postgres-session-pool-drop".to_string())
            .spawn(move || drop(std::mem::ManuallyDrop::into_inner(slot)))
            .is_err()
        {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "postgres-session-pool-drop thread spawn failed; leaking pool to avoid nested-runtime panic"
            );
        }
    }
}

/// Synchronous PostgreSQL session store backed by an `r2d2` connection pool.
pub struct PostgresSessionBackend {
    pool: DropOnThread<PgPool>,
}

impl PostgresSessionBackend {
    /// Construct the backend from the canonical environment-backed connection
    /// URL and initialize its schema.
    pub fn new(workspace_dir: &Path, pool_size: u32) -> std::io::Result<Self> {
        let _ = workspace_dir;
        let url = read_postgres_url()?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session_backend=postgres requires ZEROCLAW_channels__postgres_url \
                 (or ZEROCLAW_TEST_POSTGRES_URL in tests) to be set in the \
                 environment. Populate `channels.postgres_url` or inject it \
                 through the standard dotted-path environment override.",
            )
        })?;
        Self::new_with_url(&url, pool_size)
    }

    fn new_with_url(database_url: &str, pool_size: u32) -> std::io::Result<Self> {
        let config = database_url.parse().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("session PostgreSQL URL is invalid: {error}"),
            )
        })?;
        let manager = PostgresConnectionManager::new(config, NoTls);
        let pool = Pool::builder()
            .max_size(pool_size.max(1))
            .build(manager)
            .map_err(|error| pg_error("build connection pool", error))?;
        let backend = Self {
            pool: DropOnThread::new(pool),
        };
        backend.ensure_schema()?;
        Ok(backend)
    }

    fn conn(&self) -> std::io::Result<PooledConnection<PgManager>> {
        self.pool
            .0
            .as_ref()
            .ok_or_else(|| std::io::Error::other("session PostgreSQL pool is unavailable"))?
            .get()
            .map_err(|error| pg_error("checkout pooled connection", error))
    }

    fn ensure_schema(&self) -> std::io::Result<()> {
        let mut conn = self.conn()?;
        conn.batch_execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          BIGSERIAL   PRIMARY KEY,
                session_key TEXT        NOT NULL,
                role        TEXT        NOT NULL,
                content     TEXT        NOT NULL,
                created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_key
                 ON sessions(session_key);
             CREATE INDEX IF NOT EXISTS idx_sessions_key_id
                 ON sessions(session_key, id);
             CREATE INDEX IF NOT EXISTS idx_sessions_content_fts
                 ON sessions USING GIN (to_tsvector('simple', content));

             CREATE TABLE IF NOT EXISTS session_metadata (
                session_key      TEXT        PRIMARY KEY,
                created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_activity    TIMESTAMPTZ NOT NULL DEFAULT now(),
                message_count    BIGINT      NOT NULL DEFAULT 0,
                name             TEXT,
                state            TEXT        NOT NULL DEFAULT 'idle',
                turn_id          TEXT,
                turn_started_at  TIMESTAMPTZ,
                agent_alias      TEXT,
                channel_id       TEXT,
                room_id          TEXT,
                sender_id        TEXT
             );
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS name TEXT;
             ALTER TABLE session_metadata
                 ADD COLUMN IF NOT EXISTS state TEXT NOT NULL DEFAULT 'idle';
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS turn_id TEXT;
             ALTER TABLE session_metadata
                 ADD COLUMN IF NOT EXISTS turn_started_at TIMESTAMPTZ;
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS agent_alias TEXT;
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS channel_id TEXT;
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS room_id TEXT;
             ALTER TABLE session_metadata ADD COLUMN IF NOT EXISTS sender_id TEXT;
             CREATE INDEX IF NOT EXISTS idx_session_metadata_agent_alias
                 ON session_metadata(agent_alias);
             CREATE INDEX IF NOT EXISTS idx_session_metadata_channel_id
                 ON session_metadata(channel_id);
             CREATE INDEX IF NOT EXISTS idx_session_metadata_room_id
                 ON session_metadata(room_id);
             CREATE INDEX IF NOT EXISTS idx_session_metadata_sender_id
                 ON session_metadata(sender_id);",
        )
        .map_err(|error| pg_error("initialize schema", error))
    }

    fn row_to_metadata(row: &Row) -> SessionMetadata {
        let count: i64 = row.get("message_count");
        let message_count = usize::try_from(count.max(0)).unwrap_or(usize::MAX);
        SessionMetadata {
            key: row.get("session_key"),
            name: row.get("name"),
            created_at: row.get("created_at"),
            last_activity: row.get("last_activity"),
            message_count,
            agent_alias: row.get("agent_alias"),
            channel_id: row.get("channel_id"),
            room_id: row.get("room_id"),
            sender_id: row.get("sender_id"),
        }
    }

    fn metadata_columns() -> &'static str {
        "session_key, name, created_at, last_activity, message_count, \
         agent_alias, channel_id, room_id, sender_id"
    }
}

fn pg_error(context: &str, error: impl Display) -> std::io::Error {
    std::io::Error::other(format!(
        "session_backend=postgres: failed to {context}: {error}"
    ))
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn read_postgres_url() -> std::io::Result<Option<String>> {
    if let Ok(value) = std::env::var("ZEROCLAW_channels__postgres_url") {
        if value.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ZEROCLAW_channels__postgres_url is set but empty; provide a postgres:// URL.",
            ));
        }
        return Ok(Some(value));
    }
    if let Ok(value) = std::env::var("ZEROCLAW_TEST_POSTGRES_URL") {
        if value.trim().is_empty() {
            return Ok(None);
        }
        return Ok(Some(value));
    }
    Ok(None)
}

pub(crate) fn read_pool_size() -> u32 {
    std::env::var("ZEROCLAW_channels__pool_size")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(5)
}

fn build_tsquery(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .filter_map(|token| {
            let lexeme: String = token
                .chars()
                .filter(|character| character.is_alphanumeric() || *character == '_')
                .flat_map(char::to_lowercase)
                .collect();
            (!lexeme.is_empty()).then(|| format!("{lexeme}:*"))
        })
        .collect::<Vec<_>>()
        .join(" & ")
}

impl SessionBackend for PostgresSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content FROM sessions WHERE session_key = $1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| ChatMessage {
                role: row.get("role"),
                content: row.get("content"),
            })
            .collect()
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content, created_at FROM sessions \
             WHERE session_key = $1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| TimestampedMessage {
                message: ChatMessage {
                    role: row.get("role"),
                    content: row.get("content"),
                },
                created_at: Some(row.get("created_at")),
            })
            .collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin append transaction", error))?;
        let now = Utc::now();
        tx.execute(
            "INSERT INTO sessions (session_key, role, content, created_at) \
             VALUES ($1, $2, $3, $4)",
            &[&session_key, &message.role, &message.content, &now],
        )
        .map_err(|error| pg_error("append message", error))?;
        tx.execute(
            "INSERT INTO session_metadata \
                 (session_key, created_at, last_activity, message_count) \
             VALUES ($1, $2, $3, 1) \
             ON CONFLICT (session_key) DO UPDATE SET \
                 last_activity = EXCLUDED.last_activity, \
                 message_count = session_metadata.message_count + 1",
            &[&session_key, &now, &now],
        )
        .map_err(|error| pg_error("update metadata after append", error))?;
        tx.commit()
            .map_err(|error| pg_error("commit append transaction", error))
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin remove-last transaction", error))?;
        let removed = tx
            .query_opt(
                "DELETE FROM sessions WHERE id = ( \
                     SELECT id FROM sessions WHERE session_key = $1 \
                     ORDER BY id DESC LIMIT 1 \
                 ) RETURNING id",
                &[&session_key],
            )
            .map_err(|error| pg_error("remove last message", error))?
            .is_some();
        if removed {
            tx.execute(
                "UPDATE session_metadata SET \
                     message_count = GREATEST(message_count - 1, 0), \
                     last_activity = now() \
                 WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("update metadata after remove", error))?;
        }
        tx.commit()
            .map_err(|error| pg_error("commit remove-last transaction", error))?;
        Ok(removed)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin update-last transaction", error))?;
        let updated = tx
            .execute(
                "UPDATE sessions SET role = $1, content = $2 WHERE id = ( \
                     SELECT id FROM sessions WHERE session_key = $3 \
                     ORDER BY id DESC LIMIT 1 \
                 )",
                &[&message.role, &message.content, &session_key],
            )
            .map_err(|error| pg_error("update last message", error))?
            > 0;
        if updated {
            tx.execute(
                "UPDATE session_metadata SET last_activity = now() WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("update metadata after message update", error))?;
        }
        tx.commit()
            .map_err(|error| pg_error("commit update-last transaction", error))?;
        Ok(updated)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT session_key FROM session_metadata ORDER BY last_activity DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows.into_iter().map(|row| row.get(0)).collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let query = format!(
            "SELECT {} FROM session_metadata ORDER BY last_activity DESC",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&query, &[]) else {
            return Vec::new();
        };
        rows.iter().map(Self::row_to_metadata).collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::hours(i64::from(ttl_hours));
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin stale-session cleanup transaction", error))?;
        let keys: Vec<String> = tx
            .query(
                "SELECT session_key FROM session_metadata WHERE last_activity < $1 FOR UPDATE",
                &[&cutoff],
            )
            .map_err(|error| pg_error("select stale sessions", error))?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        for key in &keys {
            tx.execute("DELETE FROM sessions WHERE session_key = $1", &[key])
                .map_err(|error| pg_error("delete stale session messages", error))?;
            tx.execute(
                "DELETE FROM session_metadata WHERE session_key = $1",
                &[key],
            )
            .map_err(|error| pg_error("delete stale session metadata", error))?;
        }
        tx.commit()
            .map_err(|error| pg_error("commit stale-session cleanup transaction", error))?;
        Ok(keys.len())
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = query.keyword.as_deref() else {
            return self.list_sessions_with_metadata();
        };
        let tsquery = build_tsquery(keyword);
        if tsquery.is_empty() {
            return Vec::new();
        }
        let limit = i64::try_from(query.limit.unwrap_or(50)).unwrap_or(i64::MAX);
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "SELECT {} FROM session_metadata m \
             WHERE EXISTS ( \
                 SELECT 1 FROM sessions s \
                 WHERE s.session_key = m.session_key \
                   AND to_tsvector('simple', s.content) @@ to_tsquery('simple', $1) \
             ) \
             ORDER BY m.last_activity DESC LIMIT $2",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&sql, &[&tsquery, &limit]) else {
            return Vec::new();
        };
        rows.iter().map(Self::row_to_metadata).collect()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin clear-messages transaction", error))?;
        let removed = tx
            .execute(
                "DELETE FROM sessions WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("clear session messages", error))?;
        if removed > 0 {
            tx.execute(
                "UPDATE session_metadata SET message_count = 0, last_activity = now() \
                 WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("update metadata after clearing messages", error))?;
        }
        tx.commit()
            .map_err(|error| pg_error("commit clear-messages transaction", error))?;
        usize::try_from(removed).map_err(|error| pg_error("convert removed message count", error))
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.conn()?;
        let mut tx = conn
            .transaction()
            .map_err(|error| pg_error("begin delete-session transaction", error))?;
        tx.execute(
            "DELETE FROM sessions WHERE session_key = $1",
            &[&session_key],
        )
        .map_err(|error| pg_error("delete session messages", error))?;
        let removed = tx
            .execute(
                "DELETE FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("delete session metadata", error))?
            > 0;
        tx.commit()
            .map_err(|error| pg_error("commit delete-session transaction", error))?;
        Ok(removed)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let affected = self
            .conn()?
            .execute(
                "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = $1",
                &[&agent_alias],
            )
            .map_err(|error| pg_error("clear agent attribution", error))?;
        usize::try_from(affected)
            .map_err(|error| pg_error("convert cleared attribution count", error))
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let affected = self
            .conn()?
            .execute(
                "UPDATE session_metadata SET agent_alias = $1 WHERE agent_alias = $2",
                &[&to, &from],
            )
            .map_err(|error| pg_error("rename agent attribution", error))?;
        usize::try_from(affected)
            .map_err(|error| pg_error("convert renamed attribution count", error))
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let row = self
            .conn()?
            .query_one(
                "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = $1",
                &[&agent_alias],
            )
            .map_err(|error| pg_error("count agent attribution", error))?;
        let count: i64 = row.get(0);
        usize::try_from(count.max(0))
            .map_err(|error| pg_error("convert agent attribution count", error))
    }

    fn session_exists(&self, session_key: &str) -> bool {
        let Ok(mut conn) = self.conn() else {
            return false;
        };
        conn.query_opt(
            "SELECT 1 FROM session_metadata WHERE session_key = $1",
            &[&session_key],
        )
        .ok()
        .flatten()
        .is_some()
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let value = (!name.is_empty()).then_some(name);
        self.conn()?
            .execute(
                "UPDATE session_metadata SET name = $1 WHERE session_key = $2",
                &[&value, &session_key],
            )
            .map_err(|error| pg_error("set session name", error))?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let row = self
            .conn()?
            .query_opt(
                "SELECT name FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("get session name", error))?;
        Ok(row.and_then(|value| value.get(0)))
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let alias = (!agent_alias.is_empty()).then_some(agent_alias);
        let now = Utc::now();
        self.conn()?
            .execute(
                "INSERT INTO session_metadata \
                     (session_key, created_at, last_activity, message_count, agent_alias) \
                 VALUES ($1, $2, $3, 0, $4) \
                 ON CONFLICT (session_key) DO UPDATE SET agent_alias = EXCLUDED.agent_alias",
                &[&session_key, &now, &now, &alias],
            )
            .map_err(|error| pg_error("set session agent alias", error))?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let row = self
            .conn()?
            .query_opt(
                "SELECT agent_alias FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("get session agent alias", error))?;
        Ok(row.and_then(|value| value.get(0)))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let channel_id = normalize_optional(context.channel_id);
        let room_id = normalize_optional(context.room_id);
        let sender_id = normalize_optional(context.sender_id);
        let now = Utc::now();
        self.conn()?
            .execute(
                "INSERT INTO session_metadata \
                     (session_key, created_at, last_activity, message_count, \
                      channel_id, room_id, sender_id) \
                 VALUES ($1, $2, $3, 0, $4, $5, $6) \
                 ON CONFLICT (session_key) DO UPDATE SET \
                     channel_id = COALESCE(EXCLUDED.channel_id, session_metadata.channel_id), \
                     room_id = COALESCE(EXCLUDED.room_id, session_metadata.room_id), \
                     sender_id = COALESCE(EXCLUDED.sender_id, session_metadata.sender_id)",
                &[&session_key, &now, &now, &channel_id, &room_id, &sender_id],
            )
            .map_err(|error| pg_error("set session routing context", error))?;
        Ok(())
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let mut conn = self.conn().ok()?;
        let query = format!(
            "SELECT {} FROM session_metadata WHERE session_key = $1",
            Self::metadata_columns()
        );
        let row = conn.query_opt(&query, &[&session_key]).ok()??;
        Some(Self::row_to_metadata(&row))
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let started: Option<DateTime<Utc>> = (state == "running").then(Utc::now);
        let turn_id = turn_id.filter(|value| !value.is_empty());
        self.conn()?
            .execute(
                "UPDATE session_metadata SET state = $1, turn_id = $2, \
                 turn_started_at = $3 WHERE session_key = $4",
                &[&state, &turn_id, &started, &session_key],
            )
            .map_err(|error| pg_error("set session state", error))?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let row = self
            .conn()?
            .query_opt(
                "SELECT state, turn_id, turn_started_at FROM session_metadata \
                 WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(|error| pg_error("get session state", error))?;
        Ok(row.map(|value| SessionState {
            state: value.get("state"),
            turn_id: value.get("turn_id"),
            turn_started_at: value.get("turn_started_at"),
        }))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let query = format!(
            "SELECT {} FROM session_metadata WHERE state = 'running' \
             ORDER BY turn_started_at DESC NULLS LAST",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&query, &[]) else {
            return Vec::new();
        };
        rows.iter().map(Self::row_to_metadata).collect()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let seconds = i64::try_from(threshold_secs).unwrap_or(i64::MAX);
        let cutoff = Utc::now() - chrono::Duration::seconds(seconds);
        let Ok(mut conn) = self.conn() else {
            return Vec::new();
        };
        let query = format!(
            "SELECT {} FROM session_metadata \
             WHERE state = 'running' AND turn_started_at < $1 \
             ORDER BY turn_started_at ASC",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&query, &[&cutoff]) else {
            return Vec::new();
        };
        rows.iter().map(Self::row_to_metadata).collect()
    }

    fn compact(&self, _session_key: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsquery_uses_safe_prefix_lexemes() {
        assert_eq!(
            build_tsquery("Rust async (best)"),
            "rust:* & async:* & best:*"
        );
    }

    #[test]
    fn tsquery_drops_punctuation_only_tokens() {
        assert_eq!(build_tsquery(" -- !!! "), "");
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_POSTGRES_URL pointing at a live PostgreSQL database"]
    fn postgres_live_round_trip_metadata_state_and_search() {
        let Ok(url) = std::env::var("ZEROCLAW_TEST_POSTGRES_URL") else {
            eprintln!("ZEROCLAW_TEST_POSTGRES_URL not set; skipping PostgreSQL live test");
            return;
        };
        if url.trim().is_empty() {
            eprintln!("ZEROCLAW_TEST_POSTGRES_URL is empty; skipping PostgreSQL live test");
            return;
        }

        let _ = url;
        let workspace = tempfile::TempDir::new().expect("create temporary workspace");
        let backend = crate::make_session_backend(workspace.path(), "postgres")
            .expect("construct PostgreSQL backend through factory");
        let key = format!(
            "postgres_live_{}_{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        backend
            .append(
                &key,
                &ChatMessage::user("unique postgresql full text needle"),
            )
            .expect("append user message");
        backend
            .append(&key, &ChatMessage::assistant("initial response"))
            .expect("append assistant message");
        backend
            .update_last(&key, &ChatMessage::assistant("updated response"))
            .expect("update last message");
        backend
            .set_session_name(&key, "PostgreSQL live test")
            .unwrap();
        backend
            .set_session_agent_alias(&key, "postgres-test")
            .unwrap();
        backend
            .set_session_context(
                &key,
                SessionContext {
                    channel_id: Some("discord.test"),
                    room_id: Some("room-1"),
                    sender_id: Some("sender-1"),
                },
            )
            .unwrap();
        backend
            .set_session_state(&key, "running", Some("turn-1"))
            .unwrap();

        let messages = backend.load_with_timestamps(&key);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].message.content, "updated response");
        assert!(messages.iter().all(|message| message.created_at.is_some()));

        let metadata = backend.get_session_metadata(&key).expect("metadata");
        assert_eq!(metadata.message_count, 2);
        assert_eq!(metadata.agent_alias.as_deref(), Some("postgres-test"));
        assert_eq!(metadata.channel_id.as_deref(), Some("discord.test"));
        assert_eq!(metadata.room_id.as_deref(), Some("room-1"));
        assert_eq!(metadata.sender_id.as_deref(), Some("sender-1"));
        assert_eq!(
            backend.get_session_state(&key).unwrap().unwrap().state,
            "running"
        );

        let matches = backend.search(&SessionQuery {
            keyword: Some("postgres needle".to_string()),
            limit: Some(10),
        });
        assert!(matches.iter().any(|metadata| metadata.key == key));

        assert!(backend.remove_last(&key).unwrap());
        assert_eq!(backend.get_session_metadata(&key).unwrap().message_count, 1);
        assert_eq!(backend.clear_messages(&key).unwrap(), 1);
        assert!(backend.delete_session(&key).unwrap());
    }
}
