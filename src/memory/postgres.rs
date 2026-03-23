use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use std::time::Duration;
use tokio::sync::OnceCell;
use tokio_postgres::Row;
use tokio_postgres_rustls::MakeRustlsConnect;
use uuid::Uuid;

/// Maximum allowed connect timeout (seconds) to avoid unreasonable waits.
const POSTGRES_CONNECT_TIMEOUT_CAP_SECS: u64 = 300;

/// Default connection pool size.
const DEFAULT_POOL_SIZE: usize = 4;

/// TLS mode for PostgreSQL connections.
#[derive(Debug, Clone, Copy)]
enum TlsMode {
    Disable,
    Prefer,
    Require,
}

/// Internal configuration stored for lazy pool initialization.
struct PostgresConfig {
    db_url: String,
    schema_ident: String,
    connect_timeout_secs: Option<u64>,
    pool_size: usize,
    tls_mode: TlsMode,
}

/// PostgreSQL-backed persistent memory using `tokio-postgres` with connection
/// pooling via `deadpool-postgres`.
///
/// All operations are natively async. The pool is initialized lazily on first
/// use, following the same pattern as the Qdrant backend.
pub struct PostgresMemory {
    pool: OnceCell<deadpool_postgres::Pool>,
    config: PostgresConfig,
    qualified_table: String,
}

impl PostgresMemory {
    /// Create a new PostgresMemory instance without performing any I/O.
    ///
    /// The connection pool is created lazily on first operation. This keeps the
    /// constructor synchronous so it can be called from the memory factory.
    pub fn new_lazy(
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
        pool_size: Option<usize>,
        tls_mode: Option<&str>,
    ) -> Result<Self> {
        validate_identifier(schema, "storage schema")?;
        validate_identifier(table, "storage table")?;

        let schema_ident = quote_identifier(schema);
        let table_ident = quote_identifier(table);
        let qualified_table = format!("{schema_ident}.{table_ident}");

        let tls = match tls_mode.map(str::trim).unwrap_or("auto") {
            "disable" => TlsMode::Disable,
            "require" => TlsMode::Require,
            "prefer" => TlsMode::Prefer,
            "auto" => {
                if db_url.contains("sslmode=require") {
                    TlsMode::Require
                } else if db_url.contains("sslmode=prefer") {
                    TlsMode::Prefer
                } else {
                    TlsMode::Disable
                }
            }
            other => {
                anyhow::bail!("invalid tls_mode '{other}'; expected disable, prefer, or require")
            }
        };

        Ok(Self {
            pool: OnceCell::new(),
            config: PostgresConfig {
                db_url: db_url.to_string(),
                schema_ident,
                connect_timeout_secs,
                pool_size: pool_size.unwrap_or(DEFAULT_POOL_SIZE),
                tls_mode: tls,
            },
            qualified_table,
        })
    }

    /// Get a reference to the initialized pool, creating it on first call.
    async fn pool(&self) -> Result<&deadpool_postgres::Pool> {
        self.pool
            .get_or_try_init(|| async {
                let pool = self.create_pool()?;

                // Verify connectivity and run schema migration
                let client = pool
                    .get()
                    .await
                    .context("failed to get connection from pool during initialization")?;
                Self::init_schema(&client, &self.config.schema_ident, &self.qualified_table)
                    .await?;

                Ok(pool)
            })
            .await
    }

    /// Build the connection pool from stored configuration.
    fn create_pool(&self) -> Result<deadpool_postgres::Pool> {
        let mut pg_config: tokio_postgres::Config = self
            .config
            .db_url
            .parse()
            .context("invalid PostgreSQL connection URL")?;

        if let Some(timeout_secs) = self.config.connect_timeout_secs {
            let bounded = timeout_secs.min(POSTGRES_CONNECT_TIMEOUT_CAP_SECS);
            pg_config.connect_timeout(Duration::from_secs(bounded));
        }

        // Set sslmode on the tokio-postgres config based on our TlsMode.
        // The MakeRustlsConnect connector is always provided but only invoked
        // when the protocol negotiation requests TLS.
        match self.config.tls_mode {
            TlsMode::Disable => {
                pg_config.ssl_mode(tokio_postgres::config::SslMode::Disable);
            }
            TlsMode::Prefer => {
                pg_config.ssl_mode(tokio_postgres::config::SslMode::Prefer);
            }
            TlsMode::Require => {
                pg_config.ssl_mode(tokio_postgres::config::SslMode::Require);
            }
        }

        let tls = Self::build_tls()?;
        let mgr_config = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let mgr = Manager::from_config(pg_config, tls, mgr_config);

        Pool::builder(mgr)
            .max_size(self.config.pool_size)
            .runtime(Runtime::Tokio1)
            .build()
            .context("failed to build PostgreSQL connection pool")
    }

    /// Build a TLS connector using the system root certificates.
    fn build_tls() -> Result<MakeRustlsConnect> {
        // Ensure the ring crypto provider is available. In production this is
        // installed by main(), but tests and CLI paths may reach here first.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let tls_config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Ok(MakeRustlsConnect::new(tls_config))
    }

    /// Create the schema, table, indexes, and run column migrations.
    ///
    /// All DDL is idempotent: safe to run on both new and existing databases.
    async fn init_schema(
        client: &deadpool_postgres::Client,
        schema_ident: &str,
        qualified_table: &str,
    ) -> Result<()> {
        client
            .batch_execute(&format!(
                "
                CREATE SCHEMA IF NOT EXISTS {schema_ident};

                CREATE TABLE IF NOT EXISTS {qualified_table} (
                    id TEXT PRIMARY KEY,
                    key TEXT UNIQUE NOT NULL,
                    content TEXT NOT NULL,
                    category TEXT NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL,
                    session_id TEXT,
                    namespace TEXT DEFAULT 'default',
                    importance DOUBLE PRECISION DEFAULT 0.5,
                    superseded_by TEXT
                );

                CREATE INDEX IF NOT EXISTS idx_memories_category
                    ON {qualified_table}(category);
                CREATE INDEX IF NOT EXISTS idx_memories_session_id
                    ON {qualified_table}(session_id);
                CREATE INDEX IF NOT EXISTS idx_memories_updated_at
                    ON {qualified_table}(updated_at DESC);
                CREATE INDEX IF NOT EXISTS idx_memories_content_fts
                    ON {qualified_table} USING gin(to_tsvector('simple', content));
                CREATE INDEX IF NOT EXISTS idx_memories_key_fts
                    ON {qualified_table} USING gin(to_tsvector('simple', key));
                CREATE INDEX IF NOT EXISTS idx_memories_namespace
                    ON {qualified_table}(namespace);

                -- Idempotent column migrations for existing databases
                ALTER TABLE {qualified_table}
                    ADD COLUMN IF NOT EXISTS namespace TEXT DEFAULT 'default';
                ALTER TABLE {qualified_table}
                    ADD COLUMN IF NOT EXISTS importance DOUBLE PRECISION DEFAULT 0.5;
                ALTER TABLE {qualified_table}
                    ADD COLUMN IF NOT EXISTS superseded_by TEXT;
                "
            ))
            .await
            .context("failed to initialize PostgreSQL schema")?;

        Ok(())
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

    /// Map a row from queries that do not include a computed score column.
    ///
    /// Expected column order: id(0), key(1), content(2), category(3),
    /// created_at(4), session_id(5), namespace(6), importance(7),
    /// superseded_by(8).
    fn row_to_entry(row: &Row) -> Result<MemoryEntry> {
        let timestamp: DateTime<Utc> = row.get(4);

        Ok(MemoryEntry {
            id: row.get(0),
            key: row.get(1),
            content: row.get(2),
            category: Self::parse_category(&row.get::<_, String>(3)),
            timestamp: timestamp.to_rfc3339(),
            session_id: row.get(5),
            score: None,
            namespace: row
                .get::<_, Option<String>>(6)
                .unwrap_or_else(|| "default".into()),
            importance: row.get(7),
            superseded_by: row.get(8),
        })
    }

    /// Map a row from recall queries that include a computed score as the last
    /// column.
    ///
    /// Expected column order: id(0), key(1), content(2), category(3),
    /// created_at(4), session_id(5), namespace(6), importance(7),
    /// superseded_by(8), score(9).
    fn scored_row_to_entry(row: &Row) -> Result<MemoryEntry> {
        let timestamp: DateTime<Utc> = row.get(4);

        Ok(MemoryEntry {
            id: row.get(0),
            key: row.get(1),
            content: row.get(2),
            category: Self::parse_category(&row.get::<_, String>(3)),
            timestamp: timestamp.to_rfc3339(),
            session_id: row.get(5),
            namespace: row
                .get::<_, Option<String>>(6)
                .unwrap_or_else(|| "default".into()),
            importance: row.get(7),
            superseded_by: row.get(8),
            score: row.try_get::<_, f64>(9).ok(),
        })
    }
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
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let cat = Self::category_to_str(&category);

        let stmt = format!(
            "
            INSERT INTO {qt}
                (id, key, content, category, created_at, updated_at, session_id,
                 namespace, importance)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, 'default', 0.5)
            ON CONFLICT (key) DO UPDATE SET
                content = EXCLUDED.content,
                category = EXCLUDED.category,
                updated_at = EXCLUDED.updated_at,
                session_id = EXCLUDED.session_id
            ",
            qt = self.qualified_table
        );

        client
            .execute(&stmt, &[&id, &key, &content, &cat, &now, &now, &session_id])
            .await
            .context("failed to store memory entry")?;

        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;
        let query = query.trim().to_string();
        let sid = session_id.map(str::to_string);

        let time_filter: &str = match (since, until) {
            (Some(_), Some(_)) => {
                " AND created_at >= $4::TIMESTAMPTZ AND created_at <= $5::TIMESTAMPTZ"
            }
            (Some(_), None) => " AND created_at >= $4::TIMESTAMPTZ",
            (None, Some(_)) => " AND created_at <= $4::TIMESTAMPTZ",
            (None, None) => "",
        };

        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id,
                   namespace, importance, superseded_by,
                   (
                     CASE WHEN to_tsvector('simple', key) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', key), plainto_tsquery('simple', $1)) * 2.0
                       ELSE 0.0 END +
                     CASE WHEN to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', content), plainto_tsquery('simple', $1))
                       ELSE 0.0 END
                   )::FLOAT8 AS score
            FROM {qt}
            WHERE superseded_by IS NULL
              AND ($2::TEXT IS NULL OR session_id = $2)
              AND ($1 = '' OR to_tsvector('simple', key || ' ' || content) @@ plainto_tsquery('simple', $1))
              {time_filter}
            ORDER BY score DESC, updated_at DESC
            LIMIT $3
            ",
            qt = self.qualified_table
        );

        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        let rows = match (since, until) {
            (Some(s), Some(u)) => {
                client
                    .query(&stmt, &[&query, &sid, &limit_i64, &s, &u])
                    .await?
            }
            (Some(s), None) => client.query(&stmt, &[&query, &sid, &limit_i64, &s]).await?,
            (None, Some(u)) => client.query(&stmt, &[&query, &sid, &limit_i64, &u]).await?,
            (None, None) => client.query(&stmt, &[&query, &sid, &limit_i64]).await?,
        };

        rows.iter()
            .map(Self::scored_row_to_entry)
            .collect::<Result<Vec<MemoryEntry>>>()
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        // No superseded_by filter: allows audit access to superseded entries
        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id,
                   namespace, importance, superseded_by
            FROM {qt}
            WHERE key = $1
            LIMIT 1
            ",
            qt = self.qualified_table
        );

        let row = client
            .query_opt(&stmt, &[&key])
            .await
            .context("failed to get memory entry")?;

        row.as_ref().map(Self::row_to_entry).transpose()
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let cat = category.map(Self::category_to_str);
        let sid = session_id.map(str::to_string);

        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id,
                   namespace, importance, superseded_by
            FROM {qt}
            WHERE superseded_by IS NULL
              AND ($1::TEXT IS NULL OR category = $1)
              AND ($2::TEXT IS NULL OR session_id = $2)
            ORDER BY updated_at DESC
            ",
            qt = self.qualified_table
        );

        let rows = client
            .query(&stmt, &[&cat, &sid])
            .await
            .context("failed to list memory entries")?;

        rows.iter()
            .map(Self::row_to_entry)
            .collect::<Result<Vec<MemoryEntry>>>()
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let stmt = format!("DELETE FROM {qt} WHERE key = $1", qt = self.qualified_table);
        let deleted = client
            .execute(&stmt, &[&key])
            .await
            .context("failed to forget memory entry")?;

        Ok(deleted > 0)
    }

    async fn count(&self) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let stmt = format!("SELECT COUNT(*) FROM {qt}", qt = self.qualified_table);
        let count: i64 = client
            .query_one(&stmt, &[])
            .await
            .context("failed to count memory entries")?
            .get(0);

        usize::try_from(count).context("PostgreSQL returned a negative memory count")
    }

    async fn health_check(&self) -> bool {
        let Ok(pool) = self.pool().await else {
            return false;
        };
        let Ok(client) = pool.get().await else {
            return false;
        };
        client.simple_query("SELECT 1").await.is_ok()
    }

    async fn purge_namespace(&self, namespace: &str) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let stmt = format!(
            "DELETE FROM {qt} WHERE namespace = $1",
            qt = self.qualified_table
        );
        let deleted = client
            .execute(&stmt, &[&namespace])
            .await
            .context("failed to purge namespace")?;

        usize::try_from(deleted).context("purge_namespace: row count exceeded usize")
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let stmt = format!(
            "DELETE FROM {qt} WHERE session_id = $1",
            qt = self.qualified_table
        );
        let deleted = client
            .execute(&stmt, &[&session_id])
            .await
            .context("failed to purge session")?;

        usize::try_from(deleted).context("purge_session: row count exceeded usize")
    }

    async fn store_with_metadata(
        &self,
        key: &str,
        content: &str,
        category: MemoryCategory,
        session_id: Option<&str>,
        namespace: Option<&str>,
        importance: Option<f64>,
    ) -> Result<()> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;

        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let cat = Self::category_to_str(&category);
        let ns = namespace.unwrap_or("default").to_string();
        let imp = importance.unwrap_or(0.5);

        let stmt = format!(
            "
            INSERT INTO {qt}
                (id, key, content, category, created_at, updated_at, session_id,
                 namespace, importance)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (key) DO UPDATE SET
                content = EXCLUDED.content,
                category = EXCLUDED.category,
                updated_at = EXCLUDED.updated_at,
                session_id = EXCLUDED.session_id,
                namespace = EXCLUDED.namespace,
                importance = EXCLUDED.importance
            ",
            qt = self.qualified_table
        );

        client
            .execute(
                &stmt,
                &[
                    &id,
                    &key,
                    &content,
                    &cat,
                    &now,
                    &now,
                    &session_id,
                    &ns,
                    &imp,
                ],
            )
            .await
            .context("failed to store memory entry with metadata")?;

        Ok(())
    }

    async fn recall_namespaced(
        &self,
        namespace: &str,
        query: &str,
        limit: usize,
        session_id: Option<&str>,
        since: Option<&str>,
        until: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("pool checkout failed")?;
        let query = query.trim().to_string();
        let sid = session_id.map(str::to_string);
        let ns = namespace.to_string();

        let time_filter: &str = match (since, until) {
            (Some(_), Some(_)) => {
                " AND created_at >= $5::TIMESTAMPTZ AND created_at <= $6::TIMESTAMPTZ"
            }
            (Some(_), None) => " AND created_at >= $5::TIMESTAMPTZ",
            (None, Some(_)) => " AND created_at <= $5::TIMESTAMPTZ",
            (None, None) => "",
        };

        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id,
                   namespace, importance, superseded_by,
                   (
                     CASE WHEN to_tsvector('simple', key) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', key), plainto_tsquery('simple', $1)) * 2.0
                       ELSE 0.0 END +
                     CASE WHEN to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', content), plainto_tsquery('simple', $1))
                       ELSE 0.0 END
                   )::FLOAT8 AS score
            FROM {qt}
            WHERE superseded_by IS NULL
              AND namespace = $2
              AND ($3::TEXT IS NULL OR session_id = $3)
              AND ($1 = '' OR to_tsvector('simple', key || ' ' || content) @@ plainto_tsquery('simple', $1))
              {time_filter}
            ORDER BY score DESC, updated_at DESC
            LIMIT $4
            ",
            qt = self.qualified_table
        );

        #[allow(clippy::cast_possible_wrap)]
        let limit_i64 = limit as i64;

        let rows = match (since, until) {
            (Some(s), Some(u)) => {
                client
                    .query(&stmt, &[&query, &ns, &sid, &limit_i64, &s, &u])
                    .await?
            }
            (Some(s), None) => {
                client
                    .query(&stmt, &[&query, &ns, &sid, &limit_i64, &s])
                    .await?
            }
            (None, Some(u)) => {
                client
                    .query(&stmt, &[&query, &ns, &sid, &limit_i64, &u])
                    .await?
            }
            (None, None) => {
                client
                    .query(&stmt, &[&query, &ns, &sid, &limit_i64])
                    .await?
            }
        };

        rows.iter()
            .map(Self::scored_row_to_entry)
            .collect::<Result<Vec<MemoryEntry>>>()
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
    async fn new_lazy_does_not_panic_inside_tokio_runtime() {
        let result = PostgresMemory::new_lazy(
            "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
            "public",
            "memories",
            Some(1),
            None,
            None,
        );

        assert!(result.is_ok(), "new_lazy should succeed without I/O");
    }

    #[tokio::test]
    async fn first_operation_on_unreachable_server_returns_false_health() {
        let mem = PostgresMemory::new_lazy(
            "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
            "public",
            "memories",
            Some(1),
            None,
            None,
        )
        .unwrap();

        let healthy = mem.health_check().await;
        assert!(
            !healthy,
            "health_check should return false for unreachable server"
        );
    }

    #[test]
    fn tls_mode_auto_detects_require_from_url() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db?sslmode=require",
            "public",
            "memories",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(mem.config.tls_mode, TlsMode::Require));
    }

    #[test]
    fn tls_mode_auto_detects_prefer_from_url() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db?sslmode=prefer",
            "public",
            "memories",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(mem.config.tls_mode, TlsMode::Prefer));
    }

    #[test]
    fn tls_mode_defaults_to_disable_without_sslmode() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db",
            "public",
            "memories",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(matches!(mem.config.tls_mode, TlsMode::Disable));
    }

    #[test]
    fn tls_mode_explicit_override() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db",
            "public",
            "memories",
            None,
            None,
            Some("require"),
        )
        .unwrap();
        assert!(matches!(mem.config.tls_mode, TlsMode::Require));
    }

    #[test]
    fn tls_mode_invalid_is_rejected() {
        let result = PostgresMemory::new_lazy(
            "postgres://u:p@host/db",
            "public",
            "memories",
            None,
            None,
            Some("invalid"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn pool_size_defaults_to_four() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db",
            "public",
            "memories",
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(mem.config.pool_size, DEFAULT_POOL_SIZE);
    }

    #[test]
    fn pool_size_respects_explicit_value() {
        let mem = PostgresMemory::new_lazy(
            "postgres://u:p@host/db",
            "public",
            "memories",
            None,
            Some(16),
            None,
        )
        .unwrap();
        assert_eq!(mem.config.pool_size, 16);
    }
}
