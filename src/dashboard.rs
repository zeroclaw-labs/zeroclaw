use crate::aria::db::AriaDb;
use anyhow::{Context, Result};
use rusqlite::params;
use std::path::Path;

const DASHBOARD_SCHEMA_VERSION: i64 = 10;

pub fn ensure_schema(db: &AriaDb) -> Result<()> {
    db.with_conn(|conn| {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT
            );

            CREATE TABLE IF NOT EXISTS chats (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              title TEXT,
              preview TEXT,
              session_id TEXT,
              message_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chats_tenant ON chats(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_chats_updated ON chats(tenant_id, updated_at DESC);

            CREATE TABLE IF NOT EXISTS messages (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              chat_id TEXT NOT NULL,
              role TEXT NOT NULL,
              content TEXT,
              run_json TEXT,
              mentions_json TEXT,
              command_json TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id);
            CREATE INDEX IF NOT EXISTS idx_messages_tenant ON messages(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(chat_id, created_at ASC);

            CREATE TABLE IF NOT EXISTS conversations (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              title TEXT,
              session_id TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_tenant ON conversations(tenant_id);

            CREATE TABLE IF NOT EXISTS approvals (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              command TEXT NOT NULL,
              metadata_json TEXT,
              countdown INTEGER DEFAULT 30,
              status TEXT DEFAULT 'pending',
              created_at INTEGER NOT NULL,
              expires_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_approvals_tenant ON approvals(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(tenant_id, status);

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              last_activity INTEGER,
              run_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              user_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_tenant ON sessions(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(tenant_id, status);

            CREATE TABLE IF NOT EXISTS events (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              type TEXT NOT NULL,
              title TEXT NOT NULL,
              description TEXT,
              source TEXT,
              metadata_json TEXT,
              timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_events_tenant ON events(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(tenant_id, type);
            CREATE INDEX IF NOT EXISTS idx_events_time ON events(tenant_id, timestamp DESC);

            CREATE TABLE IF NOT EXISTS nodes (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              type TEXT NOT NULL,
              status TEXT DEFAULT 'online',
              load INTEGER DEFAULT 0,
              memory_usage INTEGER DEFAULT 0,
              cpu_usage INTEGER DEFAULT 0,
              last_heartbeat INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_tenant ON nodes(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_status ON nodes(tenant_id, status);

            CREATE TABLE IF NOT EXISTS channels (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              type TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              requests_per_min INTEGER DEFAULT 0,
              endpoint TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_tenant ON channels(tenant_id);

            CREATE TABLE IF NOT EXISTS cron_jobs (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              schedule TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              last_run INTEGER,
              next_run INTEGER,
              handler TEXT,
              description TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_dash_cron_tenant ON cron_jobs(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_dash_cron_status ON cron_jobs(tenant_id, status);

            CREATE TABLE IF NOT EXISTS skills (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              description TEXT,
              enabled INTEGER DEFAULT 1,
              call_count INTEGER DEFAULT 0,
              version TEXT,
              category TEXT,
              permissions_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_skills_tenant ON skills(tenant_id);

            CREATE TABLE IF NOT EXISTS system_config (
              tenant_id TEXT NOT NULL,
              key TEXT NOT NULL,
              value_json TEXT,
              updated_at INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, key)
            );

            CREATE TABLE IF NOT EXISTS logs (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              timestamp INTEGER NOT NULL,
              level TEXT NOT NULL,
              source TEXT,
              message TEXT,
              metadata_json TEXT,
              trace_id TEXT,
              span_id TEXT,
              session_id TEXT,
              agent_id TEXT,
              duration INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_logs_tenant ON logs(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(tenant_id, level);
            CREATE INDEX IF NOT EXISTS idx_logs_time ON logs(tenant_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_logs_source ON logs(tenant_id, source);
            CREATE INDEX IF NOT EXISTS idx_logs_trace ON logs(trace_id);

            CREATE TABLE IF NOT EXISTS tool_calls (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              session_id TEXT,
              run_id TEXT,
              agent_id TEXT,
              tool_name TEXT NOT NULL,
              status TEXT NOT NULL,
              args_json TEXT,
              result_json TEXT,
              error TEXT,
              duration_ms INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tool_calls_tenant ON tool_calls(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(tenant_id, session_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_run ON tool_calls(tenant_id, run_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_status ON tool_calls(tenant_id, status);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_time ON tool_calls(tenant_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS api_keys (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              key_preview TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              scopes_json TEXT,
              rate_limit INTEGER DEFAULT 1000,
              request_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              last_used_at INTEGER,
              expires_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON api_keys(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);
            CREATE INDEX IF NOT EXISTS idx_api_keys_status ON api_keys(tenant_id, status);

            CREATE TABLE IF NOT EXISTS feed_items (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              feed_id TEXT NOT NULL,
              card_type TEXT NOT NULL,
              title TEXT NOT NULL,
              body TEXT,
              source TEXT,
              url TEXT,
              metadata_json TEXT NOT NULL,
              run_id TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_feed_items_tenant ON feed_items(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_feed_items_feed ON feed_items(tenant_id, feed_id);
            CREATE INDEX IF NOT EXISTS idx_feed_items_time ON feed_items(tenant_id, feed_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS feed_files (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              extension TEXT NOT NULL,
              content_type TEXT NOT NULL,
              size INTEGER NOT NULL,
              blob_key TEXT NOT NULL,
              source_id TEXT,
              description TEXT,
              tags_json TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_feed_files_tenant ON feed_files(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_feed_files_source ON feed_files(tenant_id, source_id);
            CREATE INDEX IF NOT EXISTS idx_feed_files_time ON feed_files(tenant_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS tenant_config (
              tenant_id TEXT PRIMARY KEY,
              system_prompt_custom TEXT,
              system_prompt_version INTEGER DEFAULT 1,
              runtime_config_json TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS magic_numbers (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              email TEXT,
              key_hash TEXT NOT NULL UNIQUE,
              key_preview TEXT NOT NULL,
              name TEXT NOT NULL DEFAULT 'default',
              status TEXT NOT NULL DEFAULT 'active',
              created_at INTEGER NOT NULL,
              last_used_at INTEGER,
              revoked_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_tenant ON magic_numbers(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_hash ON magic_numbers(key_hash);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_status ON magic_numbers(tenant_id, status);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_user ON magic_numbers(tenant_id, user_id);
            "#,
        )?;

        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('dashboard_schema_version', ?1)",
            params![DASHBOARD_SCHEMA_VERSION.to_string()],
        )?;

        Ok(())
    })
}

pub fn import_from_cloud_db(db: &AriaDb, source_path: &Path) -> Result<usize> {
    if !source_path.exists() {
        return Ok(0);
    }

    let tables = [
        "chats",
        "messages",
        "conversations",
        "approvals",
        "sessions",
        "events",
        "nodes",
        "channels",
        "cron_jobs",
        "skills",
        "system_config",
        "logs",
        "tool_calls",
        "api_keys",
        "feed_items",
        "feed_files",
        "tenant_config",
        "magic_numbers",
    ];

    db.with_conn(|conn| {
        conn.execute(
            "ATTACH DATABASE ?1 AS cloud",
            params![source_path.to_string_lossy()],
        )
        .context("Failed to attach cloud dashboard DB")?;

        let mut imported = 0usize;
        for table in tables {
            let exists: Option<String> = conn
                .query_row(
                    "SELECT name FROM cloud.sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .ok();
            if exists.is_none() {
                continue;
            }
            let sql = format!("INSERT OR REPLACE INTO {table} SELECT * FROM cloud.{table}");
            conn.execute_batch(&sql)
                .with_context(|| format!("Failed importing table {table}"))?;
            imported += 1;
        }

        conn.execute_batch("DETACH DATABASE cloud")
            .context("Failed to detach cloud DB")?;
        Ok(imported)
    })
}
