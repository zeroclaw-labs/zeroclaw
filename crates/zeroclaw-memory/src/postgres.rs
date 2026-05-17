//! PostgreSQL-backed memory implementation.
//!
//! Compiled in only when the crate is built with `--features memory-postgres`.
//! Selected at runtime by setting `[memory].backend = "postgres"` and
//! supplying `db_url` under `[storage.model_provider.config]`. Optional pgvector
//! support is enabled via `[memory.postgres].vector_enabled`.
//!
//! Designed for multi-instance deployments where several agents need to share
//! a single durable memory store with concurrent writes — the SQLite backend
//! cannot serve that use case.

use super::traits::{Memory, MemoryCategory, MemoryEntry, normalize_recent_recall_query};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use postgres::{Client, NoTls, Row};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;
use zeroclaw_api::session_keys::sanitize_session_key;

/// Maximum allowed connect timeout (seconds) to avoid unreasonable waits.
const POSTGRES_CONNECT_TIMEOUT_CAP_SECS: u64 = 300;

/// PostgreSQL-backed persistent memory.
///
/// Reliable CRUD and keyword recall via SQL. Hybrid keyword + vector recall
/// is available when pgvector is installed and `vector_enabled = true`;
/// otherwise the backend falls back to keyword-only recall and logs a
/// warning at construction.
pub struct PostgresMemory {
    alias: String,
    client: Arc<Mutex<Client>>,
    qualified_table: String,
    qualified_agents: String,
}

impl PostgresMemory {
    pub fn new(
        alias: &str,
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
        pgvector_enabled: Option<bool>,
        pgvector_dimensions: Option<usize>,
    ) -> Result<Self> {
        validate_identifier(schema, "storage schema")?;
        validate_identifier(table, "storage table")?;

        let schema_ident = quote_identifier(schema);
        let table_ident = quote_identifier(table);
        let qualified_table = format!("{schema_ident}.{table_ident}");
        let qualified_agents = format!("{schema_ident}.agents");

        let client = Self::initialize_client(
            db_url.to_string(),
            connect_timeout_secs,
            schema_ident.clone(),
            qualified_table.clone(),
        )?;

        let pgvector_enabled = pgvector_enabled.unwrap_or(false);
        let pgvector_dimensions = pgvector_dimensions.unwrap_or(1536);

        if pgvector_enabled {
            let client_ref = Arc::new(Mutex::new(client));
            let ext_ok = {
                let mut c = client_ref.lock();
                Self::try_enable_pgvector(&mut c, &qualified_table, pgvector_dimensions).is_ok()
            };
            if !ext_ok {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "pgvector extension not available; falling back to keyword-only recall"
                );
            }
            Ok(Self {
                alias: alias.to_string(),
                client: client_ref,
                qualified_table,
                qualified_agents,
            })
        } else {
            Ok(Self {
                alias: alias.to_string(),
                client: Arc::new(Mutex::new(client)),
                qualified_table,
                qualified_agents,
            })
        }
    }

    fn initialize_client(
        db_url: String,
        connect_timeout_secs: Option<u64>,
        schema_ident: String,
        qualified_table: String,
    ) -> Result<Client> {
        let init_handle = std::thread::Builder::new()
            .name("postgres-memory-init".to_string())
            .spawn(move || -> Result<Client> {
                let mut config: postgres::Config = db_url
                    .parse()
                    .context("invalid PostgreSQL connection URL")?;

                if let Some(timeout_secs) = connect_timeout_secs {
                    let bounded = timeout_secs.min(POSTGRES_CONNECT_TIMEOUT_CAP_SECS);
                    config.connect_timeout(Duration::from_secs(bounded));
                }

                let mut client = config
                    .connect(NoTls)
                    .context("failed to connect to PostgreSQL memory backend")?;

                Self::init_schema(&mut client, &schema_ident, &qualified_table)?;
                zeroclaw_config::schema::v2::migrate_postgres_memory_to_v3(
                    &mut client,
                    &schema_ident,
                    &qualified_table,
                )?;
                Ok(client)
            })
            .context("failed to spawn PostgreSQL initializer thread")?;

        init_handle
            .join()
            .map_err(|_| anyhow::anyhow!("PostgreSQL initializer thread panicked"))?
    }

    fn init_schema(client: &mut Client, schema_ident: &str, qualified_table: &str) -> Result<()> {
        client.batch_execute(&format!(
            "
            CREATE SCHEMA IF NOT EXISTS {schema_ident};

            CREATE TABLE IF NOT EXISTS {qualified_table} (
                id TEXT PRIMARY KEY,
                key TEXT UNIQUE NOT NULL,
                content TEXT NOT NULL,
                category TEXT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL,
                session_id TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memories_category ON {qualified_table}(category);
            CREATE INDEX IF NOT EXISTS idx_memories_session_id ON {qualified_table}(session_id);
            CREATE INDEX IF NOT EXISTS idx_memories_updated_at ON {qualified_table}(updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_memories_content_fts ON {qualified_table} USING gin(to_tsvector('simple', content));
            CREATE INDEX IF NOT EXISTS idx_memories_key_fts ON {qualified_table} USING gin(to_tsvector('simple', key));
            "
        ))?;

        Self::migrate_session_ids_to_sanitized(client, qualified_table)?;

        Ok(())
    }

    /// One-shot, idempotent normalization of `memories.session_id`.
    ///
    /// Mirrors the SQLite path: the orchestrator sanitizes session keys at
    /// the source so the runtime HashMap, on-disk JSONL filename, and the
    /// `session_id` filter for recall all agree. Rows written before that
    /// fix retained the raw, un-sanitized form (e.g. `slack_C123_1.2_user one`)
    /// and would be invisible to the new sanitized recall filter. Rewrite
    /// them once at startup; later runs find nothing to update because
    /// `sanitize_session_key` is idempotent.
    fn migrate_session_ids_to_sanitized(client: &mut Client, qualified_table: &str) -> Result<()> {
        let select = format!(
            "SELECT DISTINCT session_id FROM {qualified_table} WHERE session_id IS NOT NULL"
        );
        let rows = client.query(&select, &[])?;
        let distinct: Vec<String> = rows.iter().map(|r| r.get(0)).collect();

        let rewrites = Self::compute_session_id_rewrites(&distinct);
        if rewrites.is_empty() {
            return Ok(());
        }

        let update = format!("UPDATE {qualified_table} SET session_id = $1 WHERE session_id = $2");
        let stmt = client.prepare(&update)?;
        for (old, new) in &rewrites {
            client.execute(&stmt, &[new, old])?;
        }

        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"rewritten": rewrites.len()})),
            "Normalized session_id values in memories table to sanitized form"
        );

        Ok(())
    }

    /// Pure plan of `(old, new)` `session_id` rewrites for the rows whose
    /// stored value differs from its sanitized form. Extracted from
    /// `migrate_session_ids_to_sanitized` so the rewrite logic is
    /// unit-testable without a live PostgreSQL instance.
    fn compute_session_id_rewrites(distinct: &[String]) -> Vec<(String, String)> {
        distinct
            .iter()
            .filter_map(|old| {
                let new = sanitize_session_key(old);
                if new == *old {
                    None
                } else {
                    Some((old.clone(), new))
                }
            })
            .collect()
    }

    fn category_to_str(category: &MemoryCategory) -> String {
        match category {
            MemoryCategory::Core => "core".to_string(),
            MemoryCategory::Daily => "daily".to_string(),
            MemoryCategory::Conversation => "conversation".to_string(),
            MemoryCategory::Custom(name) => name.clone(),
        }
    }

    fn parse_category(value: &str) -> MemoryCategory {
        match value {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    fn try_enable_pgvector(
        client: &mut Client,
        qualified_table: &str,
        dimensions: usize,
    ) -> Result<()> {
        client.batch_execute("CREATE EXTENSION IF NOT EXISTS vector")?;
        client.batch_execute(&format!(
            r#"
            DO $$ BEGIN
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS namespace TEXT DEFAULT 'default';
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS importance REAL;
                ALTER TABLE {qualified_table} ADD COLUMN IF NOT EXISTS embedding vector({dimensions});
            EXCEPTION WHEN OTHERS THEN
                RAISE NOTICE 'pgvector columns could not be added: %', SQLERRM;
            END $$;
            CREATE INDEX IF NOT EXISTS idx_memories_namespace ON {qualified_table}(namespace);
            "#
        ))?;
        Ok(())
    }

    fn row_to_entry(row: &Row) -> Result<MemoryEntry> {
        // Named access is used throughout so row_to_entry is immune to SELECT
        // column reordering and does not depend on matching the DDL ordering.
        let timestamp: DateTime<Utc> = row.get("created_at");

        Ok(MemoryEntry {
            id: row.get("id"),
            key: row.get("key"),
            content: row.get("content"),
            category: Self::parse_category(&row.get::<_, String>("category")),
            timestamp: timestamp.to_rfc3339(),
            session_id: row.get("session_id"),
            score: row.try_get("score").ok(),
            namespace: row
                .try_get::<_, String>("namespace")
                .unwrap_or_else(|_| "default".into()),
            importance: row.try_get("importance").ok(),
            superseded_by: None,
            agent_alias: row.try_get("agent_alias").ok(),
            agent_id: row.try_get("agent_id").ok(),
        })
    }
}

/// Run a blocking closure on a plain OS thread to avoid nested Tokio runtime
/// panics. The sync `postgres` crate internally calls `Runtime::block_on()`,
/// which conflicts with `tokio::task::spawn_blocking` threads that are still
/// associated with the Tokio runtime's blocking pool. Plain OS threads have no
/// runtime context, so the nested `block_on` succeeds.
async fn run_on_os_thread<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let (tx, rx) = oneshot::channel();

    std::thread::Builder::new()
        .name("postgres-memory-op".to_string())
        .spawn(move || {
            let result = f();
            let _ = tx.send(result);
        })
        .context("failed to spawn PostgreSQL operation thread")?;

    rx.await
        .map_err(|_| anyhow::anyhow!("PostgreSQL operation thread terminated unexpectedly"))?
}

fn validate_identifier(value: &str, field_name: &str) -> Result<()> {
    if value.is_empty() {
        anyhow::bail!("{field_name} must not be empty");
    }

    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        anyhow::bail!("{field_name} must not be empty");
    };

    if !(first.is_ascii_alphabetic() || first == '_') {
        anyhow::bail!("{field_name} must start with an ASCII letter or underscore; got '{value}'");
    }

    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        anyhow::bail!(
            "{field_name} can only contain ASCII letters, numbers, and underscores; got '{value}'"
        );
    }

    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{value}\"")
}

#[async_trait]
impl Memory for PostgresMemory {
    fn name(&self) -> &str {
        "postgres"
    }

    async fn store(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
    ) -> Result<()> {
        // Trait-level `store` has no agent context. Route through
        // `store_with_agent` so the row is attributed to the default
        // agent (the NOT NULL FK on `agent_id` rejects unattributed
        // inserts; un-attributed callers like the heartbeat memory
        // path land under the synthesized `default` agent rather than
        // surfacing a constraint violation).
        self.store_with_agent(key, content, category, session_id, None, None, None)
            .await
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let query = normalize_recent_recall_query(query).trim().to_string();
        let sid = session_id.map(str::to_string);
        let since_owned = since.map(str::to_string);
        let until_owned = until.map(str::to_string);

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let mut client = client.lock();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();

            let time_filter: String = match (since_ref, until_ref) {
                (Some(_), Some(_)) => {
                    " AND created_at >= $4::TIMESTAMPTZ AND created_at <= $5::TIMESTAMPTZ".into()
                }
                (Some(_), None) => " AND created_at >= $4::TIMESTAMPTZ".into(),
                (None, Some(_)) => " AND created_at <= $4::TIMESTAMPTZ".into(),
                (None, None) => String::new(),
            };

            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id,
                       (
                         CASE WHEN to_tsvector('simple', m.key) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.key), plainto_tsquery('simple', $1)) * 2.0
                           ELSE 0.0 END +
                         CASE WHEN to_tsvector('simple', m.content) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.content), plainto_tsquery('simple', $1))
                           ELSE 0.0 END
                       ) AS score
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE ($2::TEXT IS NULL OR m.session_id = $2)
                  AND ($1 = '' OR to_tsvector('simple', m.key || ' ' || m.content) @@ plainto_tsquery('simple', $1))
                  {time_filter}
                ORDER BY score DESC, m.updated_at DESC
                LIMIT $3
                ",
            );

            #[allow(clippy::cast_possible_wrap)]
            let limit_i64 = limit as i64;

            let rows = match (since_ref, until_ref) {
                (Some(s), Some(u)) => client.query(&stmt, &[&query, &sid, &limit_i64, &s, &u])?,
                (Some(s), None) => client.query(&stmt, &[&query, &sid, &limit_i64, &s])?,
                (None, Some(u)) => client.query(&stmt, &[&query, &sid, &limit_i64, &u])?,
                (None, None) => client.query(&stmt, &[&query, &sid, &limit_i64])?,
            };
            rows.iter()
                .map(Self::row_to_entry)
                .collect::<Result<Vec<MemoryEntry>>>()
        })
        .await
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let key = key.to_string();

        run_on_os_thread(move || -> Result<Option<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE m.key = $1
                LIMIT 1
                "
            );

            let row = client.query_opt(&stmt, &[&key])?;
            row.as_ref().map(Self::row_to_entry).transpose()
        })
        .await
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let category = category.map(Self::category_to_str);
        let sid = session_id.map(str::to_string);

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let mut client = client.lock();
            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE ($1::TEXT IS NULL OR m.category = $1)
                  AND ($2::TEXT IS NULL OR m.session_id = $2)
                ORDER BY m.updated_at DESC
                "
            );

            let category_ref = category.as_deref();
            let session_ref = sid.as_deref();
            let rows = client.query(&stmt, &[&category_ref, &session_ref])?;
            rows.iter()
                .map(Self::row_to_entry)
                .collect::<Result<Vec<MemoryEntry>>>()
        })
        .await
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let key = key.to_string();

        run_on_os_thread(move || -> Result<bool> {
            let mut client = client.lock();
            let stmt = format!("DELETE FROM {qualified_table} WHERE key = $1");
            let deleted = client.execute(&stmt, &[&key])?;
            Ok(deleted > 0)
        })
        .await
    }

    async fn count(&self) -> Result<usize> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();

        run_on_os_thread(move || -> Result<usize> {
            let mut client = client.lock();
            let stmt = format!("SELECT COUNT(*) FROM {qualified_table}");
            let count: i64 = client.query_one(&stmt, &[])?.get(0);
            let count =
                usize::try_from(count).context("PostgreSQL returned a negative memory count")?;
            Ok(count)
        })
        .await
    }

    async fn health_check(&self) -> bool {
        let client = self.client.clone();
        run_on_os_thread(move || Ok(client.lock().simple_query("SELECT 1").is_ok()))
            .await
            .unwrap_or(false)
    }

    async fn store_with_agent(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        _namespace: Option<&str>,
        _importance: Option<f64>,
        agent_id: Option<&str>,
    ) -> Result<()> {
        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let key = key.to_string();
        let content = content.to_string();
        let category = Self::category_to_str(&category);
        let sid = session_id.map(str::to_string);
        let aid = agent_id.map(str::to_string);

        run_on_os_thread(move || -> Result<()> {
            let now = Utc::now();
            let mut client = client.lock();
            // `agent_id = COALESCE($8, default-agent-uuid)` so callers
            // without an agent context still satisfy the NOT NULL FK
            // by attributing to the synthesized default agent. The
            // subquery is indexed (UNIQUE alias) so the lookup is
            // metadata-cached after the first call.
            let stmt = format!(
                "
                INSERT INTO {qualified_table}
                    (id, key, content, category, created_at, updated_at, session_id, agent_id)
                VALUES
                    ($1, $2, $3, $4, $5, $6, $7,
                     COALESCE($8, (SELECT id FROM {qualified_agents} WHERE alias = 'default' LIMIT 1)))
                ON CONFLICT (key) DO UPDATE SET
                    content = EXCLUDED.content,
                    category = EXCLUDED.category,
                    updated_at = EXCLUDED.updated_at,
                    session_id = EXCLUDED.session_id,
                    agent_id = EXCLUDED.agent_id
                "
            );

            let id = Uuid::new_v4().to_string();
            client.execute(
                &stmt,
                &[&id, &key, &content, &category, &now, &now, &sid, &aid],
            )?;
            Ok(())
        })
        .await
    }

    async fn recall_for_agents(
        &self,
        allowed_agent_ids: &[&str],
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        // Empty allowlist means "no agent filter": fall back to plain
        // recall. The wrapper always includes the bound agent's UUID,
        // so a non-empty allowlist is the live-runtime case.
        if allowed_agent_ids.is_empty() {
            return self.recall(query, limit, session_id, since, until).await;
        }

        let client = self.client.clone();
        let qualified_table = self.qualified_table.clone();
        let qualified_agents = self.qualified_agents.clone();
        let q = normalize_recent_recall_query(query).trim().to_string();
        let sid = session_id.map(str::to_string);
        let since_owned = since.map(str::to_string);
        let until_owned = until.map(str::to_string);
        let allowed: Vec<String> = allowed_agent_ids.iter().map(|s| (*s).to_string()).collect();

        run_on_os_thread(move || -> Result<Vec<MemoryEntry>> {
            let mut client = client.lock();
            let since_ref = since_owned.as_deref();
            let until_ref = until_owned.as_deref();

            // The agent_id filter lives in the WHERE clause so the
            // backend never returns a foreign-agent row to the caller;
            // post-fetch attribution lookups in earlier impls were the
            // privacy escape Audacity flagged. The NOT NULL FK on
            // `memories.agent_id` means there are no legacy
            // unattributed rows to special-case.
            let time_filter: String = match (since_ref, until_ref) {
                (Some(_), Some(_)) => {
                    " AND m.created_at >= $5::TIMESTAMPTZ AND m.created_at <= $6::TIMESTAMPTZ".into()
                }
                (Some(_), None) => " AND m.created_at >= $5::TIMESTAMPTZ".into(),
                (None, Some(_)) => " AND m.created_at <= $5::TIMESTAMPTZ".into(),
                (None, None) => String::new(),
            };

            let stmt = format!(
                "
                SELECT m.id, m.key, m.content, m.category, m.created_at, m.session_id, a.alias AS agent_alias, m.agent_id,
                       (
                         CASE WHEN to_tsvector('simple', m.key) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.key), plainto_tsquery('simple', $1)) * 2.0
                           ELSE 0.0 END +
                         CASE WHEN to_tsvector('simple', m.content) @@ plainto_tsquery('simple', $1)
                           THEN ts_rank_cd(to_tsvector('simple', m.content), plainto_tsquery('simple', $1))
                           ELSE 0.0 END
                       ) AS score
                FROM {qualified_table} m
                LEFT JOIN {qualified_agents} a ON a.id = m.agent_id
                WHERE ($2::TEXT IS NULL OR m.session_id = $2)
                  AND ($1 = '' OR to_tsvector('simple', m.key || ' ' || m.content) @@ plainto_tsquery('simple', $1))
                  AND m.agent_id = ANY($4)
                  {time_filter}
                ORDER BY score DESC, m.updated_at DESC
                LIMIT $3
                ",
            );

            #[allow(clippy::cast_possible_wrap)]
            let limit_i64 = limit as i64;

            let rows = match (since_ref, until_ref) {
                (Some(s), Some(u)) => {
                    client.query(&stmt, &[&q, &sid, &limit_i64, &allowed, &s, &u])?
                }
                (Some(s), None) => client.query(&stmt, &[&q, &sid, &limit_i64, &allowed, &s])?,
                (None, Some(u)) => client.query(&stmt, &[&q, &sid, &limit_i64, &allowed, &u])?,
                (None, None) => client.query(&stmt, &[&q, &sid, &limit_i64, &allowed])?,
            };
            rows.iter()
                .map(Self::row_to_entry)
                .collect::<Result<Vec<MemoryEntry>>>()
        })
        .await
    }

    async fn ensure_agent_uuid(&self, alias: &str) -> Result<String> {
        let client = self.client.clone();
        let qualified_agents = self.qualified_agents.clone();
        let alias = alias.to_string();
        run_on_os_thread(move || -> Result<String> {
            let mut client = client.lock();
            let candidate = Uuid::new_v4().to_string();
            client.execute(
                &format!(
                    "INSERT INTO {qualified_agents} (id, alias, created_at)
                     VALUES ($1, $2, NOW())
                     ON CONFLICT (alias) DO NOTHING"
                ),
                &[&candidate, &alias],
            )?;
            let row: String = client
                .query_one(
                    &format!("SELECT id FROM {qualified_agents} WHERE alias = $1 LIMIT 1"),
                    &[&alias],
                )?
                .get(0);
            Ok(row)
        })
        .await
    }
}

impl ::zeroclaw_api::attribution::Attributable for PostgresMemory {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Memory(::zeroclaw_api::attribution::MemoryKind::Postgres)
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identifiers_pass_validation() {
        assert!(validate_identifier("public", "schema").is_ok());
        assert!(validate_identifier("_memories_01", "table").is_ok());
    }

    #[test]
    fn invalid_identifiers_are_rejected() {
        assert!(validate_identifier("", "schema").is_err());
        assert!(validate_identifier("1bad", "schema").is_err());
        assert!(validate_identifier("bad-name", "table").is_err());
    }

    #[test]
    fn parse_category_maps_known_and_custom_values() {
        assert_eq!(PostgresMemory::parse_category("core"), MemoryCategory::Core);
        assert_eq!(
            PostgresMemory::parse_category("daily"),
            MemoryCategory::Daily
        );
        assert_eq!(
            PostgresMemory::parse_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            PostgresMemory::parse_category("custom_notes"),
            MemoryCategory::Custom("custom_notes".into())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_does_not_panic_inside_tokio_runtime() {
        let outcome = std::panic::catch_unwind(|| {
            PostgresMemory::new(
                "test",
                "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
                "public",
                "memories",
                Some(1),
                None,
                None,
            )
        });

        assert!(outcome.is_ok(), "PostgresMemory::new should not panic");
        assert!(
            outcome.unwrap().is_err(),
            "PostgresMemory::new should return a connect error for an unreachable endpoint"
        );
    }

    // ── session_id migration ──────────────────────────────────────
    //
    // End-to-end migration coverage requires a live PostgreSQL instance, and
    // the crate's existing Postgres test suite does not run one in CI. The
    // unit tests below exercise the rewrite plan against the same
    // `sanitize_session_key` helper used by the migration SQL, which is
    // sufficient to verify the contract that `migrate_session_ids_to_sanitized`
    // relies on: which values change, which stay, and idempotence on re-run.

    #[test]
    fn rewrites_only_values_that_change_under_sanitization() {
        let distinct = vec![
            "slack_C123_1.2_user one".to_string(),
            "already_sanitized".to_string(),
            "whatsapp_123@g.us_alice".to_string(),
            "abc-DEF_123".to_string(),
        ];

        let rewrites = PostgresMemory::compute_session_id_rewrites(&distinct);
        assert_eq!(rewrites.len(), 2, "only the two raw forms need rewriting");

        let by_old: std::collections::HashMap<_, _> = rewrites.into_iter().collect();
        assert_eq!(
            by_old.get("slack_C123_1.2_user one").map(String::as_str),
            Some("slack_C123_1_2_user_one")
        );
        assert_eq!(
            by_old.get("whatsapp_123@g.us_alice").map(String::as_str),
            Some("whatsapp_123_g_us_alice")
        );
    }

    #[test]
    fn no_rewrites_when_all_values_already_sanitized() {
        let distinct = vec![
            "slack_C123_1_2_user_one".to_string(),
            "abc-DEF_123".to_string(),
            "".to_string(),
        ];
        let rewrites = PostgresMemory::compute_session_id_rewrites(&distinct);
        assert!(
            rewrites.is_empty(),
            "no UPDATE should be issued when every value is already sanitized"
        );
    }

    #[test]
    fn rewrite_plan_is_idempotent_when_reapplied() {
        let raw = "slack_C123_1.2_user one";
        let sanitized = sanitize_session_key(raw);

        let first = PostgresMemory::compute_session_id_rewrites(&[raw.to_string()]);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].1, sanitized);

        let second = PostgresMemory::compute_session_id_rewrites(&[sanitized]);
        assert!(
            second.is_empty(),
            "re-running the plan over the rewritten value yields no further rewrite"
        );
    }
}
