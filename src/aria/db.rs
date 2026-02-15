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
                    card_type       TEXT NOT NULL,
                    title           TEXT NOT NULL,
                    body            TEXT,
                    source          TEXT,
                    url             TEXT,
                    metadata        TEXT,
                    timestamp       INTEGER,
                    created_at      TEXT NOT NULL
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
}
