//! Shared `SQLite` database initialization for all Aria registries.
//!
//! All 11 registries share a single `SQLite` database with WAL mode.
//! This module provides thread-safe database access via a singleton pattern.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Thread-safe database handle wrapping a `SQLite` connection.
/// All registries share a single instance via `AriaDb::open()`.
#[derive(Clone)]
pub struct AriaDb {
    conn: Arc<Mutex<Connection>>,
}

impl AriaDb {
    /// Open or create the Aria database at the given path.
    /// Configures WAL mode and synchronous = NORMAL for performance.
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create Aria DB directory: {}", parent.display())
            })?;
        }

        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open Aria DB: {}", db_path.display()))?;

        // Performance pragmas
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )
        .context("Failed to set Aria DB pragmas")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        db.initialize_schema()?;

        Ok(db)
    }

    /// Open an in-memory database for testing.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("Failed to open in-memory Aria DB")?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )
        .context("Failed to set Aria DB pragmas")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        db.initialize_schema()?;

        Ok(db)
    }

    /// Execute a closure with the database connection.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("DB lock poisoned: {e}"))?;
        f(&conn)
    }

    /// Initialize all registry tables.
    fn initialize_schema(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch(
                "
                -- Tool registry
                CREATE TABLE IF NOT EXISTS aria_tools (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    schema          TEXT NOT NULL DEFAULT '{}',
                    handler_code    TEXT NOT NULL DEFAULT '',
                    handler_hash    TEXT NOT NULL DEFAULT '',
                    sandbox_config  TEXT,
                    status          TEXT NOT NULL DEFAULT 'active',
                    version         INTEGER NOT NULL DEFAULT 1,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_tools_tenant_name
                    ON aria_tools(tenant_id, name) WHERE status != 'deleted';

                -- Agent registry
                CREATE TABLE IF NOT EXISTS aria_agents (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    model           TEXT,
                    temperature     REAL,
                    system_prompt   TEXT,
                    tools           TEXT NOT NULL DEFAULT '[]',
                    thinking        TEXT,
                    max_retries     INTEGER,
                    timeout_seconds INTEGER,
                    handler_code    TEXT NOT NULL DEFAULT '',
                    handler_hash    TEXT NOT NULL DEFAULT '',
                    sandbox_config  TEXT,
                    status          TEXT NOT NULL DEFAULT 'active',
                    version         INTEGER NOT NULL DEFAULT 1,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_agents_tenant_name
                    ON aria_agents(tenant_id, name) WHERE status != 'deleted';

                -- Memory registry (tiered KV)
                CREATE TABLE IF NOT EXISTS aria_memory (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    key             TEXT NOT NULL,
                    value           TEXT NOT NULL,
                    tier            TEXT NOT NULL DEFAULT 'longterm',
                    namespace       TEXT,
                    session_id      TEXT,
                    ttl_seconds     INTEGER,
                    expires_at      TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_memory_tenant_key_tier
                    ON aria_memory(tenant_id, key, tier);
                CREATE INDEX IF NOT EXISTS idx_aria_memory_expires
                    ON aria_memory(expires_at) WHERE expires_at IS NOT NULL;
                CREATE INDEX IF NOT EXISTS idx_aria_memory_session
                    ON aria_memory(session_id) WHERE session_id IS NOT NULL;

                -- Task registry
                CREATE TABLE IF NOT EXISTS aria_tasks (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    handler_code    TEXT NOT NULL DEFAULT '',
                    handler_hash    TEXT NOT NULL DEFAULT '',
                    params          TEXT NOT NULL DEFAULT '{}',
                    status          TEXT NOT NULL DEFAULT 'pending',
                    result          TEXT,
                    error           TEXT,
                    agent_id        TEXT,
                    started_at      TEXT,
                    completed_at    TEXT,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_aria_tasks_tenant_status
                    ON aria_tasks(tenant_id, status);

                -- Feed registry
                CREATE TABLE IF NOT EXISTS aria_feeds (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    handler_code    TEXT NOT NULL DEFAULT '',
                    handler_hash    TEXT NOT NULL DEFAULT '',
                    schedule        TEXT NOT NULL,
                    refresh_seconds INTEGER,
                    category        TEXT,
                    retention       TEXT,
                    display         TEXT,
                    sandbox_config  TEXT,
                    status          TEXT NOT NULL DEFAULT 'active',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_feeds_tenant_name
                    ON aria_feeds(tenant_id, name) WHERE status != 'deleted';

                -- Feed items (produced by feed execution)
                CREATE TABLE IF NOT EXISTS aria_feed_items (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    feed_id         TEXT NOT NULL,
                    run_id          TEXT NOT NULL,
                    item_key        TEXT,
                    card_type       TEXT NOT NULL,
                    title           TEXT NOT NULL,
                    body            TEXT,
                    source          TEXT,
                    url             TEXT,
                    metadata        TEXT,
                    timestamp       INTEGER,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_aria_feed_items_feed
                    ON aria_feed_items(feed_id, created_at DESC);

                -- Cron function registry
                CREATE TABLE IF NOT EXISTS aria_cron_functions (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    schedule_kind   TEXT NOT NULL,
                    schedule_data   TEXT NOT NULL,
                    session_target  TEXT NOT NULL DEFAULT 'main',
                    wake_mode       TEXT NOT NULL DEFAULT 'next-heartbeat',
                    payload_kind    TEXT NOT NULL,
                    payload_data    TEXT NOT NULL,
                    isolation       TEXT,
                    enabled         INTEGER NOT NULL DEFAULT 1,
                    delete_after_run INTEGER NOT NULL DEFAULT 0,
                    cron_job_id     TEXT,
                    status          TEXT NOT NULL DEFAULT 'active',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_cron_tenant_name
                    ON aria_cron_functions(tenant_id, name);

                -- KV registry (simple persistent key-value)
                CREATE TABLE IF NOT EXISTS aria_kv (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    key             TEXT NOT NULL,
                    value           TEXT NOT NULL,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_kv_tenant_key
                    ON aria_kv(tenant_id, key);

                -- Team registry
                CREATE TABLE IF NOT EXISTS aria_teams (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    mode            TEXT NOT NULL DEFAULT 'coordinator',
                    members         TEXT NOT NULL DEFAULT '[]',
                    shared_context  TEXT,
                    timeout_seconds INTEGER,
                    max_rounds      INTEGER,
                    status          TEXT NOT NULL DEFAULT 'active',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_teams_tenant_name
                    ON aria_teams(tenant_id, name) WHERE status != 'deleted';

                -- Pipeline registry
                CREATE TABLE IF NOT EXISTS aria_pipelines (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    description     TEXT NOT NULL DEFAULT '',
                    steps           TEXT NOT NULL DEFAULT '[]',
                    variables       TEXT NOT NULL DEFAULT '{}',
                    timeout_seconds INTEGER,
                    max_parallel    INTEGER,
                    status          TEXT NOT NULL DEFAULT 'active',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_pipelines_tenant_name
                    ON aria_pipelines(tenant_id, name) WHERE status != 'deleted';

                -- Container registry
                CREATE TABLE IF NOT EXISTS aria_containers (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    image           TEXT NOT NULL,
                    config          TEXT NOT NULL DEFAULT '{}',
                    state           TEXT NOT NULL DEFAULT 'pending',
                    container_ip    TEXT,
                    container_pid   INTEGER,
                    network_id      TEXT,
                    labels          TEXT NOT NULL DEFAULT '{}',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_containers_tenant_name
                    ON aria_containers(tenant_id, name);
                CREATE INDEX IF NOT EXISTS idx_aria_containers_network
                    ON aria_containers(network_id) WHERE network_id IS NOT NULL;

                -- Network registry
                CREATE TABLE IF NOT EXISTS aria_networks (
                    id              TEXT PRIMARY KEY,
                    tenant_id       TEXT NOT NULL,
                    name            TEXT NOT NULL,
                    driver          TEXT NOT NULL DEFAULT 'bridge',
                    isolation       TEXT NOT NULL DEFAULT 'default',
                    ipv6            INTEGER NOT NULL DEFAULT 0,
                    dns_config      TEXT,
                    labels          TEXT NOT NULL DEFAULT '{}',
                    options         TEXT NOT NULL DEFAULT '{}',
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL
                );
                CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_networks_tenant_name
                    ON aria_networks(tenant_id, name);
                ",
            )
            .context("Failed to initialize Aria registry schema")?;

            let mut has_item_key = false;
            let mut has_updated_at = false;
            let mut feed_columns = conn.prepare("PRAGMA table_info(aria_feed_items)")?;
            let rows = feed_columns.query_map([], |row| row.get::<_, String>(1))?;
            for col in rows {
                match col?.as_str() {
                    "item_key" => has_item_key = true,
                    "updated_at" => has_updated_at = true,
                    _ => {}
                }
            }

            if !has_item_key {
                conn.execute("ALTER TABLE aria_feed_items ADD COLUMN item_key TEXT", [])
                    .context("Failed to add aria_feed_items.item_key column")?;
            }
            if !has_updated_at {
                conn.execute("ALTER TABLE aria_feed_items ADD COLUMN updated_at TEXT", [])
                    .context("Failed to add aria_feed_items.updated_at column")?;
            }

            conn.execute(
                "UPDATE aria_feed_items
                 SET updated_at = COALESCE(updated_at, created_at)
                 WHERE updated_at IS NULL OR updated_at = ''",
                [],
            )
            .context("Failed to backfill aria_feed_items.updated_at")?;

            conn.execute_batch(
                "UPDATE aria_feed_items
                 SET item_key = card_type || '|' || COALESCE(
                     CASE WHEN json_extract(metadata, '$.id') IS NOT NULL THEN 'id:' || lower(CAST(json_extract(metadata, '$.id') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.itemId') IS NOT NULL THEN 'itemid:' || lower(CAST(json_extract(metadata, '$.itemId') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.uid') IS NOT NULL THEN 'uid:' || lower(CAST(json_extract(metadata, '$.uid') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.uuid') IS NOT NULL THEN 'uuid:' || lower(CAST(json_extract(metadata, '$.uuid') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.key') IS NOT NULL THEN 'key:' || lower(CAST(json_extract(metadata, '$.key') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.ticker') IS NOT NULL THEN 'ticker:' || lower(CAST(json_extract(metadata, '$.ticker') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.symbol') IS NOT NULL THEN 'symbol:' || lower(CAST(json_extract(metadata, '$.symbol') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.handle') IS NOT NULL THEN 'handle:' || lower(CAST(json_extract(metadata, '$.handle') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.flightNumber') IS NOT NULL THEN 'flightnumber:' || lower(CAST(json_extract(metadata, '$.flightNumber') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.repo') IS NOT NULL THEN 'repo:' || lower(CAST(json_extract(metadata, '$.repo') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.fileId') IS NOT NULL THEN 'fileid:' || lower(CAST(json_extract(metadata, '$.fileId') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.url') IS NOT NULL THEN 'url:' || lower(CAST(json_extract(metadata, '$.url') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.slug') IS NOT NULL THEN 'slug:' || lower(CAST(json_extract(metadata, '$.slug') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.name') IS NOT NULL THEN 'name:' || lower(CAST(json_extract(metadata, '$.name') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.title') IS NOT NULL THEN 'title:' || lower(CAST(json_extract(metadata, '$.title') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.question') IS NOT NULL THEN 'question:' || lower(CAST(json_extract(metadata, '$.question') AS TEXT)) END,
                     CASE WHEN json_extract(metadata, '$.location') IS NOT NULL THEN 'location:' || lower(CAST(json_extract(metadata, '$.location') AS TEXT)) END,
                     CASE WHEN url IS NOT NULL AND trim(url) != '' THEN 'url:' || lower(trim(url)) END,
                     CASE WHEN (source IS NOT NULL AND trim(source) != '') OR (title IS NOT NULL AND trim(title) != '')
                        THEN 'source-title:' || lower(trim(COALESCE(source, ''))) || '|' || lower(trim(COALESCE(title, '')))
                     END,
                     'fallback:unknown'
                 )
                 WHERE item_key IS NULL OR item_key = '';

                 DELETE FROM aria_feed_items
                 WHERE aria_feed_items.item_key IS NOT NULL
                   AND aria_feed_items.item_key != ''
                   AND EXISTS (
                        SELECT 1
                        FROM aria_feed_items AS newer
                        WHERE newer.tenant_id = aria_feed_items.tenant_id
                          AND newer.feed_id = aria_feed_items.feed_id
                          AND newer.item_key = aria_feed_items.item_key
                          AND (
                                newer.updated_at > aria_feed_items.updated_at
                                OR (newer.updated_at = aria_feed_items.updated_at AND newer.id > aria_feed_items.id)
                              )
                   );",
            )
            .context("Failed to backfill/dedupe aria_feed_items.item_key")?;

            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_aria_feed_items_feed;
                 CREATE INDEX IF NOT EXISTS idx_aria_feed_items_feed
                     ON aria_feed_items(feed_id, updated_at DESC);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_aria_feed_items_identity
                     ON aria_feed_items(tenant_id, feed_id, item_key);",
            )
            .context("Failed to ensure aria_feed_items indexes")?;

            Ok(())
        })
    }
}

impl std::fmt::Debug for AriaDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AriaDb").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_succeeds() {
        let db = AriaDb::open_in_memory().unwrap();
        db.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'aria_%'",
                [],
                |row| row.get(0),
            )?;
            // 11 registry tables + 1 feed_items table = 12
            assert_eq!(count, 12);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn wal_mode_is_set() {
        let db = AriaDb::open_in_memory().unwrap();
        db.with_conn(|conn| {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            // In-memory databases use "memory" journal mode, but WAL is set for file-based
            assert!(mode == "wal" || mode == "memory");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn schema_is_idempotent() {
        let db = AriaDb::open_in_memory().unwrap();
        // Re-initialize should not fail
        db.initialize_schema().unwrap();
        db.initialize_schema().unwrap();
    }

    #[test]
    fn open_file_based_db() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("aria.db");
        let db = AriaDb::open(&db_path).unwrap();
        assert!(db_path.exists());

        db.with_conn(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name LIKE 'aria_%'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 12);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn unique_indexes_enforce_tenant_name_constraint() {
        let db = AriaDb::open_in_memory().unwrap();
        db.with_conn(|conn| {
            let now = chrono::Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO aria_tools (id, tenant_id, name, created_at, updated_at)
                 VALUES ('t1', 'tenant1', 'my-tool', ?1, ?1)",
                rusqlite::params![now],
            )?;

            // Same tenant+name should conflict
            let result = conn.execute(
                "INSERT INTO aria_tools (id, tenant_id, name, created_at, updated_at)
                 VALUES ('t2', 'tenant1', 'my-tool', ?1, ?1)",
                rusqlite::params![now],
            );
            assert!(result.is_err());

            // Different tenant, same name should succeed
            conn.execute(
                "INSERT INTO aria_tools (id, tenant_id, name, created_at, updated_at)
                 VALUES ('t3', 'tenant2', 'my-tool', ?1, ?1)",
                rusqlite::params![now],
            )?;

            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn migrates_legacy_feed_items_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE aria_feed_items (
                id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                feed_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                card_type TEXT NOT NULL,
                title TEXT NOT NULL,
                body TEXT,
                source TEXT,
                url TEXT,
                metadata TEXT,
                timestamp INTEGER,
                created_at TEXT NOT NULL
            );
            INSERT INTO aria_feed_items (id, tenant_id, feed_id, run_id, card_type, title, metadata, created_at)
            VALUES
              ('a', 't1', 'f1', 'r1', 'stock', 'AAPL $100', '{\"ticker\":\"AAPL\"}', '2026-02-16T00:00:00Z'),
              ('b', 't1', 'f1', 'r2', 'stock', 'AAPL $101', '{\"ticker\":\"AAPL\"}', '2026-02-16T01:00:00Z');
            ",
        )
        .unwrap();

        let db = AriaDb {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize_schema().unwrap();

        db.with_conn(|conn| {
            let mut stmt = conn.prepare("PRAGMA table_info(aria_feed_items)")?;
            let cols: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))?
                .filter_map(|r| r.ok())
                .collect();
            assert!(cols.contains(&"item_key".to_string()));
            assert!(cols.contains(&"updated_at".to_string()));

            let count: i64 = conn.query_row("SELECT COUNT(*) FROM aria_feed_items", [], |r| r.get(0))?;
            assert_eq!(count, 1, "legacy duplicate rows should be deduped");

            let idx_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_index_list('aria_feed_items') WHERE name='idx_aria_feed_items_identity'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(idx_count, 1);
            Ok(())
        })
        .unwrap();
    }
}
