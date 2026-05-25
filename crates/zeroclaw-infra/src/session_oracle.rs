//! Oracle 23ai-backed session persistence with HA/failover support.
//!
//! Stores sessions in Oracle tables `ZC_SESSIONS` and `ZC_SESSION_META`
//! using the [`oracle`] OCI thin-client crate.  Connection pooling is
//! provided by [`r2d2`]; the pool size is configurable.  Compatible with
//! Oracle 23ai Free, Oracle 23ai (on-prem / ADB), and Oracle 19c+.
//! DRCP (Database Resident Connection Pooling) and wallet-based auth are
//! transparent — pass the appropriate DSN and credentials to [`OracleSessionBackend::new`].
//!
//! # Prerequisites
//!
//! Oracle Instant Client (or a full Oracle client installation) must be
//! present at runtime so that OCI can be loaded.  Set `LD_LIBRARY_PATH`
//! (Linux) or `DYLD_LIBRARY_PATH` (macOS) to the Instant Client directory.
//!
//! # Feature flag
//!
//! Requires `--features backend-oracle` (adds `oracle` + `r2d2`).
//!
//! # Configuration
//!
//! ```toml
//! [sessions]
//! backend          = "oracle"
//! oracle_user      = "zeroclaw"
//! oracle_password  = "secret"
//! oracle_dsn       = "//primary-host:1521/ORCLPDB1"
//! pool_size        = 5
//! # For HA: point at a SCAN address, DRCP endpoint, or connection-pool DSN.
//! ```
//!
//! The schema uses the same logical columns as the SQLite and PostgreSQL
//! backends so data can be migrated between backends without transformation.

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState,
    TimestampedMessage,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use oracle::Connection;
use r2d2::{ManageConnection, Pool};
use zeroclaw_api::model_provider::ChatMessage;

// ── Thread-safety wrapper ─────────────────────────────────────────────────

/// Newtype that allows `oracle::Connection` to be sent across threads.
///
/// # Safety
///
/// Oracle OCI is initialised in `OCI_THREADED` mode, which makes the OCI
/// environment itself thread-safe.  Individual connections are NOT safe for
/// *concurrent* use from multiple threads — but they ARE safe to *move*
/// between threads provided only one thread holds the connection at a time.
/// `r2d2` enforces this invariant: each `PooledConnection` is an exclusive
/// handle; no two threads can access the same underlying connection
/// simultaneously.  Wrapping in `Mutex` would provide the same guarantee
/// but with unnecessary overhead given `r2d2`'s existing exclusive-handle
/// discipline.
struct OracleConn(Connection);

// SAFETY: see struct-level doc comment above.
unsafe impl Send for OracleConn {}

// ── r2d2 connection manager ───────────────────────────────────────────────

struct OracleManager {
    user: String,
    password: String,
    dsn: String,
}

impl ManageConnection for OracleManager {
    type Connection = OracleConn;
    type Error = oracle::Error;

    fn connect(&self) -> Result<OracleConn, oracle::Error> {
        Connection::connect(&self.user, &self.password, &self.dsn).map(OracleConn)
    }

    fn is_valid(&self, conn: &mut OracleConn) -> Result<(), oracle::Error> {
        conn.0.execute("SELECT 1 FROM DUAL", &[])?;
        Ok(())
    }

    fn has_broken(&self, _conn: &mut OracleConn) -> bool {
        false
    }
}

// ── Backend ───────────────────────────────────────────────────────────────

/// Oracle 23ai-backed session store.
///
/// Uses an `r2d2` connection pool over OCI.  Pool retries on connection
/// failure, making transparent failover to an Oracle Data Guard standby or
/// RAC node possible when the DSN is a SCAN address or DRCP endpoint.
pub struct OracleSessionBackend {
    pool: Pool<OracleManager>,
}

impl OracleSessionBackend {
    /// Connect and initialise the schema.
    ///
    /// `oracle_dsn` is an Oracle Easy Connect string, e.g.
    /// `"//db-host:1521/ORCLPDB1"`.  `pool_size` sets the maximum number
    /// of pooled OCI connections.
    pub fn new(user: &str, password: &str, dsn: &str, pool_size: u32) -> Result<Self> {
        let manager = OracleManager {
            user: user.to_owned(),
            password: password.to_owned(),
            dsn: dsn.to_owned(),
        };
        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .context("failed to build Oracle connection pool")?;

        let mut guard = pool
            .get()
            .context("failed to get initial Oracle connection")?;
        let conn = &mut guard.0;

        // Oracle < 23c does not support `CREATE TABLE IF NOT EXISTS`.
        // Use PL/SQL EXECUTE IMMEDIATE and swallow ORA-00955 (name already
        // used by an existing object).
        let ddl_statements: &[&str] = &[
            "CREATE TABLE ZC_SESSIONS (
                ID          NUMBER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                SESSION_KEY VARCHAR2(512)                    NOT NULL,
                ROLE        VARCHAR2(64)                     NOT NULL,
                CONTENT     CLOB                             NOT NULL,
                CREATED_AT  TIMESTAMP WITH TIME ZONE         NOT NULL
             )",
            "CREATE INDEX IDX_ZC_SESS_KEY
                 ON ZC_SESSIONS(SESSION_KEY)",
            "CREATE INDEX IDX_ZC_SESS_KEY_ID
                 ON ZC_SESSIONS(SESSION_KEY, ID)",
            "CREATE TABLE ZC_SESSION_META (
                SESSION_KEY      VARCHAR2(512)            PRIMARY KEY,
                CREATED_AT       TIMESTAMP WITH TIME ZONE NOT NULL,
                LAST_ACTIVITY    TIMESTAMP WITH TIME ZONE NOT NULL,
                MESSAGE_COUNT    NUMBER(19)  DEFAULT 0    NOT NULL,
                NAME             VARCHAR2(1024),
                STATE            VARCHAR2(64) DEFAULT 'idle' NOT NULL,
                TURN_ID          VARCHAR2(512),
                TURN_STARTED_AT  TIMESTAMP WITH TIME ZONE,
                AGENT_ALIAS      VARCHAR2(512),
                CHANNEL_ID       VARCHAR2(512),
                ROOM_ID          VARCHAR2(512),
                SENDER_ID        VARCHAR2(512)
             )",
            "CREATE INDEX IDX_ZC_SMETA_AGENT ON ZC_SESSION_META(AGENT_ALIAS)",
            "CREATE INDEX IDX_ZC_SMETA_CHAN  ON ZC_SESSION_META(CHANNEL_ID)",
            "CREATE INDEX IDX_ZC_SMETA_ROOM  ON ZC_SESSION_META(ROOM_ID)",
            "CREATE INDEX IDX_ZC_SMETA_SENDER ON ZC_SESSION_META(SENDER_ID)",
        ];

        for &ddl in ddl_statements {
            // ORA-00955 = object already exists; safe to ignore.
            conn.execute(
                "BEGIN
                   EXECUTE IMMEDIATE :1;
                 EXCEPTION WHEN OTHERS THEN
                   IF SQLCODE != -955 THEN RAISE; END IF;
                 END;",
                &[&ddl],
            )
            .context("failed to initialise Oracle session schema")?;
        }

        Ok(Self { pool })
    }
}

// ── SessionBackend impl ───────────────────────────────────────────────────

impl SessionBackend for OracleSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Ok(rows) = conn.query(
            "SELECT ROLE, CONTENT FROM ZC_SESSIONS
             WHERE SESSION_KEY = :1 ORDER BY ID ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok())
            .map(|row| ChatMessage {
                role: row.get(0).unwrap_or_default(),
                content: row.get(1).unwrap_or_default(),
            })
            .collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;

        conn.execute(
            "INSERT INTO ZC_SESSIONS (SESSION_KEY, ROLE, CONTENT, CREATED_AT)
             VALUES (:1, :2, :3, SYSTIMESTAMP)",
            &[&session_key, &message.role, &message.content],
        )
        .map_err(std::io::Error::other)?;

        // MERGE acts as an upsert: insert the metadata row on first message,
        // then increment message_count + refresh last_activity on subsequent ones.
        conn.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT :1 AS SK FROM DUAL) src ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET
                 m.LAST_ACTIVITY  = SYSTIMESTAMP,
                 m.MESSAGE_COUNT  = m.MESSAGE_COUNT + 1
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 1)",
            &[&session_key],
        )
        .map_err(std::io::Error::other)?;

        conn.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;

        let stmt = conn
            .execute(
                "DELETE FROM ZC_SESSIONS WHERE ID = (
                     SELECT MAX(ID) FROM ZC_SESSIONS WHERE SESSION_KEY = :1
                 )",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;

        let deleted = stmt.row_count().map_err(std::io::Error::other)? > 0;

        if deleted {
            conn.execute(
                "UPDATE ZC_SESSION_META
                 SET MESSAGE_COUNT = GREATEST(0, MESSAGE_COUNT - 1),
                     LAST_ACTIVITY  = SYSTIMESTAMP
                 WHERE SESSION_KEY = :1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
            conn.commit().map_err(std::io::Error::other)?;
        }

        Ok(deleted)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Ok(rows) = conn.query(
            "SELECT SESSION_KEY FROM ZC_SESSION_META ORDER BY LAST_ACTIVITY DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|row| row.get::<_, String>(0).ok())
            .collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Ok(rows) = conn.query(
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META ORDER BY LAST_ACTIVITY DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;

        // Collect stale session keys first so we can return an accurate count.
        let threshold_secs: i64 = i64::from(ttl_hours) * 3600;
        let Ok(rows) = conn.query(
            "SELECT SESSION_KEY FROM ZC_SESSION_META
             WHERE LAST_ACTIVITY < SYSTIMESTAMP - NUMTODSINTERVAL(:1, 'SECOND')",
            &[&threshold_secs],
        ) else {
            return Ok(0);
        };
        let keys: Vec<String> = rows
            .filter_map(|r| r.ok())
            .filter_map(|row| row.get::<_, String>(0).ok())
            .collect();

        let n = keys.len();
        for key in &keys {
            conn.execute("DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = :1", &[key])
                .map_err(std::io::Error::other)?;
            conn.execute(
                "DELETE FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
                &[key],
            )
            .map_err(std::io::Error::other)?;
        }
        if n > 0 {
            conn.commit().map_err(std::io::Error::other)?;
        }

        Ok(n)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Some(ref kw) = query.keyword else {
            return self.list_sessions_with_metadata();
        };
        let limit = query.limit.unwrap_or(50) as i64;
        // Oracle has no ILIKE; lower-case both sides for case-insensitive match.
        let pattern = format!("%{}%", kw.to_lowercase());
        let Ok(rows) = conn.query(
            "SELECT DISTINCT s.SESSION_KEY, m.NAME, m.CREATED_AT,
                    m.LAST_ACTIVITY, m.MESSAGE_COUNT,
                    m.AGENT_ALIAS, m.CHANNEL_ID, m.ROOM_ID, m.SENDER_ID
             FROM ZC_SESSIONS s
             JOIN ZC_SESSION_META m ON s.SESSION_KEY = m.SESSION_KEY
             WHERE LOWER(s.CONTENT) LIKE :1
             ORDER BY m.LAST_ACTIVITY DESC
             FETCH FIRST :2 ROWS ONLY",
            &[&pattern, &limit],
        ) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;

        conn.execute(
            "DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = :1",
            &[&session_key],
        )
        .map_err(std::io::Error::other)?;

        let stmt = conn
            .execute(
                "DELETE FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;

        let deleted = stmt.row_count().map_err(std::io::Error::other)? > 0;
        if deleted {
            conn.commit().map_err(std::io::Error::other)?;
        }
        Ok(deleted)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;
        conn.execute(
            "UPDATE ZC_SESSION_META SET NAME = :1 WHERE SESSION_KEY = :2",
            &[&name, &session_key],
        )
        .map_err(std::io::Error::other)?;
        conn.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut g) = self.pool.get() else {
            return Ok(None);
        };
        let conn = &mut g.0;
        let Ok(mut rows) = conn.query(
            "SELECT NAME FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
            &[&session_key],
        ) else {
            return Ok(None);
        };
        let Some(Ok(row)) = rows.next() else {
            return Ok(None);
        };
        Ok(row.get(0).unwrap_or(None))
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;
        conn.execute(
            "UPDATE ZC_SESSION_META
             SET STATE = :1, TURN_ID = :2, TURN_STARTED_AT = SYSTIMESTAMP
             WHERE SESSION_KEY = :3",
            &[&state, &turn_id, &session_key],
        )
        .map_err(std::io::Error::other)?;
        conn.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let Ok(mut g) = self.pool.get() else {
            return Ok(None);
        };
        let conn = &mut g.0;
        let Ok(mut rows) = conn.query(
            "SELECT STATE, TURN_ID, TURN_STARTED_AT
             FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
            &[&session_key],
        ) else {
            return Ok(None);
        };
        let Some(Ok(row)) = rows.next() else {
            return Ok(None);
        };
        let ts: Option<DateTime<Utc>> = row.get(2).unwrap_or(None);
        Ok(Some(SessionState {
            state: row.get(0).unwrap_or_else(|_| "idle".to_owned()),
            turn_id: row.get(1).unwrap_or(None),
            turn_started_at: ts,
        }))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Ok(rows) = conn.query(
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META WHERE STATE = 'running'
             ORDER BY TURN_STARTED_AT ASC NULLS LAST",
            &[],
        ) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let secs = threshold_secs as i64;
        let Ok(rows) = conn.query(
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META
             WHERE STATE = 'running'
               AND TURN_STARTED_AT < SYSTIMESTAMP - NUMTODSINTERVAL(:1, 'SECOND')
             ORDER BY TURN_STARTED_AT ASC",
            &[&secs],
        ) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let mut g = self.pool.get().ok()?;
        let conn = &mut g.0;
        let mut rows = conn
            .query(
                "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                        AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
                 FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
                &[&session_key],
            )
            .ok()?;
        let row = rows.next()?.ok()?;
        let count: i64 = row.get(4).unwrap_or(0);
        Some(SessionMetadata {
            key: row.get(0).unwrap_or_default(),
            name: row.get(1).unwrap_or(None),
            created_at: row.get(2).unwrap_or_else(|_| Utc::now()),
            last_activity: row.get(3).unwrap_or_else(|_| Utc::now()),
            message_count: count as usize,
            agent_alias: row.get(5).unwrap_or(None),
            channel_id: row.get(6).unwrap_or(None),
            room_id: row.get(7).unwrap_or(None),
            sender_id: row.get(8).unwrap_or(None),
        })
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(mut g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &mut g.0;
        let Ok(rows) = conn.query(
            "SELECT ROLE, CONTENT, CREATED_AT FROM ZC_SESSIONS
             WHERE SESSION_KEY = :1 ORDER BY ID ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok())
            .map(|row| {
                let ts: Option<DateTime<Utc>> = row.get(2).unwrap_or(None);
                TimestampedMessage {
                    message: ChatMessage {
                        role: row.get(0).unwrap_or_default(),
                        content: row.get(1).unwrap_or_default(),
                    },
                    created_at: ts,
                }
            })
            .collect()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;
        let stmt = conn
            .execute("DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = :1", &[&session_key])
            .map_err(std::io::Error::other)?;
        let n = stmt.row_count().map_err(std::io::Error::other)? as usize;
        if n > 0 {
            conn.execute(
                "UPDATE ZC_SESSION_META
                 SET MESSAGE_COUNT = 0, LAST_ACTIVITY = SYSTIMESTAMP
                 WHERE SESSION_KEY = :1",
                &[&session_key],
            )
            .map_err(std::io::Error::other)?;
            conn.commit().map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn set_session_agent_alias(
        &self,
        session_key: &str,
        agent_alias: &str,
    ) -> std::io::Result<()> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;
        let alias_val: Option<&str> =
            if agent_alias.is_empty() { None } else { Some(agent_alias) };
        conn.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT :1 AS SK, :2 AS ALIAS FROM DUAL) src ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET m.AGENT_ALIAS = src.ALIAS
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT, AGENT_ALIAS)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 0, src.ALIAS)",
            &[&session_key, &alias_val],
        )
        .map_err(std::io::Error::other)?;
        conn.commit().map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(mut g) = self.pool.get() else {
            return Ok(None);
        };
        let conn = &mut g.0;
        let Ok(mut rows) = conn.query(
            "SELECT AGENT_ALIAS FROM ZC_SESSION_META WHERE SESSION_KEY = :1",
            &[&session_key],
        ) else {
            return Ok(None);
        };
        let Some(Ok(row)) = rows.next() else {
            return Ok(None);
        };
        Ok(row.get(0).unwrap_or(None))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let mut g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &mut g.0;
        fn norm(v: Option<&str>) -> Option<&str> {
            v.map(str::trim).filter(|s| !s.is_empty())
        }
        let channel_id = norm(context.channel_id);
        let room_id = norm(context.room_id);
        let sender_id = norm(context.sender_id);
        // USING aliases the parameters so both WHEN MATCHED and WHEN NOT MATCHED
        // reference src.* without rebinding, avoiding OCI positional ambiguity.
        conn.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT :1 AS SK, :2 AS CID, :3 AS RID, :4 AS SID FROM DUAL) src
                 ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET
                 m.CHANNEL_ID = COALESCE(src.CID, m.CHANNEL_ID),
                 m.ROOM_ID    = COALESCE(src.RID, m.ROOM_ID),
                 m.SENDER_ID  = COALESCE(src.SID, m.SENDER_ID)
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                  CHANNEL_ID, ROOM_ID, SENDER_ID)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 0, src.CID, src.RID, src.SID)",
            &[&session_key, &channel_id, &room_id, &sender_id],
        )
        .map_err(std::io::Error::other)?;
        conn.commit().map_err(std::io::Error::other)?;
        Ok(())
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────

fn rows_to_metadata(rows: oracle::ResultSet<oracle::Row>) -> Vec<SessionMetadata> {
    rows.filter_map(|r| r.ok())
        .map(|row| {
            let count: i64 = row.get(4).unwrap_or(0);
            SessionMetadata {
                key: row.get(0).unwrap_or_default(),
                name: row.get(1).unwrap_or(None),
                created_at: row.get(2).unwrap_or_else(|_| Utc::now()),
                last_activity: row.get(3).unwrap_or_else(|_| Utc::now()),
                message_count: count as usize,
                agent_alias: row.get(5).unwrap_or(None),
                channel_id: row.get(6).unwrap_or(None),
                room_id: row.get(7).unwrap_or(None),
                sender_id: row.get(8).unwrap_or(None),
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_creds() -> Option<(String, String, String)> {
        let user = std::env::var("ZEROCLAW_TEST_ORACLE_USER").ok()?;
        let pass = std::env::var("ZEROCLAW_TEST_ORACLE_PASS").ok()?;
        let dsn  = std::env::var("ZEROCLAW_TEST_ORACLE_DSN").ok()?;
        Some((user, pass, dsn))
    }

    #[test]
    fn oracle_backend_round_trip() {
        let Some((user, pass, dsn)) = test_creds() else {
            eprintln!(
                "ZEROCLAW_TEST_ORACLE_{{USER,PASS,DSN}} not set — skipping oracle backend test"
            );
            return;
        };
        let backend = OracleSessionBackend::new(&user, &pass, &dsn, 2).expect("connect");
        let key = format!(
            "test-ora-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let msg = ChatMessage { role: "user".into(), content: "hello oracle".into() };
        backend.append(&key, &msg).expect("append");

        let loaded = backend.load(&key);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello oracle");

        assert!(backend.remove_last(&key).expect("remove_last"));
        assert!(backend.load(&key).is_empty());
        backend.delete_session(&key).expect("delete");
    }
}
