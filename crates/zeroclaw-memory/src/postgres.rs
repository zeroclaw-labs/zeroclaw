//! PostgreSQL memory backend.
//!
//! Uses `tokio-postgres` with `deadpool-postgres` for async connection pooling.
//! Gated behind the `backend-postgres` Cargo feature to avoid adding
//! dependencies to the default binary.

use super::traits::{ExportFilter, Memory, MemoryCategory, MemoryEntry, ProceduralMessage};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use deadpool_postgres::{Config as PoolConfig, Pool, Runtime};
use tokio::sync::OnceCell;
use tokio_postgres::NoTls;
use uuid::Uuid;
use zeroclaw_config::schema::PostgresConfig;

pub struct PostgresMemory {
    pool: OnceCell<Pool>,
    url: String,
    schema: String,
    table: String,
    max_connections: u32,
}

impl PostgresMemory {
    pub fn new_lazy(config: &PostgresConfig) -> Result<Self> {
        let url = config
            .url
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("POSTGRES_URL").ok())
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .filter(|s| !s.trim().is_empty())
            .context(
                "Postgres memory backend requires url in [memory.postgres] or POSTGRES_URL / DATABASE_URL env var",
            )?;

        Ok(Self {
            pool: OnceCell::new(),
            url,
            schema: config.schema.clone(),
            table: config.table.clone(),
            max_connections: config.max_connections,
        })
    }

    async fn pool(&self) -> Result<&Pool> {
        self.pool
            .get_or_try_init(|| async {
                let mut cfg = PoolConfig::new();
                cfg.url = Some(self.url.clone());
                let pool = cfg
                    .create_pool(Some(Runtime::Tokio1), NoTls)
                    .context("failed to create Postgres connection pool")?;

                // Limit pool size after creation
                pool.resize(self.max_connections as usize);

                self.run_migrations(&pool).await?;
                Ok(pool)
            })
            .await
    }

    async fn run_migrations(&self, pool: &Pool) -> Result<()> {
        let client = pool.get().await.context("failed to get connection for migrations")?;

        let query = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {schema}.{table} (
                id          TEXT PRIMARY KEY,
                key         TEXT NOT NULL,
                content     TEXT NOT NULL,
                category    TEXT NOT NULL DEFAULT 'core',
                timestamp   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                session_id  TEXT,
                namespace   TEXT NOT NULL DEFAULT 'default',
                importance  DOUBLE PRECISION,
                superseded_by TEXT,
                UNIQUE (key, namespace)
            );
            CREATE INDEX IF NOT EXISTS idx_{table}_category ON {schema}.{table} (category);
            CREATE INDEX IF NOT EXISTS idx_{table}_namespace ON {schema}.{table} (namespace);
            CREATE INDEX IF NOT EXISTS idx_{table}_session ON {schema}.{table} (session_id);
            CREATE INDEX IF NOT EXISTS idx_{table}_timestamp ON {schema}.{table} (timestamp);
            "#,
            schema = self.schema,
            table = self.table,
        );
        client
            .batch_execute(&query)
            .await
            .context("failed to run Postgres memory migrations")?;
        Ok(())
    }

    fn qualified_table(&self) -> String {
        format!("{}.{}", self.schema, self.table)
    }

    fn category_to_str(cat: &MemoryCategory) -> String {
        cat.to_string()
    }

    fn str_to_category(s: &str) -> MemoryCategory {
        match s {
            "core" => MemoryCategory::Core,
            "daily" => MemoryCategory::Daily,
            "conversation" => MemoryCategory::Conversation,
            other => MemoryCategory::Custom(other.to_string()),
        }
    }

    fn row_to_entry(row: &tokio_postgres::Row) -> MemoryEntry {
        let ts: chrono::DateTime<Utc> = row.get("timestamp");
        MemoryEntry {
            id: row.get("id"),
            key: row.get("key"),
            content: row.get("content"),
            category: Self::str_to_category(row.get("category")),
            timestamp: ts.to_rfc3339(),
            session_id: row.get("session_id"),
            score: None,
            namespace: row.get("namespace"),
            importance: row.get("importance"),
            superseded_by: row.get("superseded_by"),
        }
    }
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
        self.store_with_metadata(key, content, category, session_id, None, None)
            .await
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
        let client = pool.get().await.context("postgres: connection failed")?;
        let id = Uuid::new_v4().to_string();
        let cat = Self::category_to_str(&category);
        let ns = namespace.unwrap_or("default");
        let now = Utc::now();

        let query = format!(
            r#"
            INSERT INTO {qt} (id, key, content, category, timestamp, session_id, namespace, importance)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (key, namespace) DO UPDATE SET
                content = EXCLUDED.content,
                category = EXCLUDED.category,
                timestamp = EXCLUDED.timestamp,
                session_id = EXCLUDED.session_id,
                importance = EXCLUDED.importance
            "#,
            qt = self.qualified_table(),
        );

        client
            .execute(
                &client.prepare(&query).await.context("postgres: prepare failed")?,
                &[&id, &key, &content, &cat, &now, &session_id, &ns, &importance],
            )
            .await
            .context("postgres: failed to store memory")?;

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
        let client = pool.get().await.context("postgres: connection failed")?;
        let pattern = format!("%{query}%");

        let mut sql = format!(
            "SELECT id, key, content, category, timestamp, session_id, namespace, importance, superseded_by \
             FROM {} WHERE content ILIKE $1",
            self.qualified_table(),
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = vec![Box::new(pattern)];

        if let Some(sid) = session_id {
            sql.push_str(&format!(" AND session_id = ${}", params.len() + 1));
            params.push(Box::new(sid.to_string()));
        }
        if let Some(s) = since {
            let ts: chrono::DateTime<Utc> = s.parse().context("invalid since timestamp")?;
            sql.push_str(&format!(" AND timestamp >= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }
        if let Some(u) = until {
            let ts: chrono::DateTime<Utc> = u.parse().context("invalid until timestamp")?;
            sql.push_str(&format!(" AND timestamp <= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }

        let lim = limit as i64;
        sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ${}", params.len() + 1));
        params.push(Box::new(lim));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let rows = client
            .query(&stmt, &param_refs)
            .await
            .context("postgres: recall failed")?;

        Ok(rows.iter().map(Self::row_to_entry).collect())
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;
        let sql = format!(
            "SELECT id, key, content, category, timestamp, session_id, namespace, importance, superseded_by \
             FROM {} WHERE key = $1 LIMIT 1",
            self.qualified_table(),
        );
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let row = client
            .query_opt(&stmt, &[&key])
            .await
            .context("postgres: get failed")?;
        Ok(row.as_ref().map(Self::row_to_entry))
    }

    async fn list(
        &self,
        category: Option<&MemoryCategory>,
        session_id: Option<&str>,
    ) -> Result<Vec<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;

        let mut sql = format!(
            "SELECT id, key, content, category, timestamp, session_id, namespace, importance, superseded_by \
             FROM {} WHERE 1=1",
            self.qualified_table(),
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(cat) = category {
            sql.push_str(&format!(" AND category = ${}", params.len() + 1));
            params.push(Box::new(Self::category_to_str(cat)));
        }
        if let Some(sid) = session_id {
            sql.push_str(&format!(" AND session_id = ${}", params.len() + 1));
            params.push(Box::new(sid.to_string()));
        }
        sql.push_str(" ORDER BY timestamp DESC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let rows = client
            .query(&stmt, &param_refs)
            .await
            .context("postgres: list failed")?;

        Ok(rows.iter().map(Self::row_to_entry).collect())
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;
        let sql = format!("DELETE FROM {} WHERE key = $1", self.qualified_table());
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let count = client
            .execute(&stmt, &[&key])
            .await
            .context("postgres: forget failed")?;
        Ok(count > 0)
    }

    async fn purge_namespace(&self, namespace: &str) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;
        let sql = format!(
            "DELETE FROM {} WHERE namespace = $1",
            self.qualified_table()
        );
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let count = client
            .execute(&stmt, &[&namespace])
            .await
            .context("postgres: purge_namespace failed")?;
        Ok(count as usize)
    }

    async fn purge_session(&self, session_id: &str) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;
        let sql = format!(
            "DELETE FROM {} WHERE session_id = $1",
            self.qualified_table()
        );
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let count = client
            .execute(&stmt, &[&session_id])
            .await
            .context("postgres: purge_session failed")?;
        Ok(count as usize)
    }

    async fn count(&self) -> Result<usize> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;
        let sql = format!("SELECT COUNT(*) as cnt FROM {}", self.qualified_table());
        let row = client
            .query_one(&client.prepare(&sql).await?, &[])
            .await
            .context("postgres: count failed")?;
        let count: i64 = row.get("cnt");
        Ok(count as usize)
    }

    async fn health_check(&self) -> bool {
        let Ok(pool) = self.pool().await else {
            return false;
        };
        let Ok(client) = pool.get().await else {
            return false;
        };
        client.query_one("SELECT 1 as ok", &[]).await.is_ok()
    }

    async fn store_procedural(
        &self,
        _messages: &[ProceduralMessage],
        _session_id: Option<&str>,
    ) -> Result<()> {
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
        let client = pool.get().await.context("postgres: connection failed")?;
        let pattern = format!("%{query}%");

        let mut sql = format!(
            "SELECT id, key, content, category, timestamp, session_id, namespace, importance, superseded_by \
             FROM {} WHERE namespace = $1 AND content ILIKE $2",
            self.qualified_table(),
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> =
            vec![Box::new(namespace.to_string()), Box::new(pattern)];

        if let Some(sid) = session_id {
            sql.push_str(&format!(" AND session_id = ${}", params.len() + 1));
            params.push(Box::new(sid.to_string()));
        }
        if let Some(s) = since {
            let ts: chrono::DateTime<Utc> = s.parse().context("invalid since timestamp")?;
            sql.push_str(&format!(" AND timestamp >= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }
        if let Some(u) = until {
            let ts: chrono::DateTime<Utc> = u.parse().context("invalid until timestamp")?;
            sql.push_str(&format!(" AND timestamp <= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }

        let lim = limit as i64;
        sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ${}", params.len() + 1));
        params.push(Box::new(lim));

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let rows = client
            .query(&stmt, &param_refs)
            .await
            .context("postgres: recall_namespaced failed")?;

        Ok(rows.iter().map(Self::row_to_entry).collect())
    }

    async fn export(&self, filter: &ExportFilter) -> Result<Vec<MemoryEntry>> {
        let pool = self.pool().await?;
        let client = pool.get().await.context("postgres: connection failed")?;

        let mut sql = format!(
            "SELECT id, key, content, category, timestamp, session_id, namespace, importance, superseded_by \
             FROM {} WHERE 1=1",
            self.qualified_table(),
        );
        let mut params: Vec<Box<dyn tokio_postgres::types::ToSql + Sync + Send>> = Vec::new();

        if let Some(ref ns) = filter.namespace {
            sql.push_str(&format!(" AND namespace = ${}", params.len() + 1));
            params.push(Box::new(ns.clone()));
        }
        if let Some(ref sid) = filter.session_id {
            sql.push_str(&format!(" AND session_id = ${}", params.len() + 1));
            params.push(Box::new(sid.clone()));
        }
        if let Some(ref cat) = filter.category {
            sql.push_str(&format!(" AND category = ${}", params.len() + 1));
            params.push(Box::new(Self::category_to_str(cat)));
        }
        if let Some(ref since) = filter.since {
            let ts: chrono::DateTime<Utc> = since.parse().context("invalid since timestamp")?;
            sql.push_str(&format!(" AND timestamp >= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }
        if let Some(ref until) = filter.until {
            let ts: chrono::DateTime<Utc> = until.parse().context("invalid until timestamp")?;
            sql.push_str(&format!(" AND timestamp <= ${}", params.len() + 1));
            params.push(Box::new(ts));
        }
        sql.push_str(" ORDER BY timestamp ASC");

        let param_refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params.iter().map(|p| p.as_ref() as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
        let stmt = client.prepare(&sql).await.context("postgres: prepare failed")?;
        let rows = client
            .query(&stmt, &param_refs)
            .await
            .context("postgres: export failed")?;

        Ok(rows.iter().map(Self::row_to_entry).collect())
    }
}

impl std::fmt::Debug for PostgresMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresMemory")
            .field("schema", &self.schema)
            .field("table", &self.table)
            .field("connected", &self.pool.initialized())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_roundtrip() {
        assert_eq!(
            PostgresMemory::str_to_category("core"),
            MemoryCategory::Core
        );
        assert_eq!(
            PostgresMemory::str_to_category("daily"),
            MemoryCategory::Daily
        );
        assert_eq!(
            PostgresMemory::str_to_category("conversation"),
            MemoryCategory::Conversation
        );
        assert_eq!(
            PostgresMemory::str_to_category("custom_cat"),
            MemoryCategory::Custom("custom_cat".into())
        );
    }

    #[test]
    fn new_lazy_requires_url() {
        let config = PostgresConfig::default();
        // Remove env vars to ensure they don't interfere.
        // SAFETY: test-only, single-threaded test runner.
        unsafe { std::env::remove_var("POSTGRES_URL") };
        unsafe { std::env::remove_var("DATABASE_URL") };
        let result = PostgresMemory::new_lazy(&config);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Postgres memory backend requires url")
        );
    }

    #[test]
    fn new_lazy_accepts_config_url() {
        let config = PostgresConfig {
            url: Some("postgres://test:test@localhost/test".into()),
            ..PostgresConfig::default()
        };
        let mem = PostgresMemory::new_lazy(&config).unwrap();
        assert_eq!(mem.name(), "postgres");
        assert_eq!(mem.schema, "public");
        assert_eq!(mem.table, "memories");
    }

    #[test]
    fn qualified_table_format() {
        let config = PostgresConfig {
            url: Some("postgres://test:test@localhost/test".into()),
            schema: "custom_schema".into(),
            table: "custom_table".into(),
            ..PostgresConfig::default()
        };
        let mem = PostgresMemory::new_lazy(&config).unwrap();
        assert_eq!(mem.qualified_table(), "custom_schema.custom_table");
    }

    #[test]
    fn debug_shows_schema_and_table() {
        let config = PostgresConfig {
            url: Some("postgres://test:test@localhost/test".into()),
            ..PostgresConfig::default()
        };
        let mem = PostgresMemory::new_lazy(&config).unwrap();
        let debug = format!("{mem:?}");
        assert!(debug.contains("public"));
        assert!(debug.contains("memories"));
        assert!(debug.contains("connected: false"));
    }
}
