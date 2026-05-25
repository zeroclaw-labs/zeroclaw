//! PostgreSQL-backed session persistence with HA/failover support.
//!
//! Stores sessions in a remote PostgreSQL database using an `r2d2` connection
//! pool. Designed for multi-agent fleets where session state must be shared
//! across worker hosts. Supports Patroni, pgBouncer, RDS, Aurora, or any
//! standard PostgreSQL endpoint.
//!
//! # Feature flag
//!
//! Requires `--features backend-postgres` (adds `postgres` + `r2d2-postgres`).
//!
//! # Configuration
//!
//! ```toml
//! [sessions]
//! backend = "postgres"
//! postgres_url = "postgresql://zeroclaw:secret@primary-host/zeroclaw"
//! pool_size = 5
//! # For HA: point at a pgBouncer VIP or Patroni virtual IP.
//! ```
//!
//! The schema uses the same column names as the SQLite backend so data can be
//! migrated between backends without transformation.

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState, TimestampedMessage,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use r2d2::Pool;
use r2d2_postgres::{PostgresConnectionManager, postgres::NoTls};
use zeroclaw_api::model_provider::ChatMessage;

/// PostgreSQL-backed session store.
///
/// Uses an `r2d2` connection pool. The pool retries on connection failure,
/// making failover to a Patroni replica transparent to callers.
pub struct PostgresSessionBackend {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresSessionBackend {
    /// Connect and initialise the schema.
    ///
    /// `database_url` is a standard libpq DSN, e.g.
    /// `"postgresql://zeroclaw:secret@db-primary/zeroclaw"`.
    /// `pool_size` controls the maximum number of pooled connections.
    pub fn new(database_url: &str, pool_size: u32) -> Result<Self> {
        let manager = PostgresConnectionManager::new(
            database_url.parse().context("invalid postgres URL")?,
            NoTls,
        );
        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .context("failed to build postgres connection pool")?;

        let mut conn = pool
            .get()
            .context("failed to get initial postgres connection")?;

        conn.batch_execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id          BIGSERIAL    PRIMARY KEY,
                session_key TEXT         NOT NULL,
                role        TEXT         NOT NULL,
                content     TEXT         NOT NULL,
                created_at  TIMESTAMPTZ  NOT NULL DEFAULT now()
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_key
                 ON sessions(session_key);
             CREATE INDEX IF NOT EXISTS idx_sessions_key_id
                 ON sessions(session_key, id);

             CREATE TABLE IF NOT EXISTS session_metadata (
                session_key     TEXT        PRIMARY KEY,
                created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_activity   TIMESTAMPTZ NOT NULL DEFAULT now(),
                message_count   BIGINT      NOT NULL DEFAULT 0,
                name            TEXT,
                state           TEXT        NOT NULL DEFAULT 'idle',
                turn_id         TEXT,
                turn_started_at TIMESTAMPTZ,
                agent_alias     TEXT,
                channel_id      TEXT,
                room_id         TEXT,
                sender_id       TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_smeta_agent_alias ON session_metadata(agent_alias);
             CREATE INDEX IF NOT EXISTS idx_smeta_channel_id  ON session_metadata(channel_id);
             CREATE INDEX IF NOT EXISTS idx_smeta_room_id     ON session_metadata(room_id);
             CREATE INDEX IF NOT EXISTS idx_smeta_sender_id   ON session_metadata(sender_id);",
        )
        .context("failed to initialise postgres session schema")?;

        Ok(Self { pool })
    }
}

impl SessionBackend for PostgresSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content FROM sessions
             WHERE session_key = $1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| ChatMessage {
                role: row.get(0),
                content: row.get(1),
            })
            .collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        let now = Utc::now();

        conn.execute(
            "INSERT INTO sessions (session_key, role, content, created_at)
             VALUES ($1, $2, $3, $4)",
            &[&session_key, &message.role, &message.content, &now],
        )
        .map_err(std::io::Error::other)?;

        conn.execute(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count)
             VALUES ($1, $2, $3, 1)
             ON CONFLICT (session_key) DO UPDATE SET
                 last_activity = EXCLUDED.last_activity,
                 message_count = session_metadata.message_count + 1",
            &[&session_key, &now, &now],
        )
        .map_err(std::io::Error::other)?;

        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;

        let n = conn
            .execute(
                "DELETE FROM sessions WHERE id = (
                     SELECT id FROM sessions
                     WHERE session_key = $1
                     ORDER BY id DESC LIMIT 1
                 )",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;

        if n > 0 {
            conn.execute(
                "UPDATE session_metadata
                 SET message_count = GREATEST(0, message_count - 1),
                     last_activity  = now()
                 WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        }

        Ok(n > 0)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT session_key FROM session_metadata ORDER BY last_activity DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows.into_iter().map(|r| r.get(0)).collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata ORDER BY last_activity DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| {
                let count: i64 = row.get(4);
                SessionMetadata {
                    key: row.get(0),
                    name: row.get(1),
                    created_at: row.get(2),
                    last_activity: row.get(3),
                    message_count: count as usize,
                    agent_alias: row.get(5),
                    channel_id: row.get(6),
                    room_id: row.get(7),
                    sender_id: row.get(8),
                }
            })
            .collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        let cutoff = Utc::now() - chrono::Duration::hours(i64::from(ttl_hours));

        let keys: Vec<String> = conn
            .query(
                "SELECT session_key FROM session_metadata WHERE last_activity < $1",
                &[&cutoff],
            )
            .map_err(std::io::Error::other)?
            .into_iter()
            .map(|r| r.get(0))
            .collect();

        let n = keys.len();
        for key in &keys {
            conn.execute("DELETE FROM sessions WHERE session_key = $1", &[key])
                .map_err(std::io::Error::other)?;
            conn.execute(
                "DELETE FROM session_metadata WHERE session_key = $1",
                &[key],
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Some(ref kw) = query.keyword else {
            return self.list_sessions_with_metadata();
        };
        let limit = query.limit.unwrap_or(50) as i64;
        let pattern = format!("%{kw}%");
        let Ok(rows) = conn.query(
            "SELECT DISTINCT s.session_key, m.name, m.created_at,
                    m.last_activity, m.message_count,
                    m.agent_alias, m.channel_id, m.room_id, m.sender_id
             FROM sessions s
             JOIN session_metadata m USING (session_key)
             WHERE s.content ILIKE $1
             ORDER BY m.last_activity DESC
             LIMIT $2",
            &[&pattern, &limit],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| {
                let count: i64 = row.get(4);
                SessionMetadata {
                    key: row.get(0),
                    name: row.get(1),
                    created_at: row.get(2),
                    last_activity: row.get(3),
                    message_count: count as usize,
                    agent_alias: row.get(5),
                    channel_id: row.get(6),
                    room_id: row.get(7),
                    sender_id: row.get(8),
                }
            })
            .collect()
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        conn.execute(
            "DELETE FROM sessions WHERE session_key = $1",
            &[&session_key],
        )
        .map_err(std::io::Error::other)?;
        let n = conn
            .execute(
                "DELETE FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        Ok(n > 0)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        conn.execute(
            "UPDATE session_metadata SET name = $1 WHERE session_key = $2",
            &[&name, &session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut conn) = self.pool.get() else {
            return Ok(None);
        };
        let row = conn
            .query_opt(
                "SELECT name FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        Ok(row.and_then(|r| r.get(0)))
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        let now = Utc::now();
        conn.execute(
            "UPDATE session_metadata
             SET state = $1, turn_id = $2, turn_started_at = $3
             WHERE session_key = $4",
            &[&state, &turn_id, &now, &session_key],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let Ok(mut conn) = self.pool.get() else {
            return Ok(None);
        };
        let row = conn
            .query_opt(
                "SELECT state, turn_id, turn_started_at
                 FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        Ok(row.map(|r| {
            let ts: Option<DateTime<Utc>> = r.get(2);
            SessionState {
                state: r.get(0),
                turn_id: r.get(1),
                turn_started_at: ts,
            }
        }))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata WHERE state = 'running'
             ORDER BY turn_started_at ASC NULLS LAST",
            &[],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| {
                let count: i64 = row.get(4);
                SessionMetadata {
                    key: row.get(0),
                    name: row.get(1),
                    created_at: row.get(2),
                    last_activity: row.get(3),
                    message_count: count as usize,
                    agent_alias: row.get(5),
                    channel_id: row.get(6),
                    room_id: row.get(7),
                    sender_id: row.get(8),
                }
            })
            .collect()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let cutoff = Utc::now() - chrono::Duration::seconds(threshold_secs as i64);
        let Ok(rows) = conn.query(
            "SELECT session_key, name, created_at, last_activity, message_count,
                    agent_alias, channel_id, room_id, sender_id
             FROM session_metadata
             WHERE state = 'running' AND turn_started_at < $1
             ORDER BY turn_started_at ASC",
            &[&cutoff],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| {
                let count: i64 = row.get(4);
                SessionMetadata {
                    key: row.get(0),
                    name: row.get(1),
                    created_at: row.get(2),
                    last_activity: row.get(3),
                    message_count: count as usize,
                    agent_alias: row.get(5),
                    channel_id: row.get(6),
                    room_id: row.get(7),
                    sender_id: row.get(8),
                }
            })
            .collect()
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let mut conn = self.pool.get().ok()?;
        let row = conn
            .query_opt(
                "SELECT session_key, name, created_at, last_activity, message_count,
                        agent_alias, channel_id, room_id, sender_id
                 FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .ok()??;
        let count: i64 = row.get(4);
        Some(SessionMetadata {
            key: row.get(0),
            name: row.get(1),
            created_at: row.get(2),
            last_activity: row.get(3),
            message_count: count as usize,
            agent_alias: row.get(5),
            channel_id: row.get(6),
            room_id: row.get(7),
            sender_id: row.get(8),
        })
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(mut conn) = self.pool.get() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content, created_at FROM sessions
             WHERE session_key = $1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.into_iter()
            .map(|row| {
                let ts: DateTime<Utc> = row.get(2);
                TimestampedMessage {
                    message: ChatMessage {
                        role: row.get(0),
                        content: row.get(1),
                    },
                    created_at: Some(ts),
                }
            })
            .collect()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        let n = conn
            .execute(
                "DELETE FROM sessions WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)? as usize;
        if n > 0 {
            conn.execute(
                "UPDATE session_metadata
                 SET message_count = 0, last_activity = now()
                 WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        let alias_val: Option<&str> = if agent_alias.is_empty() {
            None
        } else {
            Some(agent_alias)
        };
        let now = Utc::now();
        conn.execute(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count, agent_alias)
             VALUES ($1, $2, $3, 0, $4)
             ON CONFLICT (session_key) DO UPDATE SET
                 agent_alias = EXCLUDED.agent_alias",
            &[&session_key, &now, &now, &alias_val],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut conn) = self.pool.get() else {
            return Ok(None);
        };
        let row = conn
            .query_opt(
                "SELECT agent_alias FROM session_metadata WHERE session_key = $1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
        Ok(row.and_then(|r| r.get(0)))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let mut conn = self.pool.get().map_err(std::io::Error::other)?;
        fn norm(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let channel_id = norm(context.channel_id);
        let room_id = norm(context.room_id);
        let sender_id = norm(context.sender_id);
        let now = Utc::now();
        conn.execute(
            "INSERT INTO session_metadata
                 (session_key, created_at, last_activity, message_count,
                  channel_id, room_id, sender_id)
             VALUES ($1, $2, $3, 0, $4, $5, $6)
             ON CONFLICT (session_key) DO UPDATE SET
                 channel_id = COALESCE(EXCLUDED.channel_id, session_metadata.channel_id),
                 room_id    = COALESCE(EXCLUDED.room_id,    session_metadata.room_id),
                 sender_id  = COALESCE(EXCLUDED.sender_id,  session_metadata.sender_id)",
            &[&session_key, &now, &now, &channel_id, &room_id, &sender_id],
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_url() -> Option<String> {
        std::env::var("ZEROCLAW_TEST_POSTGRES_URL").ok()
    }

    #[test]
    fn postgres_backend_round_trip() {
        let Some(url) = test_url() else {
            eprintln!("ZEROCLAW_TEST_POSTGRES_URL not set — skipping postgres backend test");
            return;
        };
        let backend = PostgresSessionBackend::new(&url, 2).expect("connect");
        let key = format!(
            "test-pg-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let msg = ChatMessage {
            role: "user".into(),
            content: "hello postgres".into(),
        };
        backend.append(&key, &msg).expect("append");

        let loaded = backend.load(&key);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello postgres");

        assert!(backend.remove_last(&key).expect("remove_last"));
        assert!(backend.load(&key).is_empty());
        backend.delete_session(&key).expect("delete");
    }
}
