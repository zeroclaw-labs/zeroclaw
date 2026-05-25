//! IBM Db2 12.1.5-backed session persistence (Oracle compatibility mode).
//!
//! Connects to Db2 via the standard ODBC CLI driver using the [`odbc-api`]
//! crate.  The database must be started with Oracle compatibility enabled:
//!
//! ```sh
//! db2set DB2_COMPATIBILITY_VECTOR=ORA
//! db2stop && db2start
//! ```
//!
//! With Oracle compatibility active, Db2 accepts `DUAL`, `MERGE … USING DUAL`,
//! `SYSTIMESTAMP`, `GREATEST`, `FETCH FIRST n ROWS ONLY`, `NULLS LAST`,
//! `VARCHAR2`, `NUMBER`, and `CLOB` — the same syntax used in the Oracle 23ai
//! backend.  `NUMTODSINTERVAL` is **not** available; timestamp cutoffs are
//! computed on the Rust side and bound as ISO-8601 strings.
//!
//! Table and index DDL uses `CREATE TABLE IF NOT EXISTS` (native Db2 12.1+).
//! Index creation falls back gracefully on SQLSTATE 42710 (index already
//! exists).
//!
//! # Prerequisites
//!
//! The IBM Db2 CLI / ODBC driver must be installed at runtime.  Set
//! `DB2INSTANCE` and add the Db2 `lib64` directory to `LD_LIBRARY_PATH`
//! (Linux) so that `libdb2.so` / `libdb2o.so` is visible to the ODBC driver
//! manager.
//!
//! # Feature flag
//!
//! Requires `--features backend-db2` (adds `odbc-api` + `r2d2`).
//!
//! # Configuration
//!
//! ```toml
//! [sessions]
//! backend      = "db2"
//! db2_conn_str = "DSN=ZEROCLAW;UID=zeroclaw;PWD=secret;"
//! # — or — inline ODBC connection string without a configured DSN:
//! # db2_conn_str = "DRIVER={IBM Db2 ODBC DRIVER};DATABASE=ZEROCLAW;\
//! #                 HOSTNAME=primary;PORT=50000;PROTOCOL=TCPIP;\
//! #                 UID=zeroclaw;PWD=secret;"
//! pool_size    = 5
//! ```
//!
//! Supports Db2 HADR standbys, pureScale, and Db2 on Cloud — just point the
//! connection string at the correct endpoint.

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState, TimestampedMessage,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use odbc_api::{
    Connection, Cursor, Environment, ParameterCollectionRef, ResultSetMetadata,
    buffers::TextRowSet,
    parameter::{VarCharBox, VarCharSlice},
};
use r2d2::ManageConnection;
use r2d2::Pool;
use std::sync::LazyLock;
use zeroclaw_api::model_provider::ChatMessage;

// ── Global ODBC environment ───────────────────────────────────────────────

// ODBC requires a single long-lived `Environment` per process.  A `LazyLock`
// makes it safe to initialise once and reuse across threads.
static ODBC_ENV: LazyLock<Environment> = LazyLock::new(|| {
    // SAFETY: Environment::new() is safe to call exactly once per process;
    // LazyLock ensures this.
    Environment::new().expect("ODBC environment init failed")
});

// ── Connection wrapper ────────────────────────────────────────────────────

/// Wraps `odbc_api::Connection<'static>` so it can be pooled via `r2d2`.
///
/// # Safety
///
/// `Connection<'_>` is `Send` (documented in odbc-api) but not `Sync`.
/// `r2d2` provides exclusive access — only one thread holds a
/// `PooledConnection` at a time — so concurrent use via `Sync` is never
/// needed.  The `Sync` impl here is for `parking_lot::Mutex` compatibility
/// only; the Mutex itself prevents concurrent access.
struct Db2Conn(Connection<'static>);

// SAFETY: IBM Db2 CLI driver initialises with thread-safe (`SQL_THREADED`)
// mode; connections are safe to move between threads when protected by a
// mutex (which r2d2 enforces via exclusive PooledConnection handles).
unsafe impl Send for Db2Conn {}
unsafe impl Sync for Db2Conn {}

// ── r2d2 connection manager ───────────────────────────────────────────────

struct Db2Manager {
    conn_str: String,
}

impl ManageConnection for Db2Manager {
    type Connection = Db2Conn;
    type Error = odbc_api::Error;

    fn connect(&self) -> Result<Db2Conn, odbc_api::Error> {
        // ODBC_ENV is 'static, so the resulting Connection is also 'static.
        ODBC_ENV
            .connect_with_connection_string(&self.conn_str, Default::default())
            .map(Db2Conn)
    }

    fn is_valid(&self, conn: &mut Db2Conn) -> Result<(), odbc_api::Error> {
        conn.0.execute("SELECT 1 FROM SYSIBM.SYSDUMMY1", ())?;
        Ok(())
    }

    fn has_broken(&self, _: &mut Db2Conn) -> bool {
        false
    }
}

// ── Backend ───────────────────────────────────────────────────────────────

/// IBM Db2 12.1.5-backed session store (Oracle compatibility mode).
pub struct Db2SessionBackend {
    pool: Pool<Db2Manager>,
}

impl Db2SessionBackend {
    /// Connect and initialise the schema.
    ///
    /// `conn_str` is an ODBC connection string or DSN reference, e.g.
    /// `"DSN=ZEROCLAW;UID=zeroclaw;PWD=secret;"`.
    /// `pool_size` sets the maximum number of pooled ODBC connections.
    pub fn new(conn_str: &str, pool_size: u32) -> Result<Self> {
        let manager = Db2Manager {
            conn_str: conn_str.to_owned(),
        };
        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .context("failed to build Db2 ODBC connection pool")?;

        let mut guard = pool.get().context("failed to get initial Db2 connection")?;
        let conn = &mut guard.0;

        // CREATE TABLE IF NOT EXISTS is supported natively in Db2 12.1+.
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ZC_SESSIONS (
                ID          BIGINT    GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                SESSION_KEY VARCHAR(512)                  NOT NULL,
                ROLE        VARCHAR(64)                   NOT NULL,
                CONTENT     CLOB                          NOT NULL,
                CREATED_AT  TIMESTAMP WITH TIME ZONE      NOT NULL
             )",
            (),
        )
        .context("failed to create ZC_SESSIONS")?;

        // Db2 12 does not support CREATE INDEX IF NOT EXISTS; swallow
        // SQLSTATE 42710 (duplicate object).
        for idx_sql in [
            "CREATE INDEX IDX_ZC_SESS_KEY    ON ZC_SESSIONS(SESSION_KEY)",
            "CREATE INDEX IDX_ZC_SESS_KEY_ID ON ZC_SESSIONS(SESSION_KEY, ID)",
        ] {
            if let Err(e) = conn.execute(idx_sql, ()) {
                if !is_duplicate_object(&e) {
                    return Err(e).context("failed to create ZC_SESSIONS index")?;
                }
            }
        }

        conn.execute(
            "CREATE TABLE IF NOT EXISTS ZC_SESSION_META (
                SESSION_KEY      VARCHAR(512)            NOT NULL PRIMARY KEY,
                CREATED_AT       TIMESTAMP WITH TIME ZONE NOT NULL,
                LAST_ACTIVITY    TIMESTAMP WITH TIME ZONE NOT NULL,
                MESSAGE_COUNT    BIGINT   DEFAULT 0       NOT NULL,
                NAME             VARCHAR(1024),
                STATE            VARCHAR(64) DEFAULT 'idle' NOT NULL,
                TURN_ID          VARCHAR(512),
                TURN_STARTED_AT  TIMESTAMP WITH TIME ZONE,
                AGENT_ALIAS      VARCHAR(512),
                CHANNEL_ID       VARCHAR(512),
                ROOM_ID          VARCHAR(512),
                SENDER_ID        VARCHAR(512)
             )",
            (),
        )
        .context("failed to create ZC_SESSION_META")?;

        for idx_sql in [
            "CREATE INDEX IDX_ZC_SMETA_AGENT ON ZC_SESSION_META(AGENT_ALIAS)",
            "CREATE INDEX IDX_ZC_SMETA_CHAN  ON ZC_SESSION_META(CHANNEL_ID)",
            "CREATE INDEX IDX_ZC_SMETA_ROOM  ON ZC_SESSION_META(ROOM_ID)",
            "CREATE INDEX IDX_ZC_SMETA_SEND  ON ZC_SESSION_META(SENDER_ID)",
        ] {
            if let Err(e) = conn.execute(idx_sql, ()) {
                if !is_duplicate_object(&e) {
                    return Err(e).context("failed to create ZC_SESSION_META index")?;
                }
            }
        }

        Ok(Self { pool })
    }
}

/// Returns `true` when the ODBC error indicates the object already exists.
///
/// SQLSTATE 42710 is "duplicate object name" in Db2.  In Oracle
/// compatibility mode, Db2 may also surface -955 as the native code.
fn is_duplicate_object(e: &odbc_api::Error) -> bool {
    let s = e.to_string();
    s.contains("42710") || s.contains("SQL0955") || s.contains("-955")
}

// ── Row-reading helpers ───────────────────────────────────────────────────

/// Execute a SELECT and collect all rows as `Vec<Vec<Option<String>>>`.
///
/// Each inner `Vec` represents one row; each `Option<String>` is a column
/// value (None = SQL NULL).
fn select_rows(
    conn: &Connection<'static>,
    sql: &str,
    params: impl ParameterCollectionRef,
) -> Vec<Vec<Option<String>>> {
    let mut cursor = match conn.execute(sql, params) {
        Ok(Some(c)) => c,
        _ => return Vec::new(),
    };
    let num_cols = match cursor.num_result_cols() {
        Ok(n) => n as usize,
        Err(_) => return Vec::new(),
    };
    if num_cols == 0 {
        return Vec::new();
    }
    let row_set = match TextRowSet::for_cursor(256, &mut cursor, Some(8192)) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let mut block_cursor = match cursor.bind_buffer(row_set) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    while let Ok(Some(batch)) = block_cursor.fetch() {
        for row_idx in 0..batch.num_rows() {
            let row: Vec<Option<String>> = (0..num_cols)
                .map(|col| {
                    batch
                        .at_as_str(col, row_idx)
                        .ok()
                        .flatten()
                        .map(|s| s.to_string())
                })
                .collect();
            out.push(row);
        }
    }
    out
}

/// Extract column `idx` from a row as `String`.
fn col_str(row: &[Option<String>], idx: usize) -> String {
    row.get(idx)
        .and_then(|v| v.as_deref())
        .unwrap_or("")
        .to_owned()
}

/// Extract column `idx` from a row as `Option<String>`.
fn col_opt(row: &[Option<String>], idx: usize) -> Option<String> {
    row.get(idx)?.clone()
}

/// Parse an ISO-8601 timestamp string returned by Db2.
///
/// Db2 formats TIMESTAMP WITH TIME ZONE as `"YYYY-MM-DD HH:MM:SS+HH:MM"`.
fn parse_ts(row: &[Option<String>], idx: usize) -> DateTime<Utc> {
    row.get(idx)
        .and_then(|v| v.as_deref())
        .and_then(|s| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%z").ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now)
}

/// Format a `DateTime<Utc>` for binding as an ODBC timestamp string.
fn fmt_ts(dt: &DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Build `SessionMetadata` from a 9-column row
/// `(SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
///   AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID)`.
fn row_to_meta(row: &[Option<String>]) -> SessionMetadata {
    let count = col_str(row, 4).parse::<i64>().unwrap_or(0);
    SessionMetadata {
        key: col_str(row, 0),
        name: col_opt(row, 1),
        created_at: parse_ts(row, 2),
        last_activity: parse_ts(row, 3),
        message_count: count as usize,
        agent_alias: col_opt(row, 5),
        channel_id: col_opt(row, 6),
        room_id: col_opt(row, 7),
        sender_id: col_opt(row, 8),
    }
}

// ── SessionBackend impl ───────────────────────────────────────────────────

impl SessionBackend for Db2SessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        select_rows(
            &g.0,
            "SELECT ROLE, CONTENT FROM ZC_SESSIONS
             WHERE SESSION_KEY = ? ORDER BY ID ASC",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .into_iter()
        .map(|row| ChatMessage {
            role: col_str(&row, 0),
            content: col_str(&row, 1),
        })
        .collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &g.0;
        let now = fmt_ts(&Utc::now());

        let sk = VarCharSlice::new(session_key.as_bytes());
        let role = VarCharSlice::new(message.role.as_bytes());
        let content = VarCharSlice::new(message.content.as_bytes());
        let ts = VarCharSlice::new(now.as_bytes());
        conn.execute(
            "INSERT INTO ZC_SESSIONS (SESSION_KEY, ROLE, CONTENT, CREATED_AT)
             VALUES (?, ?, ?, TIMESTAMP(?))",
            (&sk, &role, &content, &ts),
        )
        .map_err(std::io::Error::other)?;

        // MERGE for upsert — same syntax works in Oracle compat mode.
        conn.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT ? AS SK FROM SYSIBM.SYSDUMMY1) src ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET
                 m.LAST_ACTIVITY  = SYSTIMESTAMP,
                 m.MESSAGE_COUNT  = m.MESSAGE_COUNT + 1
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 1)",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .map_err(std::io::Error::other)?;

        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &g.0;

        // Find the maximum ID for this session (None → nothing to remove).
        let rows = select_rows(
            conn,
            "SELECT MAX(ID) FROM ZC_SESSIONS WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        );
        let max_id = rows
            .into_iter()
            .next()
            .and_then(|r| col_opt(&r, 0))
            .and_then(|s| s.parse::<i64>().ok());

        let Some(id) = max_id else { return Ok(false) };

        conn.execute("DELETE FROM ZC_SESSIONS WHERE ID = ?", &id)
            .map_err(std::io::Error::other)?;

        conn.execute(
            "UPDATE ZC_SESSION_META
             SET MESSAGE_COUNT = GREATEST(0, MESSAGE_COUNT - 1),
                 LAST_ACTIVITY  = SYSTIMESTAMP
             WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        select_rows(
            &g.0,
            "SELECT SESSION_KEY FROM ZC_SESSION_META ORDER BY LAST_ACTIVITY DESC",
            (),
        )
        .into_iter()
        .map(|row| col_str(&row, 0))
        .collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        select_rows(
            &g.0,
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META ORDER BY LAST_ACTIVITY DESC",
            (),
        )
        .into_iter()
        .map(|row| row_to_meta(&row))
        .collect()
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &g.0;

        // Compute the cutoff timestamp in Rust — Db2 does not support
        // NUMTODSINTERVAL even in Oracle compat mode.
        let cutoff = fmt_ts(&(Utc::now() - chrono::Duration::hours(i64::from(ttl_hours))));

        let stale: Vec<String> = select_rows(
            conn,
            "SELECT SESSION_KEY FROM ZC_SESSION_META
             WHERE LAST_ACTIVITY < TIMESTAMP(?)",
            &VarCharSlice::new(cutoff.as_bytes()),
        )
        .into_iter()
        .map(|row| col_str(&row, 0))
        .collect();

        let n = stale.len();
        for key in &stale {
            conn.execute(
                "DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = ?",
                &VarCharSlice::new(key.as_bytes()),
            )
            .map_err(std::io::Error::other)?;
            conn.execute(
                "DELETE FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
                &VarCharSlice::new(key.as_bytes()),
            )
            .map_err(std::io::Error::other)?;
        }

        Ok(n)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        let conn = &g.0;
        let Some(ref kw) = query.keyword else {
            return self.list_sessions_with_metadata();
        };
        let limit = query.limit.unwrap_or(50) as i64;
        // Db2 does not have ILIKE; use LOWER() on both sides.
        let pattern = format!("%{}%", kw.to_lowercase());
        let p = VarCharSlice::new(pattern.as_bytes());
        select_rows(
            conn,
            "SELECT DISTINCT s.SESSION_KEY, m.NAME, m.CREATED_AT,
                    m.LAST_ACTIVITY, m.MESSAGE_COUNT,
                    m.AGENT_ALIAS, m.CHANNEL_ID, m.ROOM_ID, m.SENDER_ID
             FROM ZC_SESSIONS s
             JOIN ZC_SESSION_META m ON s.SESSION_KEY = m.SESSION_KEY
             WHERE LOWER(CAST(s.CONTENT AS VARCHAR(8192))) LIKE ?
             ORDER BY m.LAST_ACTIVITY DESC
             FETCH FIRST ? ROWS ONLY",
            (&p, &limit),
        )
        .into_iter()
        .map(|row| row_to_meta(&row))
        .collect()
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &g.0;

        // Check existence before delete so we can return an accurate bool.
        let exists = select_rows(
            conn,
            "SELECT 1 FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .first()
        .is_some();

        if !exists {
            return Ok(false);
        }

        conn.execute(
            "DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .map_err(std::io::Error::other)?;
        conn.execute(
            "DELETE FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .map_err(std::io::Error::other)?;

        Ok(true)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let n = VarCharSlice::new(name.as_bytes());
        let sk = VarCharSlice::new(session_key.as_bytes());
        g.0.execute(
            "UPDATE ZC_SESSION_META SET NAME = ? WHERE SESSION_KEY = ?",
            (&n, &sk),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(g) = self.pool.get() else {
            return Ok(None);
        };
        let rows = select_rows(
            &g.0,
            "SELECT NAME FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        );
        Ok(rows.into_iter().next().and_then(|r| col_opt(&r, 0)))
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let turn_id_val = match turn_id {
            Some(s) => VarCharBox::from_string(s.to_string()),
            None => VarCharBox::null(),
        };
        let st = VarCharSlice::new(state.as_bytes());
        let sk = VarCharSlice::new(session_key.as_bytes());
        g.0.execute(
            "UPDATE ZC_SESSION_META
             SET STATE = ?, TURN_ID = ?, TURN_STARTED_AT = SYSTIMESTAMP
             WHERE SESSION_KEY = ?",
            (&st, &turn_id_val, &sk),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let Ok(g) = self.pool.get() else {
            return Ok(None);
        };
        let rows = select_rows(
            &g.0,
            "SELECT STATE, TURN_ID, TURN_STARTED_AT
             FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        );
        let row = match rows.into_iter().next() {
            Some(r) => r,
            None => return Ok(None),
        };
        // Parse optional TURN_STARTED_AT.
        let turn_started_at: Option<DateTime<Utc>> = row
            .get(2)
            .and_then(|v| v.as_deref())
            .and_then(|s| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%z").ok())
            .map(|dt| dt.with_timezone(&Utc));
        Ok(Some(SessionState {
            state: col_str(&row, 0),
            turn_id: col_opt(&row, 1),
            turn_started_at,
        }))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        select_rows(
            &g.0,
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META WHERE STATE = 'running'
             ORDER BY TURN_STARTED_AT ASC NULLS LAST",
            (),
        )
        .into_iter()
        .map(|row| row_to_meta(&row))
        .collect()
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        // Compute cutoff in Rust — Db2 does not support NUMTODSINTERVAL.
        let cutoff = fmt_ts(&(Utc::now() - chrono::Duration::seconds(threshold_secs as i64)));
        select_rows(
            &g.0,
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META
             WHERE STATE = 'running'
               AND TURN_STARTED_AT < TIMESTAMP(?)
             ORDER BY TURN_STARTED_AT ASC",
            &VarCharSlice::new(cutoff.as_bytes()),
        )
        .into_iter()
        .map(|row| row_to_meta(&row))
        .collect()
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let g = self.pool.get().ok()?;
        let rows = select_rows(
            &g.0,
            "SELECT SESSION_KEY, NAME, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                    AGENT_ALIAS, CHANNEL_ID, ROOM_ID, SENDER_ID
             FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        );
        rows.into_iter().next().map(|row| row_to_meta(&row))
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(g) = self.pool.get() else {
            return Vec::new();
        };
        select_rows(
            &g.0,
            "SELECT ROLE, CONTENT, CREATED_AT FROM ZC_SESSIONS
             WHERE SESSION_KEY = ? ORDER BY ID ASC",
            &VarCharSlice::new(session_key.as_bytes()),
        )
        .into_iter()
        .map(|row| {
            // parse_ts handles Db2's "YYYY-MM-DD HH:MM:SS±HH:MM" format.
            let ts = row
                .get(2)
                .and_then(|v| v.as_deref())
                .and_then(|s| DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%z").ok())
                .map(|dt| dt.with_timezone(&Utc));
            TimestampedMessage {
                message: ChatMessage {
                    role: col_str(&row, 0),
                    content: col_str(&row, 1),
                },
                created_at: ts,
            }
        })
        .collect()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let conn = &g.0;
        let sk = VarCharSlice::new(session_key.as_bytes());
        // Count before delete so we can return an accurate value.
        let count_rows = select_rows(
            conn,
            "SELECT COUNT(*) FROM ZC_SESSIONS WHERE SESSION_KEY = ?",
            &sk,
        );
        let n = count_rows
            .into_iter()
            .next()
            .and_then(|r| col_opt(&r, 0))
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        if n > 0 {
            conn.execute(
                "DELETE FROM ZC_SESSIONS WHERE SESSION_KEY = ?",
                &VarCharSlice::new(session_key.as_bytes()),
            )
            .map_err(std::io::Error::other)?;
            conn.execute(
                "UPDATE ZC_SESSION_META
                 SET MESSAGE_COUNT = 0, LAST_ACTIVITY = SYSTIMESTAMP
                 WHERE SESSION_KEY = ?",
                &VarCharSlice::new(session_key.as_bytes()),
            )
            .map_err(std::io::Error::other)?;
        }
        Ok(n)
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        let alias_val = if agent_alias.is_empty() {
            VarCharBox::null()
        } else {
            VarCharBox::from_string(agent_alias.to_string())
        };
        let sk = VarCharSlice::new(session_key.as_bytes());
        // USING aliases both parameters so WHEN NOT MATCHED INSERT can reference src.*
        // without re-binding — ODBC positional `?` markers are consumed left-to-right.
        g.0.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT ? AS SK, ? AS ALIAS FROM SYSIBM.SYSDUMMY1) src
                 ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET m.AGENT_ALIAS = src.ALIAS
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT, AGENT_ALIAS)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 0, src.ALIAS)",
            (&sk, &alias_val),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let Ok(g) = self.pool.get() else {
            return Ok(None);
        };
        let rows = select_rows(
            &g.0,
            "SELECT AGENT_ALIAS FROM ZC_SESSION_META WHERE SESSION_KEY = ?",
            &VarCharSlice::new(session_key.as_bytes()),
        );
        Ok(rows.into_iter().next().and_then(|r| col_opt(&r, 0)))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let g = self.pool.get().map_err(std::io::Error::other)?;
        fn to_varchar(v: Option<&str>) -> VarCharBox {
            match v.map(str::trim).filter(|s| !s.is_empty()) {
                Some(s) => VarCharBox::from_string(s.to_string()),
                None => VarCharBox::null(),
            }
        }
        let channel_id = to_varchar(context.channel_id);
        let room_id = to_varchar(context.room_id);
        let sender_id = to_varchar(context.sender_id);
        let sk = VarCharSlice::new(session_key.as_bytes());
        // Four params in USING; all WHEN clauses reference src.* aliases.
        g.0.execute(
            "MERGE INTO ZC_SESSION_META m
             USING (SELECT ? AS SK, ? AS CID, ? AS RID, ? AS SID
                    FROM SYSIBM.SYSDUMMY1) src ON (m.SESSION_KEY = src.SK)
             WHEN MATCHED THEN UPDATE SET
                 m.CHANNEL_ID = COALESCE(src.CID, m.CHANNEL_ID),
                 m.ROOM_ID    = COALESCE(src.RID, m.ROOM_ID),
                 m.SENDER_ID  = COALESCE(src.SID, m.SENDER_ID)
             WHEN NOT MATCHED THEN INSERT
                 (SESSION_KEY, CREATED_AT, LAST_ACTIVITY, MESSAGE_COUNT,
                  CHANNEL_ID, ROOM_ID, SENDER_ID)
             VALUES (src.SK, SYSTIMESTAMP, SYSTIMESTAMP, 0, src.CID, src.RID, src.SID)",
            (&sk, &channel_id, &room_id, &sender_id),
        )
        .map_err(std::io::Error::other)?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn_str() -> Option<String> {
        std::env::var("ZEROCLAW_TEST_DB2_CONN_STR").ok()
    }

    #[test]
    fn db2_backend_round_trip() {
        let Some(conn_str) = test_conn_str() else {
            eprintln!(
                "ZEROCLAW_TEST_DB2_CONN_STR not set — skipping Db2 backend test\n\
                 Example: DSN=ZEROCLAW;UID=zeroclaw;PWD=secret;"
            );
            return;
        };
        let backend = Db2SessionBackend::new(&conn_str, 2).expect("connect");
        let key = format!(
            "test-db2-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let msg = ChatMessage {
            role: "user".into(),
            content: "hello db2".into(),
        };
        backend.append(&key, &msg).expect("append");

        let loaded = backend.load(&key);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "hello db2");

        assert!(backend.remove_last(&key).expect("remove_last"));
        assert!(backend.load(&key).is_empty());
        backend.delete_session(&key).expect("delete");
    }
}
