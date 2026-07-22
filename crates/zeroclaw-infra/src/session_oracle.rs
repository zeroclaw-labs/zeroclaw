//! Oracle-backed session persistence.
//!
//! The backend stores ordered messages in `sessions`; that table is the source
//! of truth for message content and timestamps. `session_metadata` is the source
//! of truth for names, routing attribution, activity counters, and turn state.
//!
//! The synchronous [`oracle`] crate embeds ODPI-C, which is compiled with a C
//! compiler, and loads Oracle Client 11.2 or newer at runtime. Operators must
//! install Oracle Instant Client or a full Oracle Client and make its shared
//! libraries discoverable by the platform loader. The Oracle Client package may
//! have additional operating-system dependencies (commonly `libaio` on Linux).
//! The driver uses OCI's native homogeneous session pool.

use std::fmt::Display;
use std::path::Path;

use chrono::{DateTime, Utc};
use oracle::pool::{GetMode, Pool, PoolBuilder};
use oracle::sql_type::OracleType;
use oracle::{Connection, Row};
use zeroclaw_api::model_provider::ChatMessage;

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState, TimestampedMessage,
};

/// Synchronous Oracle session store backed by an OCI session pool.
pub struct OracleSessionBackend {
    pool: Pool,
}

impl OracleSessionBackend {
    /// Construct the backend from the canonical channel configuration
    /// environment overrides and initialize its schema.
    pub fn new(workspace_dir: &Path, pool_size: u32) -> std::io::Result<Self> {
        let _ = workspace_dir;
        let credentials = read_oracle_credentials()?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session_backend=oracle requires ZEROCLAW_channels__oracle_user, \
                 ZEROCLAW_channels__oracle_password, and ZEROCLAW_channels__oracle_dsn \
                 (or ZEROCLAW_TEST_ORACLE_URL in tests) to be set. Populate the \
                 channels.oracle_* fields or inject them through the standard \
                 dotted-path environment overrides.",
            )
        })?;
        Self::new_with_credentials(
            &credentials.user,
            &credentials.password,
            &credentials.dsn,
            pool_size,
        )
    }

    fn new_with_credentials(
        user: &str,
        password: &str,
        dsn: &str,
        pool_size: u32,
    ) -> std::io::Result<Self> {
        let max_connections = pool_size.max(1);
        let mut builder = PoolBuilder::new(user, password, dsn);
        builder
            .min_connections(1)
            .max_connections(max_connections)
            .connection_increment(u32::from(max_connections > 1))
            .get_mode(GetMode::TimedWait(std::time::Duration::from_secs(30)))
            .driver_name("zeroclaw-session");
        let pool = builder
            .build()
            .map_err(|error| oracle_error("build connection pool", error))?;
        let backend = Self { pool };
        backend.ensure_schema()?;
        Ok(backend)
    }

    fn conn(&self) -> std::io::Result<Connection> {
        self.pool
            .get()
            .map_err(|error| oracle_error("checkout pooled connection", error))
    }

    fn with_transaction<T>(
        &self,
        context: &str,
        operation: impl FnOnce(&Connection) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let conn = self.conn()?;
        match operation(&conn) {
            Ok(value) => match conn.commit() {
                Ok(()) => Ok(value),
                Err(commit_error) => {
                    let rollback = conn.rollback();
                    Err(transaction_error(context, commit_error, rollback.err()))
                }
            },
            Err(operation_error) => match conn.rollback() {
                Ok(()) => Err(operation_error),
                Err(rollback_error) => Err(std::io::Error::other(format!(
                    "{operation_error}; rollback after {context} also failed: {rollback_error}"
                ))),
            },
        }
    }

    fn ensure_schema(&self) -> std::io::Result<()> {
        let conn = self.conn()?;
        for ddl in [
            "CREATE TABLE sessions (
                id          NUMBER GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
                session_key VARCHAR2(512 CHAR)            NOT NULL,
                role        VARCHAR2(64 CHAR)             NOT NULL,
                content     CLOB                          NOT NULL,
                created_at  TIMESTAMP WITH TIME ZONE      DEFAULT SYSTIMESTAMP NOT NULL
             )",
            "CREATE INDEX idx_sessions_key ON sessions(session_key)",
            "CREATE INDEX idx_sessions_key_id ON sessions(session_key, id)",
            "CREATE TABLE session_metadata (
                session_key      VARCHAR2(512 CHAR)       PRIMARY KEY,
                created_at       TIMESTAMP WITH TIME ZONE DEFAULT SYSTIMESTAMP NOT NULL,
                last_activity    TIMESTAMP WITH TIME ZONE DEFAULT SYSTIMESTAMP NOT NULL,
                message_count    NUMBER(19)               DEFAULT 0 NOT NULL,
                name             VARCHAR2(1024 CHAR),
                state            VARCHAR2(64 CHAR)        DEFAULT 'idle' NOT NULL,
                turn_id          VARCHAR2(512 CHAR),
                turn_started_at  TIMESTAMP WITH TIME ZONE,
                agent_alias      VARCHAR2(512 CHAR),
                channel_id       VARCHAR2(512 CHAR),
                room_id          VARCHAR2(512 CHAR),
                sender_id        VARCHAR2(512 CHAR)
             )",
        ] {
            execute_ddl_ignoring(&conn, ddl, &[955])?;
        }

        for ddl in [
            "ALTER TABLE session_metadata ADD (name VARCHAR2(1024 CHAR))",
            "ALTER TABLE session_metadata ADD (state VARCHAR2(64 CHAR) DEFAULT 'idle' NOT NULL)",
            "ALTER TABLE session_metadata ADD (turn_id VARCHAR2(512 CHAR))",
            "ALTER TABLE session_metadata ADD (turn_started_at TIMESTAMP WITH TIME ZONE)",
            "ALTER TABLE session_metadata ADD (agent_alias VARCHAR2(512 CHAR))",
            "ALTER TABLE session_metadata ADD (channel_id VARCHAR2(512 CHAR))",
            "ALTER TABLE session_metadata ADD (room_id VARCHAR2(512 CHAR))",
            "ALTER TABLE session_metadata ADD (sender_id VARCHAR2(512 CHAR))",
        ] {
            execute_ddl_ignoring(&conn, ddl, &[1430])?;
        }

        for ddl in [
            "CREATE INDEX idx_smeta_agent ON session_metadata(agent_alias)",
            "CREATE INDEX idx_smeta_channel ON session_metadata(channel_id)",
            "CREATE INDEX idx_smeta_room ON session_metadata(room_id)",
            "CREATE INDEX idx_smeta_sender ON session_metadata(sender_id)",
        ] {
            execute_ddl_ignoring(&conn, ddl, &[955])?;
        }

        self.ensure_oracle_text_index(&conn)
    }

    fn ensure_oracle_text_index(&self, conn: &Connection) -> std::io::Result<()> {
        let exists: i64 = conn
            .query_row_as(
                "SELECT COUNT(*) FROM user_indexes \
                 WHERE index_name = 'IDX_SESSIONS_CONTENT_CTX'",
                &[],
            )
            .map_err(|error| oracle_error("inspect Oracle Text index", error))?;
        if exists > 0 {
            return Ok(());
        }

        if let Err(error) = conn.execute(
            "CREATE INDEX idx_sessions_content_ctx ON sessions(content) \
             INDEXTYPE IS CTXSYS.CONTEXT PARAMETERS ('SYNC (ON COMMIT)')",
            &[],
        ) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": error.to_string()})),
                "session_backend=oracle: Oracle Text CONTEXT index is unavailable; search will use a case-insensitive CLOB scan"
            );
        }
        Ok(())
    }

    fn oracle_text_index_available(&self, conn: &Connection) -> bool {
        conn.query_row_as::<i64>(
            "SELECT COUNT(*) FROM user_indexes \
             WHERE index_name = 'IDX_SESSIONS_CONTENT_CTX' AND status = 'VALID'",
            &[],
        )
        .is_ok_and(|count| count > 0)
    }

    fn metadata_columns() -> &'static str {
        "session_key, name, created_at, last_activity, message_count, \
         agent_alias, channel_id, room_id, sender_id"
    }

    fn search_oracle_text(
        &self,
        conn: &Connection,
        text_query: &str,
        limit: i64,
    ) -> oracle::Result<Vec<SessionMetadata>> {
        let sql = format!(
            "SELECT {} FROM session_metadata m \
             WHERE EXISTS ( \
                 SELECT 1 FROM sessions s \
                 WHERE s.session_key = m.session_key \
                   AND CONTAINS(s.content, :1) > 0 \
             ) \
             ORDER BY m.last_activity DESC FETCH FIRST :2 ROWS ONLY",
            Self::metadata_columns()
        );
        let rows = conn.query(&sql, &[&text_query, &limit])?;
        Ok(rows_to_metadata(rows))
    }

    fn search_like(
        &self,
        conn: &Connection,
        keyword: &str,
        limit: i64,
    ) -> oracle::Result<Vec<SessionMetadata>> {
        let sql = format!(
            "SELECT {} FROM session_metadata m \
             WHERE EXISTS ( \
                 SELECT 1 FROM sessions s \
                 WHERE s.session_key = m.session_key \
                   AND LOWER(s.content) LIKE LOWER(CAST(:1 AS VARCHAR2(4000 CHAR))) \
                       ESCAPE '\\' \
             ) \
             ORDER BY m.last_activity DESC FETCH FIRST :2 ROWS ONLY",
            Self::metadata_columns()
        );
        let pattern = build_like_pattern(keyword);
        let rows = conn.query(&sql, &[&pattern, &limit])?;
        Ok(rows_to_metadata(rows))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OracleCredentials {
    user: String,
    password: String,
    dsn: String,
}

fn oracle_error(context: &str, error: impl Display) -> std::io::Error {
    std::io::Error::other(format!(
        "session_backend=oracle: failed to {context}: {error}"
    ))
}

fn transaction_error(
    context: &str,
    commit_error: impl Display,
    rollback_error: Option<oracle::Error>,
) -> std::io::Error {
    let rollback = rollback_error
        .map(|error| format!("; rollback also failed: {error}"))
        .unwrap_or_default();
    oracle_error(context, format!("commit failed: {commit_error}{rollback}"))
}

fn execute_ddl_ignoring(
    conn: &Connection,
    ddl: &str,
    ignored_ora_codes: &[i32],
) -> std::io::Result<()> {
    match conn.execute(ddl, &[]) {
        Ok(_) => Ok(()),
        Err(error)
            if error
                .oci_code()
                .is_some_and(|code| ignored_ora_codes.contains(&code)) =>
        {
            Ok(())
        }
        Err(error) => Err(oracle_error("initialize schema", error)),
    }
}

fn rows_to_metadata<I>(rows: I) -> Vec<SessionMetadata>
where
    I: IntoIterator<Item = oracle::Result<Row>>,
{
    rows.into_iter()
        .filter_map(Result::ok)
        .filter_map(|row| row_to_metadata(&row).ok())
        .collect()
}

fn row_to_metadata(row: &Row) -> oracle::Result<SessionMetadata> {
    let count: i64 = row.get(4)?;
    Ok(SessionMetadata {
        key: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        last_activity: row.get(3)?,
        message_count: usize::try_from(count.max(0)).unwrap_or(usize::MAX),
        agent_alias: row.get(5)?,
        channel_id: row.get(6)?,
        room_id: row.get(7)?,
        sender_id: row.get(8)?,
    })
}

fn normalize_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_test_oracle_url(url: &str) -> std::io::Result<OracleCredentials> {
    let (user, remainder) = url.split_once('/').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ZEROCLAW_TEST_ORACLE_URL must use user/password@host:port/service format",
        )
    })?;
    let (password, dsn) = remainder.rsplit_once('@').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ZEROCLAW_TEST_ORACLE_URL must use user/password@host:port/service format",
        )
    })?;
    if user.trim().is_empty() || password.is_empty() || dsn.trim().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "ZEROCLAW_TEST_ORACLE_URL contains an empty user, password, or DSN",
        ));
    }
    Ok(OracleCredentials {
        user: user.to_string(),
        password: password.to_string(),
        dsn: dsn.to_string(),
    })
}

fn read_oracle_credentials() -> std::io::Result<Option<OracleCredentials>> {
    let user = std::env::var("ZEROCLAW_channels__oracle_user").ok();
    let password = std::env::var("ZEROCLAW_channels__oracle_password").ok();
    let dsn = std::env::var("ZEROCLAW_channels__oracle_dsn").ok();
    if user.is_some() || password.is_some() || dsn.is_some() {
        let credentials = OracleCredentials {
            user: user.unwrap_or_default(),
            password: password.unwrap_or_default(),
            dsn: dsn.unwrap_or_default(),
        };
        if credentials.user.trim().is_empty()
            || credentials.password.is_empty()
            || credentials.dsn.trim().is_empty()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Oracle session configuration is incomplete; oracle_user, \
                 oracle_password, and oracle_dsn must all be non-empty",
            ));
        }
        return Ok(Some(credentials));
    }

    match std::env::var("ZEROCLAW_TEST_ORACLE_URL") {
        Ok(value) if !value.trim().is_empty() => parse_test_oracle_url(value.trim()).map(Some),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ZEROCLAW_TEST_ORACLE_URL is not valid Unicode: {error}"),
        )),
    }
}

pub(crate) fn read_pool_size() -> u32 {
    std::env::var("ZEROCLAW_channels__pool_size")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(5)
}

fn build_like_pattern(keyword: &str) -> String {
    let mut pattern = String::with_capacity(keyword.len() + 2);
    pattern.push('%');
    for character in keyword.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.extend(character.to_lowercase());
    }
    pattern.push('%');
    pattern
}

fn build_oracle_text_query(keyword: &str) -> String {
    keyword
        .split_whitespace()
        .filter_map(|token| {
            let term: String = token
                .chars()
                .filter(|character| character.is_alphanumeric() || *character == '_')
                .flat_map(char::to_lowercase)
                .collect();
            (!term.is_empty()).then(|| format!("{{{term}}}"))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

impl SessionBackend for OracleSessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content FROM sessions \
             WHERE session_key = :1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .filter_map(|row| {
                Some(ChatMessage {
                    role: row.get(0).ok()?,
                    content: row.get(1).ok()?,
                })
            })
            .collect()
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT role, content, created_at FROM sessions \
             WHERE session_key = :1 ORDER BY id ASC",
            &[&session_key],
        ) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .filter_map(|row| {
                Some(TimestampedMessage {
                    message: ChatMessage {
                        role: row.get(0).ok()?,
                        content: row.get(1).ok()?,
                    },
                    created_at: Some(row.get(2).ok()?),
                })
            })
            .collect()
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        self.with_transaction("append message", |conn| {
            let content = message.content.as_str();
            let content_clob = (&content, &OracleType::CLOB);
            conn.execute(
                "INSERT INTO sessions (session_key, role, content, created_at) \
                 VALUES (:1, :2, :3, SYSTIMESTAMP)",
                &[&session_key, &message.role, &content_clob],
            )
            .map_err(|error| oracle_error("append message", error))?;
            conn.execute(
                "MERGE INTO session_metadata m \
                 USING (SELECT :1 AS session_key FROM dual) src \
                    ON (m.session_key = src.session_key) \
                 WHEN MATCHED THEN UPDATE SET \
                    m.last_activity = SYSTIMESTAMP, \
                    m.message_count = m.message_count + 1 \
                 WHEN NOT MATCHED THEN INSERT \
                    (session_key, created_at, last_activity, message_count) \
                 VALUES (src.session_key, SYSTIMESTAMP, SYSTIMESTAMP, 1)",
                &[&session_key],
            )
            .map_err(|error| oracle_error("update metadata after append", error))?;
            Ok(())
        })
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        self.with_transaction("remove last message", |conn| {
            let statement = conn
                .execute(
                    "DELETE FROM sessions WHERE id = ( \
                         SELECT MAX(id) FROM sessions WHERE session_key = :1 \
                     )",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("remove last message", error))?;
            let removed = statement
                .row_count()
                .map_err(|error| oracle_error("read removed message count", error))?
                > 0;
            if removed {
                conn.execute(
                    "UPDATE session_metadata SET \
                         message_count = GREATEST(message_count - 1, 0), \
                         last_activity = SYSTIMESTAMP \
                     WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("update metadata after remove", error))?;
            }
            Ok(removed)
        })
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        self.with_transaction("update last message", |conn| {
            let content = message.content.as_str();
            let content_clob = (&content, &OracleType::CLOB);
            let statement = conn
                .execute(
                    "UPDATE sessions SET role = :1, content = :2 WHERE id = ( \
                         SELECT MAX(id) FROM sessions WHERE session_key = :3 \
                     )",
                    &[&message.role, &content_clob, &session_key],
                )
                .map_err(|error| oracle_error("update last message", error))?;
            let updated = statement
                .row_count()
                .map_err(|error| oracle_error("read updated message count", error))?
                > 0;
            if updated {
                conn.execute(
                    "UPDATE session_metadata SET last_activity = SYSTIMESTAMP \
                     WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("update metadata after message update", error))?;
            }
            Ok(updated)
        })
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let Ok(rows) = conn.query(
            "SELECT session_key FROM session_metadata ORDER BY last_activity DESC",
            &[],
        ) else {
            return Vec::new();
        };
        rows.filter_map(Result::ok)
            .filter_map(|row| row.get(0).ok())
            .collect()
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "SELECT {} FROM session_metadata ORDER BY last_activity DESC",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&sql, &[]) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::hours(i64::from(ttl_hours));
        self.with_transaction("clean up stale sessions", |conn| {
            let rows = conn
                .query(
                    "SELECT session_key FROM session_metadata \
                     WHERE last_activity < :1 FOR UPDATE",
                    &[&cutoff],
                )
                .map_err(|error| oracle_error("select stale sessions", error))?;
            let keys: Vec<String> = rows
                .filter_map(Result::ok)
                .filter_map(|row| row.get(0).ok())
                .collect();
            for key in &keys {
                conn.execute("DELETE FROM sessions WHERE session_key = :1", &[key])
                    .map_err(|error| oracle_error("delete stale session messages", error))?;
                conn.execute(
                    "DELETE FROM session_metadata WHERE session_key = :1",
                    &[key],
                )
                .map_err(|error| oracle_error("delete stale session metadata", error))?;
            }
            Ok(keys.len())
        })
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = query.keyword.as_deref().map(str::trim) else {
            return self.list_sessions_with_metadata();
        };
        if keyword.is_empty() {
            return Vec::new();
        }
        let limit = i64::try_from(query.limit.unwrap_or(50)).unwrap_or(i64::MAX);
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };

        let text_query = build_oracle_text_query(keyword);
        if !text_query.is_empty() && self.oracle_text_index_available(&conn) {
            match self.search_oracle_text(&conn, &text_query, limit) {
                Ok(results) => return results,
                Err(error) => ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({"error": error.to_string()})),
                    "session_backend=oracle: Oracle Text query failed; retrying with a case-insensitive CLOB scan"
                ),
            }
        }
        self.search_like(&conn, keyword, limit).unwrap_or_default()
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        self.with_transaction("clear session messages", |conn| {
            let statement = conn
                .execute(
                    "DELETE FROM sessions WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("clear session messages", error))?;
            let removed = statement
                .row_count()
                .map_err(|error| oracle_error("read cleared message count", error))?;
            if removed > 0 {
                conn.execute(
                    "UPDATE session_metadata SET \
                         message_count = 0, last_activity = SYSTIMESTAMP \
                     WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("update metadata after clearing messages", error))?;
            }
            usize::try_from(removed)
                .map_err(|error| oracle_error("convert cleared message count", error))
        })
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        self.with_transaction("delete session", |conn| {
            conn.execute(
                "DELETE FROM sessions WHERE session_key = :1",
                &[&session_key],
            )
            .map_err(|error| oracle_error("delete session messages", error))?;
            let statement = conn
                .execute(
                    "DELETE FROM session_metadata WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("delete session metadata", error))?;
            statement
                .row_count()
                .map(|count| count > 0)
                .map_err(|error| oracle_error("read deleted session count", error))
        })
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        self.with_transaction("clear agent attribution", |conn| {
            let statement = conn
                .execute(
                    "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = :1",
                    &[&agent_alias],
                )
                .map_err(|error| oracle_error("clear agent attribution", error))?;
            let affected = statement
                .row_count()
                .map_err(|error| oracle_error("read cleared attribution count", error))?;
            usize::try_from(affected)
                .map_err(|error| oracle_error("convert cleared attribution count", error))
        })
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        self.with_transaction("rename agent attribution", |conn| {
            let statement = conn
                .execute(
                    "UPDATE session_metadata SET agent_alias = :1 WHERE agent_alias = :2",
                    &[&to, &from],
                )
                .map_err(|error| oracle_error("rename agent attribution", error))?;
            let affected = statement
                .row_count()
                .map_err(|error| oracle_error("read renamed attribution count", error))?;
            usize::try_from(affected)
                .map_err(|error| oracle_error("convert renamed attribution count", error))
        })
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let count: i64 = self
            .conn()?
            .query_row_as(
                "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = :1",
                &[&agent_alias],
            )
            .map_err(|error| oracle_error("count agent attribution", error))?;
        usize::try_from(count.max(0))
            .map_err(|error| oracle_error("convert agent attribution count", error))
    }

    fn session_exists(&self, session_key: &str) -> bool {
        self.conn()
            .and_then(|conn| {
                conn.query_row_as::<i64>(
                    "SELECT COUNT(*) FROM session_metadata WHERE session_key = :1",
                    &[&session_key],
                )
                .map_err(|error| oracle_error("check session existence", error))
            })
            .is_ok_and(|count| count > 0)
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let value = (!name.is_empty()).then_some(name);
        self.with_transaction("set session name", |conn| {
            conn.execute(
                "UPDATE session_metadata SET name = :1 WHERE session_key = :2",
                &[&value, &session_key],
            )
            .map_err(|error| oracle_error("set session name", error))?;
            Ok(())
        })
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn()?;
        match conn.query_row_as(
            "SELECT name FROM session_metadata WHERE session_key = :1",
            &[&session_key],
        ) {
            Ok(value) => Ok(value),
            Err(error) if error.kind() == oracle::ErrorKind::NoDataFound => Ok(None),
            Err(error) => Err(oracle_error("get session name", error)),
        }
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let alias = (!agent_alias.is_empty()).then_some(agent_alias);
        self.with_transaction("set session agent alias", |conn| {
            conn.execute(
                "MERGE INTO session_metadata m \
                 USING (SELECT :1 AS session_key, \
                               CAST(:2 AS VARCHAR2(512 CHAR)) AS agent_alias FROM dual) src \
                    ON (m.session_key = src.session_key) \
                 WHEN MATCHED THEN UPDATE SET m.agent_alias = src.agent_alias \
                 WHEN NOT MATCHED THEN INSERT \
                    (session_key, created_at, last_activity, message_count, agent_alias) \
                 VALUES (src.session_key, SYSTIMESTAMP, SYSTIMESTAMP, 0, src.agent_alias)",
                &[&session_key, &alias],
            )
            .map_err(|error| oracle_error("set session agent alias", error))?;
            Ok(())
        })
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn = self.conn()?;
        match conn.query_row_as(
            "SELECT agent_alias FROM session_metadata WHERE session_key = :1",
            &[&session_key],
        ) {
            Ok(value) => Ok(value),
            Err(error) if error.kind() == oracle::ErrorKind::NoDataFound => Ok(None),
            Err(error) => Err(oracle_error("get session agent alias", error)),
        }
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let channel_id = normalize_optional(context.channel_id);
        let room_id = normalize_optional(context.room_id);
        let sender_id = normalize_optional(context.sender_id);
        self.with_transaction("set session routing context", |conn| {
            conn.execute(
                "MERGE INTO session_metadata m \
                 USING (SELECT :1 AS session_key, \
                               CAST(:2 AS VARCHAR2(512 CHAR)) AS channel_id, \
                               CAST(:3 AS VARCHAR2(512 CHAR)) AS room_id, \
                               CAST(:4 AS VARCHAR2(512 CHAR)) AS sender_id FROM dual) src \
                    ON (m.session_key = src.session_key) \
                 WHEN MATCHED THEN UPDATE SET \
                    m.channel_id = COALESCE(src.channel_id, m.channel_id), \
                    m.room_id = COALESCE(src.room_id, m.room_id), \
                    m.sender_id = COALESCE(src.sender_id, m.sender_id) \
                 WHEN NOT MATCHED THEN INSERT \
                    (session_key, created_at, last_activity, message_count, \
                     channel_id, room_id, sender_id) \
                 VALUES (src.session_key, SYSTIMESTAMP, SYSTIMESTAMP, 0, \
                         src.channel_id, src.room_id, src.sender_id)",
                &[&session_key, &channel_id, &room_id, &sender_id],
            )
            .map_err(|error| oracle_error("set session routing context", error))?;
            Ok(())
        })
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let conn = self.conn().ok()?;
        let sql = format!(
            "SELECT {} FROM session_metadata WHERE session_key = :1",
            Self::metadata_columns()
        );
        let row = conn.query_row(&sql, &[&session_key]).ok()?;
        row_to_metadata(&row).ok()
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let started: Option<DateTime<Utc>> = (state == "running").then(Utc::now);
        let turn_id = turn_id.filter(|value| !value.is_empty());
        self.with_transaction("set session state", |conn| {
            conn.execute(
                "UPDATE session_metadata SET state = :1, turn_id = :2, \
                 turn_started_at = :3 WHERE session_key = :4",
                &[&state, &turn_id, &started, &session_key],
            )
            .map_err(|error| oracle_error("set session state", error))?;
            Ok(())
        })
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let conn = self.conn()?;
        match conn.query_row(
            "SELECT state, turn_id, turn_started_at FROM session_metadata \
             WHERE session_key = :1",
            &[&session_key],
        ) {
            Ok(row) => Ok(Some(SessionState {
                state: row
                    .get(0)
                    .map_err(|error| oracle_error("decode session state", error))?,
                turn_id: row
                    .get(1)
                    .map_err(|error| oracle_error("decode session turn ID", error))?,
                turn_started_at: row
                    .get(2)
                    .map_err(|error| oracle_error("decode session turn timestamp", error))?,
            })),
            Err(error) if error.kind() == oracle::ErrorKind::NoDataFound => Ok(None),
            Err(error) => Err(oracle_error("get session state", error)),
        }
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "SELECT {} FROM session_metadata WHERE state = 'running' \
             ORDER BY turn_started_at DESC NULLS LAST",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&sql, &[]) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let seconds = i64::try_from(threshold_secs).unwrap_or(i64::MAX);
        let cutoff = Utc::now() - chrono::Duration::seconds(seconds);
        let Ok(conn) = self.conn() else {
            return Vec::new();
        };
        let sql = format!(
            "SELECT {} FROM session_metadata \
             WHERE state = 'running' AND turn_started_at < :1 \
             ORDER BY turn_started_at ASC",
            Self::metadata_columns()
        );
        let Ok(rows) = conn.query(&sql, &[&cutoff]) else {
            return Vec::new();
        };
        rows_to_metadata(rows)
    }

    fn compact(&self, _session_key: &str) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_test_oracle_url() {
        assert_eq!(
            parse_test_oracle_url("test_user/test_password@db.example:1521/ORCLPDB1").unwrap(),
            OracleCredentials {
                user: "test_user".to_string(),
                password: "test_password".to_string(),
                dsn: "db.example:1521/ORCLPDB1".to_string(),
            }
        );
    }

    #[test]
    fn rejects_incomplete_test_oracle_url() {
        assert!(parse_test_oracle_url("test_user@db.example/ORCLPDB1").is_err());
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(
            build_like_pattern(r"Rust_100%\\safe"),
            r"%rust\_100\%\\safe%"
        );
    }

    #[test]
    fn oracle_text_query_uses_escaped_safe_terms() {
        assert_eq!(
            build_oracle_text_query("Rust async (best)"),
            "{rust} AND {async} AND {best}"
        );
    }

    #[test]
    fn oracle_text_query_drops_punctuation_only_tokens() {
        assert_eq!(build_oracle_text_query(" -- !!! "), "");
    }

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_ORACLE_URL pointing at a live Oracle database"]
    fn oracle_live_round_trip_metadata_state_and_search() {
        let Ok(url) = std::env::var("ZEROCLAW_TEST_ORACLE_URL") else {
            eprintln!("ZEROCLAW_TEST_ORACLE_URL not set; skipping Oracle live test");
            return;
        };
        if url.trim().is_empty() {
            eprintln!("ZEROCLAW_TEST_ORACLE_URL is empty; skipping Oracle live test");
            return;
        }

        let workspace = tempfile::TempDir::new().expect("create temporary workspace");
        let backend = crate::make_session_backend(workspace.path(), "oracle")
            .expect("construct Oracle backend through factory");
        let key = format!(
            "oracle_live_{}_{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        let initial_agent_alias = format!("oracle-test-{key}");
        let renamed_agent_alias = format!("oracle-renamed-{key}");
        backend
            .append(
                &key,
                &ChatMessage::user("unique oracle persistence search needle"),
            )
            .expect("append user message");
        backend
            .append(&key, &ChatMessage::assistant("initial response"))
            .expect("append assistant message");
        backend
            .update_last(&key, &ChatMessage::assistant("updated response"))
            .expect("update last message");
        backend.set_session_name(&key, "Oracle live test").unwrap();
        backend
            .set_session_agent_alias(&key, &initial_agent_alias)
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
        assert_eq!(metadata.name.as_deref(), Some("Oracle live test"));
        assert_eq!(
            metadata.agent_alias.as_deref(),
            Some(initial_agent_alias.as_str())
        );
        assert_eq!(metadata.channel_id.as_deref(), Some("discord.test"));
        assert_eq!(metadata.room_id.as_deref(), Some("room-1"));
        assert_eq!(metadata.sender_id.as_deref(), Some("sender-1"));
        assert_eq!(
            backend
                .count_agent_attribution(&initial_agent_alias)
                .unwrap(),
            1
        );
        assert_eq!(
            backend
                .rename_agent_attribution(&initial_agent_alias, &renamed_agent_alias)
                .unwrap(),
            1
        );
        assert_eq!(
            backend
                .count_agent_attribution(&renamed_agent_alias)
                .unwrap(),
            1
        );
        assert_eq!(
            backend
                .clear_agent_attribution(&renamed_agent_alias)
                .unwrap(),
            1
        );
        assert_eq!(
            backend.get_session_state(&key).unwrap().unwrap().state,
            "running"
        );
        assert!(
            backend
                .list_running_sessions()
                .iter()
                .any(|metadata| metadata.key == key)
        );

        let matches = backend.search(&SessionQuery {
            keyword: Some("oracle persistence search needle".to_string()),
            limit: Some(10),
        });
        assert!(matches.iter().any(|metadata| metadata.key == key));

        assert!(backend.remove_last(&key).unwrap());
        assert_eq!(backend.get_session_metadata(&key).unwrap().message_count, 1);
        assert_eq!(backend.clear_messages(&key).unwrap(), 1);
        assert!(backend.delete_session(&key).unwrap());
    }
}
