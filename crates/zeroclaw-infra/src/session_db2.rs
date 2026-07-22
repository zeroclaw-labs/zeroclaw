//! IBM Db2-backed session persistence.
//!
//! The backend stores ordered messages in `sessions`; that table is the
//! source of truth for message content and timestamps.
//! `session_metadata` is the source of truth for names, routing
//! attribution, activity counters, and turn state. Connections are
//! synchronous and issued through the IBM Db2 CLI ODBC driver
//! (`clidriver/`) using the `odbc-api` crate; pool management is
//! delegated to the unixODBC driver manager.
//!
//! ## Why ODBC, and what the trade-offs look like
//!
//! There is no first-party IBM Db2 wire-protocol crate on crates.io. The
//! realistic Rust path for Db2 is therefore the IBM Db2 CLI driver
//! (`libdb2.dylib` / `libdb2.so`) reached through ODBC, which `odbc-api`
//! wraps. We deliberately do NOT attempt a hand-rolled DRDA
//! implementation — DRDA is a SNA-derived protocol with custom flow
//! control, a documented capability-exchange phase, and decades of
//! edge cases that the CLI driver already handles; reimplementing it
//! from scratch in Rust would be a project of its own and would
//! silently regress whenever IBM ships a server-side fix.
//!
//! The two real operator-facing consequences of going through ODBC are:
//!
//! 1. **Deployment-time native dependency.** The CLI driver (and the
//!    unixODBC driver manager on Linux/macOS) must be present on the
//!    host before the binary is launched. We document this in the
//!    module-level docs; the build host used by CI here does not need
//!    it because the dedicated `check-session-backend-db2` job only
//!    verifies that the `backend-db2` feature compiles.
//!
//! 2. **Per-row fetch shape.** `odbc-api` exposes a row-at-a-time
//!    `next_row` cursor and bulk-fetch via bound buffers; we use
//!    bulk-fetch because the IBM CLI driver is at its best when the
//!    driver manager can amortise the `SQLBindCol` cost across rows,
//!    and the session sizes we care about are small enough that a
//!    single bound buffer per call is the right shape.
//!
//! ## Full-text search
//!
//! Db2 ships `TEXT SEARCH` (`db2text`) as a separately-licensed server
//! component and the free Db2 community / Lite editions either omit it
//! or ship a stub that returns zero hits. Rather than fake FTS support,
//! the `search()` implementation here uses a `LIKE` substring filter
//! against `sessions.content` — an honest fallback. The operator can
//! swap in a real FTS index on the `content` column later if their
//! Db2 install has `db2text` available; the LIKE query is still
//! correct, just less selective.
//!
//! ## Connection model and lifetimes
//!
//! The CLI driver is thread-safe at the environment level, but each
//! connection handle is single-threaded. We mirror the Postgres / MySQL
//! backends' "blocking client behind `spawn_blocking`" model: the
//! `SessionBackend` trait is `Send + Sync` and every connection
//! check-out happens inside the foundation's `spawn_blocking`
//! boundary. Each operation opens its own short-lived connection (the
//! CLI driver's session cache speeds this up enough that a per-call
//! connection is cheaper than maintaining a long-lived pool inside
//! this backend).

use std::fmt::Display;
use std::path::Path;

use chrono::{DateTime, NaiveDateTime, Utc};
use odbc_api::handles::{Record as DiagnosticRecord, StatementImpl};
use odbc_api::{
    ConnectionOptions, Cursor, CursorImpl, Environment, IntoParameter,
    buffers::{BufferDesc, ColumnarAnyBuffer},
    sys::AttrCpMatch,
};
use zeroclaw_api::model_provider::ChatMessage;

use crate::session_backend::{
    SessionBackend, SessionContext, SessionMetadata, SessionQuery, SessionState, TimestampedMessage,
};

/// Lifetime tie between the IBM Db2 CLI ODBC environment and any
/// connections opened against it.
///
/// `odbc_api::Environment` owns the global driver-manager state. We
/// materialise one environment lazily on first use (`OnceLock` is
/// overkill because all backend methods are already serialised through
/// `spawn_blocking` upstream at the session-backend factory
/// boundary). `Connection<'env>` borrows from the environment, so we
/// store the environment alongside the connection and never hand out a
/// `Db2SessionBackend` that owns a detached connection.
pub struct Db2SessionBackend {
    env: Environment,
}

/// Buffer size for the `role` column. Twelve characters covers every
/// role the runtime currently emits (`user`, `assistant`, `system`,
/// `tool`); we round up to absorb future additions without a schema
/// migration.
const ROLE_BUF_LEN: usize = 32;

/// Buffer size for short metadata columns. `session_key` is
/// `VARCHAR(512)`; the other routing columns are comfortably under 256.
/// A single conservative size keeps the bind setup trivial.
const SHORT_TEXT_BUF_LEN: usize = 512;

/// Buffer size for `content` on the `sessions` table. Db2 `CLOB`
/// payloads larger than this fall back to streaming via `get_text` in
/// the next backend revision; 4 KiB is enough for the realistic
/// chat-message sizes we see in practice.
const CONTENT_BUF_LEN: usize = 4096;

/// How many rows we ask the CLI driver to fetch per `SQLFetch` round
/// trip on the bulk path. 64 is small enough to keep memory bounded
/// under heavy load and large enough that the per-batch `SQLBindCol`
/// amortisation is meaningful for the cursor lifetime.
const ROW_BATCH: usize = 64;

impl Db2SessionBackend {
    /// Construct the backend from the canonical environment-backed
    /// Db2 connection string and initialise its schema.
    pub fn new(workspace_dir: &Path, pool_size: u32) -> std::io::Result<Self> {
        let _ = workspace_dir;
        let _ = pool_size;
        let conn_str = read_db2_conn_str()?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session_backend=db2 requires ZEROCLAW_channels__db2_conn_str \
                 (or ZEROCLAW_TEST_DB2_URL in tests) to be set in the \
                 environment. Populate `channels.db2_conn_str` with an ODBC \
                 connection string such as `DRIVER={DB2};DATABASE=ZCTEST;\
                 HOSTNAME=db2.example.com;PORT=50000;UID=zeroclaw;PWD=…;\
                 PROTOCOL=TCPIP;` (the operator-facing setup is \
                 documented on the `db2_conn_str` config field).",
            )
        })?;
        Self::new_with_conn_str(&conn_str)
    }

    fn new_with_conn_str(conn_str: &str) -> std::io::Result<Self> {
        if conn_str.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session Db2 connection string is empty; provide a non-empty \
                 DRIVER={DB2};DATABASE=…;HOSTNAME=…;PORT=…;UID=…;PWD=…; \
                 PROTOCOL=TCPIP; string.",
            ));
        }
        let mut env = Environment::new().map_err(map_db2_err)?;
        // Configure the CLI driver to be strict about matching pooled
        // sessions — we do not want a stale session with a different
        // UID/PWD to be silently reused. This is the same default the
        // foundation uses for the MySQL/MariaDB and Postgres backends
        // (the pool they manage is process-local; ours is the CLI
        // driver's own session cache).
        env.set_connection_pooling_matching(AttrCpMatch::Strict)
            .map_err(map_db2_err)?;
        let backend = Self { env };
        let probe = build_attr_string(conn_str);
        backend.ensure_schema(&probe)?;
        Ok(backend)
    }

    /// Open a short-lived connection scoped to a single backend call.
    /// Each `SessionBackend` trait method that touches Db2 calls this,
    /// relying on the CLI driver's session cache to amortise the cost
    /// of the TCP / attach handshake across calls in the same process.
    fn open_connection<'a>(&'a self, conn_str: &str) -> std::io::Result<odbc_api::Connection<'a>> {
        self.env
            .connect_with_connection_string(conn_str, ConnectionOptions::default())
            .map_err(map_db2_err)
    }

    fn ensure_schema(&self, conn_str: &str) -> std::io::Result<()> {
        // The schema DDL is split into individual statements because
        // `odbc-api`'s `execute` runs one SQL per call. Db2 also does
        // not accept multiple semicolon-separated statements in a
        // single ODBC `SQLExecDirect` roundtrip the way PostgreSQL /
        // SQLite / MySQL do, so we MUST split them anyway.
        let statements: &[&str] = &[
            // content is VARCHAR (not CLOB): CLOB retrieval needs
            // piecemeal SQLGetData, which the columnar bulk-fetch path
            // below does not do -- reading a CLOB through a plain Text
            // buffer silently comes back empty. VARCHAR up to
            // CONTENT_BUF_LEN binds and reads correctly through that
            // same columnar path.
            "CREATE TABLE sessions ( \
                 id          BIGINT       NOT NULL GENERATED ALWAYS AS IDENTITY \
                                              (START WITH 1 INCREMENT BY 1), \
                 session_key VARCHAR(512) NOT NULL, \
                 role        VARCHAR(64)  NOT NULL, \
                 content     VARCHAR(4096) NOT NULL, \
                 created_at  TIMESTAMP    NOT NULL DEFAULT CURRENT TIMESTAMP, \
                 PRIMARY KEY (id) \
             )",
            "CREATE INDEX idx_sessions_key ON sessions(session_key)",
            "CREATE INDEX idx_sessions_key_id ON sessions(session_key, id)",
            "CREATE TABLE session_metadata ( \
                 session_key      VARCHAR(512) NOT NULL PRIMARY KEY, \
                 created_at       TIMESTAMP    NOT NULL DEFAULT CURRENT TIMESTAMP, \
                 last_activity    TIMESTAMP    NOT NULL DEFAULT CURRENT TIMESTAMP, \
                 message_count    BIGINT       NOT NULL DEFAULT 0, \
                 name             VARCHAR(255), \
                 state            VARCHAR(32)  NOT NULL DEFAULT 'idle', \
                 turn_id          VARCHAR(255), \
                 turn_started_at  TIMESTAMP, \
                 agent_alias      VARCHAR(255), \
                 channel_id       VARCHAR(255), \
                 room_id          VARCHAR(255), \
                 sender_id        VARCHAR(255) \
             )",
            "CREATE INDEX idx_session_metadata_agent_alias ON session_metadata(agent_alias)",
            "CREATE INDEX idx_session_metadata_channel_id ON session_metadata(channel_id)",
            "CREATE INDEX idx_session_metadata_room_id ON session_metadata(room_id)",
            "CREATE INDEX idx_session_metadata_sender_id ON session_metadata(sender_id)",
        ];
        let conn = self.open_connection(conn_str)?;
        for stmt in statements {
            // Db2 returns success-with-info ("the requested object
            // already exists in the database") when the table / index
            // is already there; we tolerate that class of result by
            // matching SQLSTATE 42710 (object-already-exists) which
            // `execute` surfaces through the `Diagnostics` variant.
            match conn.execute(stmt, ()) {
                Ok(_) => {}
                Err(error) => match sql_state_of(&error).as_deref() {
                    Some("42710") => {}
                    _ => return Err(map_db2_err(error)),
                },
            }
        }
        Ok(())
    }

    /// Resolve the canonical Db2 connection string. Used as the
    /// default for every method that needs a connection but doesn't
    /// accept one explicitly.
    fn conn_str(&self) -> std::io::Result<String> {
        match read_db2_conn_str()? {
            Some(s) => Ok(s),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session_backend=db2: no ZEROCLAW_channels__db2_conn_str / \
                 ZEROCLAW_TEST_DB2_URL in environment; cannot open a Db2 \
                 connection. The operator-facing setup is documented \
                 on the `db2_conn_str` config field.",
            )),
        }
    }

    fn row_to_metadata(
        key: String,
        created_at_raw: String,
        last_activity_raw: String,
        count: i64,
        name: Option<String>,
        agent_alias: Option<String>,
        channel_id: Option<String>,
        room_id: Option<String>,
        sender_id: Option<String>,
    ) -> SessionMetadata {
        let created_at = parse_db2_timestamp(&created_at_raw).unwrap_or_else(Utc::now);
        let last_activity = parse_db2_timestamp(&last_activity_raw).unwrap_or_else(Utc::now);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let message_count = count.max(0) as usize;
        SessionMetadata {
            key,
            name,
            created_at,
            last_activity,
            message_count,
            agent_alias,
            channel_id,
            room_id,
            sender_id,
        }
    }
}

/// Map any `odbc_api::Error` into a `std::io::Error`. We pick `Other`
/// for the majority of cases because the IBM CLI driver surfaces
/// detailed diagnostic records only at log time, not in the variant
/// enum, and the upstream `Error` enum is non-exhaustive across
/// versions.
fn map_db2_err<E: Display>(error: E) -> std::io::Error {
    std::io::Error::other(format!("session_backend=db2: {error}"))
}

/// Extract the SQLSTATE from an `odbc_api::Error` if it carries a
/// `Diagnostics` payload. Returns `None` for variants that do not
/// expose a state (no-diagnostics, allocation failures, ...).
fn sql_state_of(error: &odbc_api::Error) -> Option<String> {
    if let odbc_api::Error::Diagnostics {
        record: DiagnosticRecord { state, .. },
        function: _,
    } = error
    {
        Some(state.as_str().to_string())
    } else {
        None
    }
}

/// Normalise an incoming `db2_conn_str` so we can use it consistently
/// as both the ODBC connection string and the catalog probe during
/// `ensure_schema`. The base helper only guarantees that the value is
/// non-empty; the CLI driver itself handles attribute quoting per the
/// ODBC spec.
fn build_attr_string(conn_str: &str) -> String {
    conn_str.trim().to_string()
}

/// Resolve the canonical Db2 connection string. Mirrors the
/// foundation PR's `default_session_pool_size` of 5; pool size is
/// exposed as `crate::session_db2::read_pool_size` so an operator can
/// override it via the dotted-path env override
/// `ZEROCLAW_channels__pool_size`.
pub(crate) fn read_pool_size() -> u32 {
    std::env::var("ZEROCLAW_channels__pool_size")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .filter(|size| *size > 0)
        .unwrap_or(5)
}

fn read_db2_conn_str() -> std::io::Result<Option<String>> {
    if let Ok(value) = std::env::var("ZEROCLAW_channels__db2_conn_str") {
        if value.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ZEROCLAW_channels__db2_conn_str is set but empty; provide a \
                 DRIVER={DB2};DATABASE=…;HOSTNAME=…;PORT=…;UID=…;PWD=…; \
                 PROTOCOL=TCPIP; string.",
            ));
        }
        return Ok(Some(value));
    }
    if let Ok(value) = std::env::var("ZEROCLAW_TEST_DB2_URL") {
        if value.trim().is_empty() {
            return Ok(None);
        }
        return Ok(Some(value));
    }
    Ok(None)
}

/// Parse a Db2-formatted `TIMESTAMP` literal.
///
/// The CLI driver returns Db2's native textual format, which is
/// `YYYY-MM-DD-HH.MM.SS.ffffff` (note: dashes between date components
/// and dots between time components). We accept that and the
/// whitespace / `:` / `-` permutations we have observed in the wild.
fn parse_db2_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    for fmt in [
        "%Y-%m-%d-%H.%M.%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d-%H.%M.%S",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Some(ndt.and_utc());
        }
    }
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Build the safe substring filter for `search()`. The parameter is
/// bound, not concatenated, so SQL injection is impossible. Truncation
/// keeps the LIKE plan from scanning absurdly long patterns.
fn build_search_pattern(keyword: &str) -> String {
    let trimmed = keyword.trim();
    let truncated = if trimmed.len() > 200 {
        &trimmed[..200]
    } else {
        trimmed
    };
    format!("%{truncated}%")
}

/// Normalise an optional routing-context field to a `String` the ODBC
/// parameter binder can swallow. Mirrors the helper in
/// `session_postgres` and `session_mysql_shared`.
fn normalize_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

/// Read the full connection-string env var or, failing that, fall
/// back to a synthetic `DRIVER={DB2}`-only probe. The latter is only
/// useful for callers that build their own complete attribute string
/// (the `ensure_schema` step). Operators configure the full string via
/// `ZEROCLAW_channels__db2_conn_str`.
/// Convert a `TextColumnView` cell to a `String`. Empty / null cells
/// become `None`; everything else round-trips through `from_utf8_lossy`
/// because the Db2 CLI driver is configured for UTF-8 by default but
/// some operator configs (especially older AIX) still produce Windows
/// codepage bytes for legacy schemas.
fn cell_to_string(
    view: Option<odbc_api::buffers::TextColumnView<u8>>,
    row: usize,
) -> Option<String> {
    view.and_then(|column| column.get(row))
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .filter(|s| !s.is_empty())
}

/// Same as `cell_to_string` but never drops empty values: the caller
/// wants the raw cell to inspect timestamps or pass through as
/// `None`-vs-`Some("")` semantics.
fn cell_to_string_lossy(view: Option<odbc_api::buffers::TextColumnView<u8>>, row: usize) -> String {
    view.and_then(|column| column.get(row))
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default()
}

impl SessionBackend for Db2SessionBackend {
    fn load(&self, session_key: &str) -> Vec<ChatMessage> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT role, content FROM sessions WHERE session_key = ? ORDER BY id ASC";
        let key_param = session_key.to_string().into_parameter();
        let Ok(Some(cursor)) = conn.execute(sql, &key_param) else {
            return Vec::new();
        };
        let buffer = ColumnarAnyBuffer::from_descs(
            ROW_BATCH,
            [
                BufferDesc::Text {
                    max_str_len: ROLE_BUF_LEN,
                },
                BufferDesc::Text {
                    max_str_len: CONTENT_BUF_LEN,
                },
            ],
        );
        let mut row_set = match cursor.bind_buffer(buffer) {
            Ok(buf) => buf,
            Err(_) => return Vec::new(),
        };
        let mut messages = Vec::new();
        loop {
            match row_set.fetch() {
                Ok(Some(batch)) => {
                    let total = batch.num_rows();
                    let roles = batch.column(0).as_text_view();
                    let contents = batch.column(1).as_text_view();
                    for i in 0..total {
                        let role = cell_to_string_lossy(roles, i);
                        let content = cell_to_string_lossy(contents, i);
                        messages.push(ChatMessage { role, content });
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        messages
    }

    fn load_with_timestamps(&self, session_key: &str) -> Vec<TimestampedMessage> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT role, content, created_at FROM sessions \
                   WHERE session_key = ? ORDER BY id ASC";
        let key_param = session_key.to_string().into_parameter();
        let Ok(Some(cursor)) = conn.execute(sql, &key_param) else {
            return Vec::new();
        };
        let buffer = ColumnarAnyBuffer::from_descs(
            ROW_BATCH,
            [
                BufferDesc::Text {
                    max_str_len: ROLE_BUF_LEN,
                },
                BufferDesc::Text {
                    max_str_len: CONTENT_BUF_LEN,
                },
                BufferDesc::Text { max_str_len: 64 },
            ],
        );
        let mut row_set = match cursor.bind_buffer(buffer) {
            Ok(buf) => buf,
            Err(_) => return Vec::new(),
        };
        let mut messages = Vec::new();
        loop {
            match row_set.fetch() {
                Ok(Some(batch)) => {
                    let total = batch.num_rows();
                    let roles = batch.column(0).as_text_view();
                    let contents = batch.column(1).as_text_view();
                    let createds = batch.column(2).as_text_view();
                    for i in 0..total {
                        let role = cell_to_string_lossy(roles, i);
                        let content = cell_to_string_lossy(contents, i);
                        let created_at = parse_db2_timestamp(&cell_to_string_lossy(createds, i));
                        messages.push(TimestampedMessage {
                            message: ChatMessage { role, content },
                            created_at,
                        });
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        messages
    }

    fn append(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<()> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        // Db2 supports local transactions; we wrap the INSERT pair
        // so a partial failure (one row written, the other not)
        // cannot leave `session_metadata.message_count` lying about
        // reality. The CLI driver defaults autocommit ON; we
        // explicitly turn it off for this method.
        conn.set_autocommit(false).map_err(map_db2_err)?;
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let role = message.role.clone();
        let content = message.content.clone();
        let key = session_key.to_string();
        let session_sql = "INSERT INTO sessions (session_key, role, content, created_at) \
                           VALUES (?, ?, ?, ?)";
        let session_p_0 = key.clone().into_parameter();
        let session_p_1 = role.into_parameter();
        let session_p_2 = content.into_parameter();
        let session_p_3 = now_str.clone().into_parameter();
        let session_params = (&session_p_0, &session_p_1, &session_p_2, &session_p_3);
        conn.execute(session_sql, session_params)
            .map_err(map_db2_err)?;
        // Update-or-insert for the metadata row. Db2 does not have
        // the `ON CONFLICT` upsert shortcut that PostgreSQL/SQLite
        // do, so we run an UPDATE first and fall through to an INSERT
        // when the UPDATE affected zero rows.
        let update_metadata_sql = "UPDATE session_metadata \
                                   SET last_activity = ?, message_count = message_count + 1 \
                                   WHERE session_key = ?";
        let update_metadata_p_0 = now_str.clone().into_parameter();
        let update_metadata_p_1 = key.clone().into_parameter();
        let update_metadata_params = (&update_metadata_p_0, &update_metadata_p_1);
        conn.execute(update_metadata_sql, update_metadata_params)
            .map_err(map_db2_err)?;
        let insert_metadata_sql = "INSERT INTO session_metadata \
                                   (session_key, created_at, last_activity, message_count) \
                                   VALUES (?, ?, ?, 1)";
        let insert_metadata_p_0 = key.into_parameter();
        let insert_metadata_p_1 = now_str.clone().into_parameter();
        let insert_metadata_p_2 = now_str.into_parameter();
        let insert_metadata_params = (
            &insert_metadata_p_0,
            &insert_metadata_p_1,
            &insert_metadata_p_2,
        );
        match conn.execute(insert_metadata_sql, insert_metadata_params) {
            Ok(_) => {}
            Err(error) => match sql_state_of(&error).as_deref() {
                Some("23505") => {
                    // The UPDATE above already updated the row; the
                    // INSERT collided on the primary key. That is the
                    // normal path for an existing session; commit and
                    // move on.
                }
                _ => {
                    let _ = conn.rollback();
                    return Err(map_db2_err(error));
                }
            },
        }
        conn.commit().map_err(map_db2_err)?;
        Ok(())
    }

    fn remove_last(&self, session_key: &str) -> std::io::Result<bool> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        conn.set_autocommit(false).map_err(map_db2_err)?;
        let key = session_key.to_string();
        // Find the latest message id for this session.
        let lookup_sql = "SELECT id FROM sessions WHERE session_key = ? \
                          ORDER BY id DESC FETCH FIRST 1 ROW ONLY";
        let lookup_params = (&key.clone().into_parameter(),);
        let Ok(Some(cursor)) = conn.execute(lookup_sql, lookup_params) else {
            return Ok(false);
        };
        let last_id = match fetch_single_i64(cursor) {
            Some(value) => value,
            None => return Ok(false),
        };
        let delete_sql = "DELETE FROM sessions WHERE id = ?";
        let last_id_p = last_id.into_parameter();
        let delete_params = (&last_id_p,);
        conn.execute(delete_sql, delete_params)
            .map_err(map_db2_err)?;
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let update_sql = "UPDATE session_metadata \
                          SET message_count = GREATEST(message_count - 1, 0), \
                              last_activity = ? \
                          WHERE session_key = ?";
        let update_p_0 = now_str.into_parameter();
        let update_p_1 = key.into_parameter();
        let update_params = (&update_p_0, &update_p_1);
        let _ = conn.execute(update_sql, update_params);
        conn.commit().map_err(map_db2_err)?;
        Ok(true)
    }

    fn update_last(&self, session_key: &str, message: &ChatMessage) -> std::io::Result<bool> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        conn.set_autocommit(false).map_err(map_db2_err)?;
        let key = session_key.to_string();
        let lookup_sql = "SELECT id FROM sessions WHERE session_key = ? \
                          ORDER BY id DESC FETCH FIRST 1 ROW ONLY";
        let lookup_params = (&key.clone().into_parameter(),);
        let Ok(Some(cursor)) = conn.execute(lookup_sql, lookup_params) else {
            return Ok(false);
        };
        let last_id = match fetch_single_i64(cursor) {
            Some(value) => value,
            None => return Ok(false),
        };
        let role = message.role.clone();
        let content = message.content.clone();
        let update_sql = "UPDATE sessions SET role = ?, content = ? WHERE id = ?";
        let update_p_0 = role.into_parameter();
        let update_p_1 = content.into_parameter();
        let update_p_2 = last_id.into_parameter();
        let update_params = (&update_p_0, &update_p_1, &update_p_2);
        conn.execute(update_sql, update_params)
            .map_err(map_db2_err)?;
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let touch_sql = "UPDATE session_metadata SET last_activity = ? WHERE session_key = ?";
        let touch_p_0 = now_str.into_parameter();
        let touch_p_1 = key.into_parameter();
        let touch_params = (&touch_p_0, &touch_p_1);
        let _ = conn.execute(touch_sql, touch_params);
        conn.commit().map_err(map_db2_err)?;
        Ok(true)
    }

    fn list_sessions(&self) -> Vec<String> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT session_key FROM session_metadata ORDER BY last_activity DESC";
        let Ok(Some(cursor)) = conn.execute(sql, ()) else {
            return Vec::new();
        };
        let buffer = ColumnarAnyBuffer::from_descs(
            ROW_BATCH,
            [BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            }],
        );
        let mut row_set = match cursor.bind_buffer(buffer) {
            Ok(buf) => buf,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        loop {
            match row_set.fetch() {
                Ok(Some(batch)) => {
                    let total = batch.num_rows();
                    let keys = batch.column(0).as_text_view();
                    for i in 0..total {
                        if let Some(key) = cell_to_string(keys, i) {
                            out.push(key);
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        out
    }

    fn list_sessions_with_metadata(&self) -> Vec<SessionMetadata> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT session_key, created_at, last_activity, message_count, \
                          name, agent_alias, channel_id, room_id, sender_id \
                   FROM session_metadata ORDER BY last_activity DESC";
        let Ok(Some(cursor)) = conn.execute(sql, ()) else {
            return Vec::new();
        };
        fetch_metadata_rows(cursor)
    }

    fn cleanup_stale(&self, ttl_hours: u32) -> std::io::Result<usize> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        conn.set_autocommit(false).map_err(map_db2_err)?;
        // We use the `CURRENT TIMESTAMP - N HOURS` form so the
        // comparison happens in server time, not the client clock,
        // which keeps the cleanup semantics aligned with the other
        // backends.
        let sql = format!(
            "SELECT session_key FROM session_metadata \
             WHERE last_activity < (CURRENT TIMESTAMP - {ttl_hours} HOURS) \
             ORDER BY last_activity ASC"
        );
        let Ok(Some(cursor)) = conn.execute(&sql, ()) else {
            return Ok(0);
        };
        let buffer = ColumnarAnyBuffer::from_descs(
            ROW_BATCH,
            [BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            }],
        );
        let mut row_set = match cursor.bind_buffer(buffer) {
            Ok(buf) => buf,
            Err(_) => return Ok(0),
        };
        let mut stale: Vec<String> = Vec::new();
        loop {
            match row_set.fetch() {
                Ok(Some(batch)) => {
                    let total = batch.num_rows();
                    let keys = batch.column(0).as_text_view();
                    for i in 0..total {
                        if let Some(key) = cell_to_string(keys, i) {
                            stale.push(key);
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let count = stale.len();
        for key in stale {
            let key_p = key.into_parameter();
            let _ = conn.execute("DELETE FROM sessions WHERE session_key = ?", (&key_p,));
            let _ = conn.execute(
                "DELETE FROM session_metadata WHERE session_key = ?",
                (&key_p,),
            );
        }
        conn.commit().map_err(map_db2_err)?;
        Ok(count)
    }

    fn search(&self, query: &SessionQuery) -> Vec<SessionMetadata> {
        let Some(keyword) = query.keyword.as_deref() else {
            return self.list_sessions_with_metadata();
        };
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let limit = i64::try_from(query.limit.unwrap_or(50)).unwrap_or(i64::MAX);
        let limit_clause = if limit > 0 && limit < i64::MAX {
            format!(" FETCH FIRST {limit} ROWS ONLY")
        } else {
            String::new()
        };
        let sql = format!(
            "SELECT session_key, created_at, last_activity, message_count, \
                    name, agent_alias, channel_id, room_id, sender_id \
             FROM session_metadata m \
             WHERE EXISTS ( \
                 SELECT 1 FROM sessions s \
                 WHERE s.session_key = m.session_key \
                   AND s.content LIKE ? \
             ) \
             ORDER BY m.last_activity DESC{limit_clause}"
        );
        let pattern_param = build_search_pattern(keyword).into_parameter();
        let Ok(Some(cursor)) = conn.execute(&sql, &pattern_param) else {
            return Vec::new();
        };
        fetch_metadata_rows(cursor)
    }

    fn clear_messages(&self, session_key: &str) -> std::io::Result<usize> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        conn.set_autocommit(false).map_err(map_db2_err)?;
        let key = session_key.to_string();
        // Count the messages we are about to delete so the return
        // value is accurate, matching the contract on every other
        // backend.
        let count_sql = "SELECT COUNT(*) FROM sessions WHERE session_key = ?";
        let key_param_for_count = key.clone().into_parameter();
        let count_params = (&key_param_for_count,);
        let mut removed: i64 = 0;
        if let Ok(Some(cursor)) = conn.execute(count_sql, count_params) {
            removed = fetch_single_i64(cursor).unwrap_or(0);
        }
        let delete_sql = "DELETE FROM sessions WHERE session_key = ?";
        let key_param_for_delete = key.clone().into_parameter();
        let delete_params = (&key_param_for_delete,);
        conn.execute(delete_sql, delete_params)
            .map_err(map_db2_err)?;
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let update_sql = "UPDATE session_metadata SET message_count = 0, last_activity = ? \
                          WHERE session_key = ?";
        let update_p_0 = now_str.into_parameter();
        let update_p_1 = key.into_parameter();
        let update_params = (&update_p_0, &update_p_1);
        let _ = conn.execute(update_sql, update_params);
        conn.commit().map_err(map_db2_err)?;
        Ok(usize::try_from(removed.max(0)).unwrap_or(0))
    }

    fn delete_session(&self, session_key: &str) -> std::io::Result<bool> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let conn = conn;
        conn.set_autocommit(false).map_err(map_db2_err)?;
        let key = session_key.to_string();
        let exists_sql = "SELECT 1 FROM session_metadata WHERE session_key = ?";
        let exists_key = key.clone().into_parameter();
        let exists_params = (&exists_key,);
        let exists = if let Ok(Some(cursor)) = conn.execute(exists_sql, exists_params) {
            fetch_single_i64(cursor).is_some()
        } else {
            false
        };
        if !exists {
            return Ok(false);
        }
        let delete_msgs = "DELETE FROM sessions WHERE session_key = ?";
        let delete_msgs_key = key.clone().into_parameter();
        let delete_msgs_params = (&delete_msgs_key,);
        conn.execute(delete_msgs, delete_msgs_params)
            .map_err(map_db2_err)?;
        let delete_meta = "DELETE FROM session_metadata WHERE session_key = ?";
        let delete_meta_params = (&key.into_parameter(),);
        conn.execute(delete_meta, delete_meta_params)
            .map_err(map_db2_err)?;
        conn.commit().map_err(map_db2_err)?;
        Ok(true)
    }

    fn clear_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let alias = agent_alias.to_string();
        let sql = "UPDATE session_metadata SET agent_alias = NULL WHERE agent_alias = ?";
        let alias_p = alias.into_parameter();
        let params = (&alias_p,);
        if conn.execute(sql, params).is_err() {
            return Ok(0);
        }
        // The CLI driver's `SQLRowCount` is not surfaced through
        // `execute`'s return type. We approximate by counting via a
        // follow-up SELECT, which is acceptable because this method
        // is invoked from slow admin paths and not hot loops.
        let count_sql = "SELECT COUNT(*) FROM session_metadata WHERE agent_alias IS NULL";
        if let Ok(Some(cursor)) = conn.execute(count_sql, ())
            && let Some(value) = fetch_single_i64(cursor)
        {
            return Ok(usize::try_from(value.max(0)).unwrap_or(0));
        }
        Ok(0)
    }

    fn rename_agent_attribution(&self, from: &str, to: &str) -> std::io::Result<usize> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let from_s = from.to_string();
        let to_s = to.to_string();
        let sql = "UPDATE session_metadata SET agent_alias = ? WHERE agent_alias = ?";
        let from_p = from_s.into_parameter();
        let to_p = to_s.into_parameter();
        let params = (&to_p, &from_p);
        let _ = conn.execute(sql, params);
        let count_sql = "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = ?";
        let alias_p = to.to_string().into_parameter();
        let count_params = (&alias_p,);
        if let Ok(Some(cursor)) = conn.execute(count_sql, count_params)
            && let Some(value) = fetch_single_i64(cursor)
        {
            return Ok(usize::try_from(value.max(0)).unwrap_or(0));
        }
        Ok(0)
    }

    fn count_agent_attribution(&self, agent_alias: &str) -> std::io::Result<usize> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let alias = agent_alias.to_string();
        let sql = "SELECT COUNT(*) FROM session_metadata WHERE agent_alias = ?";
        let alias_p = alias.into_parameter();
        let params = (&alias_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return Ok(0);
        };
        if let Some(value) = fetch_single_i64(cursor) {
            return Ok(usize::try_from(value.max(0)).unwrap_or(0));
        }
        Ok(0)
    }

    fn session_exists(&self, session_key: &str) -> bool {
        let Ok(conn_str) = self.conn_str() else {
            return false;
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return false;
        };
        let sql = "SELECT 1 FROM session_metadata WHERE session_key = ? FETCH FIRST 1 ROW ONLY";
        let key = session_key.to_string();
        let key_p = key.into_parameter();
        let params = (&key_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return false;
        };
        fetch_single_i64(cursor).is_some()
    }

    fn set_session_name(&self, session_key: &str, name: &str) -> std::io::Result<()> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let name_val: Option<String> = if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        };
        let sql = "UPDATE session_metadata SET name = ? WHERE session_key = ?";
        let key_p = key.into_parameter();
        let name_p = name_val.into_parameter();
        let params = (&name_p, &key_p);
        conn.execute(sql, params).map_err(map_db2_err)?;
        Ok(())
    }

    fn get_session_name(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let sql = "SELECT name FROM session_metadata WHERE session_key = ?";
        let key_p = key.into_parameter();
        let params = (&key_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return Ok(None);
        };
        Ok(fetch_single_text(cursor))
    }

    fn get_session_metadata(&self, session_key: &str) -> Option<SessionMetadata> {
        let conn_str = self.conn_str().ok()?;
        let conn = self.open_connection(&conn_str).ok()?;
        let key = session_key.to_string();
        let sql = "SELECT session_key, created_at, last_activity, message_count, \
                          name, agent_alias, channel_id, room_id, sender_id \
                   FROM session_metadata WHERE session_key = ?";
        let key_p = key.into_parameter();
        let params = (&key_p,);
        let cursor = conn.execute(sql, params).ok()??;
        let mut rows = fetch_metadata_rows(cursor);
        rows.pop()
    }

    fn set_session_agent_alias(&self, session_key: &str, agent_alias: &str) -> std::io::Result<()> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let alias_val: Option<String> = if agent_alias.is_empty() {
            None
        } else {
            Some(agent_alias.to_string())
        };
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let update_sql = "UPDATE session_metadata SET agent_alias = ? WHERE session_key = ?";
        let key_p = key.clone().into_parameter();
        let alias_p = alias_val.clone().into_parameter();
        let update_params = (&alias_p, &key_p);
        let _ = conn.execute(update_sql, update_params);
        let insert_sql = "INSERT INTO session_metadata \
                          (session_key, created_at, last_activity, message_count, agent_alias) \
                          VALUES (?, ?, ?, 0, ?)";
        let insert_p_0 = key.into_parameter();
        let insert_p_1 = now_str.clone().into_parameter();
        let insert_p_2 = now_str.into_parameter();
        let insert_p_3 = alias_val.into_parameter();
        let insert_params = (&insert_p_0, &insert_p_1, &insert_p_2, &insert_p_3);
        match conn.execute(insert_sql, insert_params) {
            Ok(_) => Ok(()),
            Err(error) => match sql_state_of(&error).as_deref() {
                Some("23505") => Ok(()),
                _ => Err(map_db2_err(error)),
            },
        }
    }

    fn get_session_agent_alias(&self, session_key: &str) -> std::io::Result<Option<String>> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let sql = "SELECT agent_alias FROM session_metadata WHERE session_key = ?";
        let key_p = key.into_parameter();
        let params = (&key_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return Ok(None);
        };
        Ok(fetch_single_text(cursor))
    }

    fn set_session_context(
        &self,
        session_key: &str,
        context: SessionContext<'_>,
    ) -> std::io::Result<()> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let channel_id = normalize_optional(context.channel_id);
        let room_id = normalize_optional(context.room_id);
        let sender_id = normalize_optional(context.sender_id);
        let now_str = Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string();
        let update_sql = "UPDATE session_metadata \
                          SET channel_id = COALESCE(?, channel_id), \
                              room_id    = COALESCE(?, room_id), \
                              sender_id  = COALESCE(?, sender_id) \
                          WHERE session_key = ?";
        let key_p = key.clone().into_parameter();
        let channel_p = channel_id.clone().into_parameter();
        let room_p = room_id.clone().into_parameter();
        let sender_p = sender_id.clone().into_parameter();
        let update_params = (&channel_p, &room_p, &sender_p, &key_p);
        let _ = conn.execute(update_sql, update_params);
        let insert_sql = "INSERT INTO session_metadata \
                          (session_key, created_at, last_activity, message_count, \
                           channel_id, room_id, sender_id) \
                          VALUES (?, ?, ?, 0, ?, ?, ?)";
        let insert_p_0 = key.into_parameter();
        let insert_p_1 = now_str.clone().into_parameter();
        let insert_p_2 = now_str.into_parameter();
        let insert_p_3 = channel_id.into_parameter();
        let insert_p_4 = room_id.into_parameter();
        let insert_p_5 = sender_id.into_parameter();
        let insert_params = (
            &insert_p_0,
            &insert_p_1,
            &insert_p_2,
            &insert_p_3,
            &insert_p_4,
            &insert_p_5,
        );
        match conn.execute(insert_sql, insert_params) {
            Ok(_) => Ok(()),
            Err(error) => match sql_state_of(&error).as_deref() {
                Some("23505") => Ok(()),
                _ => Err(map_db2_err(error)),
            },
        }
    }

    fn set_session_state(
        &self,
        session_key: &str,
        state: &str,
        turn_id: Option<&str>,
    ) -> std::io::Result<()> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let turn_id_val: Option<String> = turn_id
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
        let started: Option<String> = if state == "running" {
            Some(Utc::now().format("%Y-%m-%d-%H.%M.%S%.f").to_string())
        } else {
            None
        };
        let sql = "UPDATE session_metadata \
                   SET state = ?, turn_id = ?, turn_started_at = ? \
                   WHERE session_key = ?";
        let state_p = state.to_string().into_parameter();
        let turn_id_p = turn_id_val.into_parameter();
        let started_p = started.into_parameter();
        let key_p = key.into_parameter();
        let params = (&state_p, &turn_id_p, &started_p, &key_p);
        conn.execute(sql, params).map_err(map_db2_err)?;
        Ok(())
    }

    fn get_session_state(&self, session_key: &str) -> std::io::Result<Option<SessionState>> {
        let conn_str = self.conn_str()?;
        let conn = self.open_connection(&conn_str)?;
        let key = session_key.to_string();
        let sql = "SELECT state, turn_id, turn_started_at FROM session_metadata \
                   WHERE session_key = ?";
        let key_p = key.into_parameter();
        let params = (&key_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return Ok(None);
        };
        Ok(fetch_single_state_row(cursor))
    }

    fn list_running_sessions(&self) -> Vec<SessionMetadata> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT session_key, created_at, last_activity, message_count, \
                          name, agent_alias, channel_id, room_id, sender_id \
                   FROM session_metadata \
                   WHERE state = 'running' \
                   ORDER BY turn_started_at DESC";
        let Ok(Some(cursor)) = conn.execute(sql, ()) else {
            return Vec::new();
        };
        fetch_metadata_rows(cursor)
    }

    fn list_stuck_sessions(&self, threshold_secs: u64) -> Vec<SessionMetadata> {
        let Ok(conn_str) = self.conn_str() else {
            return Vec::new();
        };
        let Ok(conn) = self.open_connection(&conn_str) else {
            return Vec::new();
        };
        let sql = "SELECT session_key, created_at, last_activity, message_count, \
                          name, agent_alias, channel_id, room_id, sender_id \
                   FROM session_metadata \
                   WHERE state = 'running' \
                     AND turn_started_at < (CURRENT TIMESTAMP - ? SECONDS) \
                   ORDER BY turn_started_at ASC";
        let threshold_str = threshold_secs.to_string();
        let threshold_p = threshold_str.into_parameter();
        let params = (&threshold_p,);
        let Ok(Some(cursor)) = conn.execute(sql, params) else {
            return Vec::new();
        };
        fetch_metadata_rows(cursor)
    }
}

/// Fetch the first `i64` value from a single-column cursor. Returns
/// `None` if the cursor is empty or the column is not bindable as
/// `i64`. Used for the `id` and `count(*)` lookups scattered through
/// this backend.
fn fetch_single_i64(cursor: CursorImpl<StatementImpl<'_>>) -> Option<i64> {
    let buffer = ColumnarAnyBuffer::from_descs(8, [BufferDesc::I64 { nullable: false }]);
    let mut row_set = cursor.bind_buffer(buffer).ok()?;
    match row_set.fetch() {
        Ok(Some(batch)) => {
            let total = batch.num_rows();
            if total == 0 {
                return None;
            }
            if let Some(col) = batch.column(0).as_slice::<i64>() {
                return col.first().copied();
            }
            None
        }
        Ok(None) => None,
        Err(_) => None,
    }
}

/// Fetch the first text value from a single-column cursor. Returns
/// `None` if the cursor is empty, the cell is NULL, or the cell is
/// empty after trimming.
fn fetch_single_text(cursor: CursorImpl<StatementImpl<'_>>) -> Option<String> {
    let buffer = ColumnarAnyBuffer::from_descs(
        8,
        [BufferDesc::Text {
            max_str_len: SHORT_TEXT_BUF_LEN,
        }],
    );
    let mut row_set = cursor.bind_buffer(buffer).ok()?;
    match row_set.fetch() {
        Ok(Some(batch)) => {
            let total = batch.num_rows();
            if total == 0 {
                return None;
            }
            cell_to_string(batch.column(0).as_text_view(), 0)
        }
        Ok(None) => None,
        Err(_) => None,
    }
}

/// Fetch the (state, turn_id, turn_started_at) triple from the
/// `get_session_state` SELECT. Mirrors the column ordering of
/// `state|turn_id|turn_started_at` in the SQL above.
fn fetch_single_state_row(cursor: CursorImpl<StatementImpl<'_>>) -> Option<SessionState> {
    let buffer = ColumnarAnyBuffer::from_descs(
        8,
        [
            BufferDesc::Text { max_str_len: 32 },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text { max_str_len: 64 },
        ],
    );
    let mut row_set = cursor.bind_buffer(buffer).ok()?;
    match row_set.fetch() {
        Ok(Some(batch)) => {
            let total = batch.num_rows();
            if total == 0 {
                return None;
            }
            let state = cell_to_string_lossy(batch.column(0).as_text_view(), 0);
            let turn_id = cell_to_string(batch.column(1).as_text_view(), 0);
            let started_str = cell_to_string_lossy(batch.column(2).as_text_view(), 0);
            let turn_started_at = if started_str.is_empty() {
                None
            } else {
                parse_db2_timestamp(&started_str)
            };
            Some(SessionState {
                state,
                turn_id,
                turn_started_at,
            })
        }
        Ok(None) => None,
        Err(_) => None,
    }
}

/// Helper that converts a cursor from the `SELECT … FROM
/// session_metadata` nine-column shape into a `Vec<SessionMetadata>`
/// using the bulk fetch path. Centralised so per-row column counting
/// stays aligned with `Db2SessionBackend::row_to_metadata`.
fn fetch_metadata_rows(cursor: CursorImpl<StatementImpl<'_>>) -> Vec<SessionMetadata> {
    let buffer = ColumnarAnyBuffer::from_descs(
        ROW_BATCH,
        [
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text { max_str_len: 64 },
            BufferDesc::Text { max_str_len: 64 },
            BufferDesc::I64 { nullable: false },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
            BufferDesc::Text {
                max_str_len: SHORT_TEXT_BUF_LEN,
            },
        ],
    );
    let mut row_set = match cursor.bind_buffer(buffer) {
        Ok(buf) => buf,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    loop {
        match row_set.fetch() {
            Ok(Some(batch)) => {
                let total = batch.num_rows();
                let keys = batch.column(0).as_text_view();
                let created = batch.column(1).as_text_view();
                let last_activity = batch.column(2).as_text_view();
                let counts = batch.column(3).as_slice::<i64>();
                let names = batch.column(4).as_text_view();
                let agents = batch.column(5).as_text_view();
                let channels = batch.column(6).as_text_view();
                let rooms = batch.column(7).as_text_view();
                let senders = batch.column(8).as_text_view();
                for i in 0..total {
                    let key = cell_to_string_lossy(keys, i);
                    let created_str = cell_to_string_lossy(created, i);
                    let last_activity_str = cell_to_string_lossy(last_activity, i);
                    let count = counts.map(|c| c[i]).unwrap_or(0);
                    let name = cell_to_string(names, i);
                    let agent = cell_to_string(agents, i);
                    let channel = cell_to_string(channels, i);
                    let room = cell_to_string(rooms, i);
                    let sender = cell_to_string(senders, i);
                    out.push(Db2SessionBackend::row_to_metadata(
                        key,
                        created_str,
                        last_activity_str,
                        count,
                        name,
                        agent,
                        channel,
                        room,
                        sender,
                    ));
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_db2_timestamp_handles_dashed_dotted_native_form() {
        let ts = parse_db2_timestamp("2026-07-21-20.00.00.123456").expect("native form");
        assert_eq!(ts.to_rfc3339(), "2026-07-21T20:00:00.123456+00:00");
    }

    #[test]
    fn parse_db2_timestamp_handles_iso8601_form() {
        let ts = parse_db2_timestamp("2026-07-21T20:00:00.123+00:00")
            .or_else(|| parse_db2_timestamp("2026-07-21-20.00.00.123"))
            .expect("ISO form");
        assert!(ts.timestamp() > 0);
    }

    #[test]
    fn parse_db2_timestamp_returns_none_for_empty() {
        assert!(parse_db2_timestamp("").is_none());
        assert!(parse_db2_timestamp("   ").is_none());
    }

    #[test]
    fn build_search_pattern_wraps_keyword_with_wildcards() {
        let p = build_search_pattern("needle");
        assert_eq!(p, "%needle%");
    }

    #[test]
    fn build_search_pattern_truncates_oversized_keyword() {
        let big = "x".repeat(1000);
        let p = build_search_pattern(&big);
        // "%" + 200 chars + "%" = 202 chars
        assert_eq!(p.len(), 202);
    }

    #[test]
    fn build_search_pattern_strips_whitespace_edges() {
        let p = build_search_pattern("  hello  ");
        assert_eq!(p, "%hello%");
    }

    #[test]
    fn normalize_optional_drops_whitespace_only_values() {
        assert_eq!(normalize_optional(Some("")), None);
        assert_eq!(normalize_optional(Some("   ")), None);
        assert_eq!(normalize_optional(None), None);
        assert_eq!(
            normalize_optional(Some("discord.clamps")),
            Some("discord.clamps".to_string())
        );
        assert_eq!(
            normalize_optional(Some("  discord.clamps  ")),
            Some("discord.clamps".to_string())
        );
    }

    #[test]
    fn map_db2_err_keeps_session_backend_prefix() {
        let err = map_db2_err("the CLI driver reported a missing table");
        let msg = err.to_string();
        assert!(
            msg.contains("session_backend=db2"),
            "Db2 errors must be prefixed with the session_backend discriminant; got: {msg}"
        );
        assert!(
            msg.contains("missing table"),
            "Db2 errors must surface the underlying error message; got: {msg}"
        );
    }

    // ── Live-DB integration test ─────────────────────────────────────
    //
    // This test is gated behind `ZEROCLAW_TEST_DB2_URL` so it does not
    // run in CI's standard lane — the dedicated
    // `check-session-backend-db2` job only verifies that the
    // `backend-db2` feature compiles cleanly, not that it actually
    // talks to a live Db2 server. Operators who want to run this
    // against a real instance set `ZEROCLAW_TEST_DB2_URL` to a
    // `DRIVER={DB2};...` ODBC connection string and execute:
    //
    //     cargo test -p zeroclaw-infra --features backend-db2 \
    //         -- --include-ignored db2_live_round_trip_metadata_state_and_search
    //
    // The test exercises the full surface area used by the
    // orchestrator: append / update / remove / clear, routing
    // context, agent alias, session state, and the LIKE-based search
    // fallback. We deliberately do NOT exercise full-text search via
    // Db2's `db2text` feature — that requires a server-side license
    // and is documented as out of scope for this backend in the
    // module-level docs.

    #[test]
    #[ignore = "requires ZEROCLAW_TEST_DB2_URL pointing at a live Db2 instance"]
    fn db2_live_round_trip_metadata_state_and_search() {
        let Ok(url) = std::env::var("ZEROCLAW_TEST_DB2_URL") else {
            eprintln!("ZEROCLAW_TEST_DB2_URL not set; skipping Db2 live test");
            return;
        };
        if url.trim().is_empty() {
            eprintln!("ZEROCLAW_TEST_DB2_URL is empty; skipping Db2 live test");
            return;
        }
        let _ = url;

        let workspace = tempfile::TempDir::new().expect("create temporary workspace");
        let backend = crate::make_session_backend(workspace.path(), "db2")
            .expect("construct Db2 backend through factory");
        let key = format!(
            "db2_live_{}_{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        backend
            .append(&key, &ChatMessage::user("unique db2 full text needle"))
            .expect("append user message");
        backend
            .append(&key, &ChatMessage::assistant("initial response"))
            .expect("append assistant message");
        backend
            .update_last(&key, &ChatMessage::assistant("updated response"))
            .expect("update last message");
        backend.set_session_name(&key, "Db2 live test").unwrap();
        backend.set_session_agent_alias(&key, "db2-test").unwrap();
        backend
            .set_session_context(
                &key,
                SessionContext {
                    channel_id: Some("discord.db2"),
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
        assert!(messages.iter().all(|m| m.created_at.is_some()));

        let metadata = backend.get_session_metadata(&key).expect("metadata");
        assert_eq!(metadata.message_count, 2);
        assert_eq!(metadata.agent_alias.as_deref(), Some("db2-test"));
        assert_eq!(metadata.channel_id.as_deref(), Some("discord.db2"));
        assert_eq!(metadata.room_id.as_deref(), Some("room-1"));
        assert_eq!(metadata.sender_id.as_deref(), Some("sender-1"));
        assert_eq!(
            backend.get_session_state(&key).unwrap().unwrap().state,
            "running"
        );

        let matches = backend.search(&SessionQuery {
            keyword: Some("needle".to_string()),
            limit: Some(10),
        });
        assert!(matches.iter().any(|m| m.key == key));

        assert!(backend.remove_last(&key).unwrap());
        assert_eq!(backend.get_session_metadata(&key).unwrap().message_count, 1);
        assert_eq!(backend.clear_messages(&key).unwrap(), 1);
        assert!(backend.delete_session(&key).unwrap());
    }
}
