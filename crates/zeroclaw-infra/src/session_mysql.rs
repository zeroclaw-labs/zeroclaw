//! MySQL 9.0+-backed session persistence.
//!
//! Stores sessions in a remote MySQL database using an `r2d2` connection
//! pool backed by the `mysql` native protocol crate.  Designed for
//! multi-agent fleets where session state must be shared across worker hosts.
//! MySQL 9.0+ is required; earlier versions lack the `VECTOR` column type
//! (unused here but relied on by other fleet services that share the same
//! instance) and may have replication edge-cases with fractional-second
//! timestamps.
//!
//! # Feature flag
//!
//! Requires `--features backend-mysql` (adds `mysql` + `r2d2-mysql`).
//!
//! # Configuration
//!
//! ```toml
//! [sessions]
//! backend   = "mysql"
//! mysql_url = "mysql://zeroclaw:secret@primary:3306/zeroclaw"
//! pool_size = 5
//! # For HA: point at a ProxySQL VIP, Group Replication, or MySQL Router endpoint.
//! ```
//!
//! The schema uses the same logical columns as the SQLite, PostgreSQL, and
//! Oracle backends so data can be migrated between backends without
//! transformation.

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
    TimestampedMessage,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mysql::{prelude::*, Pool};
use zeroclaw_api::model_provider::ChatMessage;

// ── Backend ───────────────────────────────────────────────────────────────

/// MySQL 9.0+-backed session store.
///
/// Uses an `r2d2` connection pool.  All timestamps are stored and queried
/// in UTC using `DATETIME(6)` columns; each new connection runs
/// `SET time_zone = '+00:00'` automatically via the pool initialisation.
pub struct MysqlSessionBackend {
    pool: Pool,
}

impl MysqlSessionBackend {
    /// Connect and initialise the schema.
    ///
    /// `database_url` is a standard MySQL DSN, e.g.
    /// `"mysql://zeroclaw:secret@db-primary:3306/zeroclaw"`.
    /// `pool_size` controls the maximum number of pooled connections.
    pub fn new(database_url: &str, pool_size: usize) -> Result<Self> {
        let opts = mysql::Opts::from_url(database_url).context("invalid MySQL URL")?;
        // Pool::new_manual was removed in mysql 25; use Pool::new + PoolOpts for
        // size control. PoolConstraints::new(active_min, active_max).
        let constraints = mysql::PoolConstraints::new(1, pool_size)
            .context("invalid pool constraints")?;
        let pool_opts = mysql::PoolOpts::new().with_constraints(constraints);
        let builder = mysql::OptsBuilder::from_opts(opts)
            // Enforce UTC for all connections so DATETIME(6) values round-trip
            // correctly without server-side timezone configuration.
            .init(vec!["SET time_zone = '+00:00'"])
            .pool_opts(pool_opts);
        let pool = Pool::new(builder).context("failed to build MySQL connection pool")?;

        let mut conn = pool.get_conn().context("failed to get initial MySQL connection")?;

        // MySQL 9.0 supports CREATE TABLE IF NOT EXISTS and CREATE INDEX IF NOT EXISTS.
        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          BIGINT UNSIGNED  NOT NULL AUTO_INCREMENT PRIMARY KEY,
                session_key VARCHAR(512)     NOT NULL,
                role        VARCHAR(64)      NOT NULL,
                content     LONGTEXT         NOT NULL,
                created_at  DATETIME(6)      NOT NULL DEFAULT NOW(6),
                INDEX idx_sessions_key    (session_key),
                INDEX idx_sessions_key_id (session_key, id)
             ) DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
        )
        .context("failed to create sessions table")?;

        conn.query_drop(
            "CREATE TABLE IF NOT EXISTS session_metadata (
                session_key      VARCHAR(512)    NOT NULL,
                created_at       DATETIME(6)     NOT NULL DEFAULT NOW(6),
                last_activity    DATETIME(6)     NOT NULL DEFAULT NOW(6),
                message_count    BIGINT UNSIGNED NOT NULL DEFAULT 0,
                name             VARCHAR(1024),
                state            VARCHAR(64)     NOT NULL DEFAULT 'idle',
                turn_id          VARCHAR(512),
                turn_started_at  DATETIME(6),
                agent_alias      VARCHAR(512),
                channel_id       VARCHAR(512),
                room_id          VARCHAR(512),
                sender_id        VARCHAR(512),
                PRIMARY KEY (session_key),
                INDEX idx_smeta_agent_alias (agent_alias),
                INDEX idx_smeta_channel_id  (channel_id),
                INDEX idx_smeta_room_id     (room_id),
                INDEX idx_smeta_sender_id   (sender_id)
             ) DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci",
        )
        .context("failed to create session_metadata table")?;

        Ok(Self { pool })
    }
}

// ── SessionBackend impl ───────────────────────────────────────────────────

impl SessionBackend for MysqlSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        conn.exec_map(
            "SELECT role, content FROM sessions
             WHERE session_key = ? ORDER BY id ASC",
            (session_key,),
            |(role, content)| ChatMessage { role, content },
        )
        .unwrap_or_default()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;

        conn.exec_drop(
            "INSERT INTO sessions (session_key, role, content, created_at)
             VALUES (?, ?, ?, NOW(6))",
            (session_key, &message.role, &message.content),
        )
        .map_err(std::io::Error::other)?;

        // INSERT … ON DUPLICATE KEY UPDATE is MySQL's upsert idiom.
        conn.exec_drop(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count)
             VALUES (?, NOW(6), NOW(6), 1)
             ON DUPLICATE KEY UPDATE
                 last_activity = NOW(6),
                 message_count = message_count + 1",
            (session_key,),
        )
        .map_err(std::io::Error::other)?;

        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;

        // Find the max id for this session; None means nothing to remove.
        let max_id: Option<u64> = conn
            .exec_first(
                "SELECT MAX(id) FROM sessions WHERE session_key = ?",
                (session_key,),
            )
            .map_err(std::io::Error::other)?
            .flatten();

        let Some(id) = max_id else { return Ok(false) };

        conn.exec_drop("DELETE FROM sessions WHERE id = ?", (id,))
            .map_err(std::io::Error::other)?;

        conn.exec_drop(
            "UPDATE session_metadata
             SET message_count = GREATEST(0, CAST(message_count AS SIGNED) - 1),
                 last_activity  = NOW(6)
             WHERE session_key = ?",
            (session_key,),
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        conn.exec_map(
            "SELECT session_key FROM session_metadata ORDER BY last_activity DESC",
            (),
            |k: String| k,
        )
        .unwrap_or_default()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        conn.exec_map(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata ORDER BY last_activity DESC",
            (),
            map_row,
        )
        .unwrap_or_default()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;

        let keys: Vec<String> = conn
            .exec_map(
                "SELECT session_key FROM session_metadata
                 WHERE last_activity < DATE_SUB(NOW(6), INTERVAL ? HOUR)",
                (ttl_hours,),
                |k: String| k,
            )
            .map_err(std::io::Error::other)?;

        let n = keys.len();
        for key in &keys {
            conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (key,))
                .map_err(std::io::Error::other)?;
            conn.exec_drop(
                "DELETE FROM session_metadata WHERE session_key = ?",
                (key,),
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        let Some(ref kw) = query.keyword else {
            return self.list_sessions_with_metadata();
        };
        let limit = query.limit.unwrap_or(50) as u64;
        // MySQL LIKE on utf8mb4_unicode_ci columns is case-insensitive by default.
        let pattern = format!("%{kw}%");
        conn.exec_map(
            "SELECT DISTINCT s.session_key, m.name, m.created_at,
                    m.last_activity, m.message_count,
                    m.agent_alias, m.channel_id, m.room_id, m.sender_id
             FROM sessions s
             JOIN session_metadata m USING (session_key)
             WHERE s.content LIKE ?
             ORDER BY m.last_activity DESC
             LIMIT ?",
            (pattern, limit),
            map_row,
        )
        .unwrap_or_default()
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;

        conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (session_key,))
            .map_err(std::io::Error::other)?;

        conn.exec_drop(
            "DELETE FROM session_metadata WHERE session_key = ?",
            (session_key,),
        )
        .map_err(std::io::Error::other)?;

        Ok(conn.affected_rows() > 0)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;
        conn.exec_drop(
            "UPDATE session_metadata SET name = ? WHERE session_key = ?",
            (name, session_key),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Ok(None);
        };
        let row: Option<Option<String>> = conn
            .exec_first(
                "SELECT name FROM session_metadata WHERE session_key = ?",
                (session_key,),
            )
            .map_err(std::io::Error::other)?;
        Ok(row.flatten())
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;
        conn.exec_drop(
            "UPDATE session_metadata
             SET state = ?, turn_id = ?, turn_started_at = NOW(6)
             WHERE session_key = ?",
            (state, turn_id, session_key),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Ok(None);
        };
        let row: Option<(String, Option<String>, Option<String>)> = conn
            .exec_first(
                "SELECT state, turn_id,
                         DATE_FORMAT(turn_started_at, '%Y-%m-%dT%H:%i:%S.%fZ')
                  FROM session_metadata WHERE session_key = ?",
                (session_key,),
            )
            .map_err(std::io::Error::other)?;

        let Some((state, turn_id, ts_str)) = row else {
            return Ok(None);
        };
        let turn_started_at = ts_str
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Ok(Some(SessionState { state, turn_id, turn_started_at }))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        // MySQL has no NULLS LAST; achieve it with IS NULL (1 for NULL → sorts last).
        conn.exec_map(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata WHERE state = 'running'
             ORDER BY (turn_started_at IS NULL), turn_started_at ASC",
            (),
            map_row,
        )
        .unwrap_or_default()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        conn.exec_map(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata
             WHERE state = 'running'
               AND turn_started_at < DATE_SUB(NOW(6), INTERVAL ? SECOND)
             ORDER BY turn_started_at ASC",
            (threshold_secs,),
            map_row,
        )
        .unwrap_or_default()
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let mut conn = self.pool.get_conn().ok()?;
        conn.exec_first(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata WHERE session_key = ?",
            (session_key,),
        )
        .ok()?
        .map(map_row)
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Vec::new();
        };
        conn.exec_map(
            "SELECT role, content, created_at FROM sessions
             WHERE session_key = ? ORDER BY id ASC",
            (session_key,),
            |(role, content, created_at): (String, String, chrono::NaiveDateTime)| {
                TimestampedMessage {
                    message: ChatMessage { role, content },
                    created_at: Some(DateTime::from_naive_utc_and_offset(created_at, Utc)),
                }
            },
        )
        .unwrap_or_default()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;
        conn.exec_drop("DELETE FROM sessions WHERE session_key = ?", (session_key,))
            .map_err(std::io::Error::other)?;
        let n = conn.affected_rows() as usize;
        if n > 0 {
            conn.exec_drop(
                "UPDATE session_metadata
                 SET message_count = 0, last_activity = NOW(6)
                 WHERE session_key = ?",
                (session_key,),
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn set_session_agent_alias(
        &self,
        session_key: &str,
        agent_alias: &str,
    ) -> std::io::Result<()> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;
        let alias_val: Option<&str> =
            if agent_alias.is_empty() { None } else { Some(agent_alias) };
        conn.exec_drop(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count, agent_alias)
             VALUES (?, NOW(6), NOW(6), 0, ?)
             ON DUPLICATE KEY UPDATE agent_alias = VALUES(agent_alias)",
            (session_key, alias_val),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut conn) = self.pool.get_conn() else {
            return Ok(None);
        };
        let row: Option<Option<String>> = conn
            .exec_first(
                "SELECT agent_alias FROM session_metadata WHERE session_key = ?",
                (session_key,),
            )
            .map_err(std::io::Error::other)?;
        Ok(row.flatten())
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let mut conn = self.pool.get_conn().map_err(std::io::Error::other)?;
        fn norm(v: Option<&str>) -> Option<String> {
            v.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
        }
        let channel_id = norm(context.channel_id);
        let room_id = norm(context.room_id);
        let sender_id = norm(context.sender_id);
        conn.exec_drop(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count,
                  channel_id, room_id, sender_id)
             VALUES (?, NOW(6), NOW(6), 0, ?, ?, ?)
             ON DUPLICATE KEY UPDATE
                 channel_id = COALESCE(VALUES(channel_id), channel_id),
                 room_id    = COALESCE(VALUES(room_id),    room_id),
                 sender_id  = COALESCE(VALUES(sender_id),  sender_id)",
            (session_key, channel_id, room_id, sender_id),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }
}

// ── Row mapping ───────────────────────────────────────────────────────────

/// Map a 9-column metadata row from `session_metadata`.
///
/// Columns: `session_key, name, created_at, last_activity, message_count,
///           agent_alias, channel_id, room_id, sender_id`.
///
/// `created_at` and `last_activity` are MySQL `DATETIME(6)` values, which
/// the `mysql` crate deserialises as `chrono::NaiveDateTime`.  We treat all
/// values as UTC (enforced by `SET time_zone = '+00:00'` on connect).
fn map_row(
    (key, name, created_at, last_activity, count, agent_alias, channel_id, room_id, sender_id): (
        String,
        Option<String>,
        chrono::NaiveDateTime,
        chrono::NaiveDateTime,
        u64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ),
) -> SessionMetadata {
    SessionMetadata {
        key,
        name,
        created_at: DateTime::from_naive_utc_and_offset(created_at, Utc),
        last_activity: DateTime::from_naive_utc_and_offset(last_activity, Utc),
        message_count: count as usize,
        agent_alias,
        channel_id,
        room_id,
        sender_id,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_url() -> Option<String> {
        std::env::var("ZEROCLAW_TEST_MYSQL_URL").ok()
    }

    #[test]
    fn mysql_backend_round_trip() {
        let Some(url) = test_url() else {
            eprintln!(
                "ZEROCLAW_TEST_MYSQL_URL not set — skipping MySQL backend test\n\
                 Example: mysql://zeroclaw:secret@localhost:3306/zeroclaw"
            );
            return;
        };
        let backend = MysqlSessionBackend::new(&url, 2).expect("connect");
        let key = format!(
            "test-mysql-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let msg = ChatMessage { role: "user".into(), content: "hello mysql".into() };
        backend.append(&key, &msg).expect("append");

        let loaded = backend.load(&key);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello mysql");

        assert!(backend.remove_last(&key).expect("remove_last"));
        assert!(backend.load(&key).is_empty());
        backend.delete_session(&key).expect("delete");
    }
}
