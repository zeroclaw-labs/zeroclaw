use super::traits::{Memory, MemoryCategory, MemoryEntry};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls, Row};
use uuid::Uuid;

/// Maximum allowed connect timeout (seconds) to avoid unreasonable waits.
const POSTGRES_CONNECT_TIMEOUT_CAP_SECS: u64 = 300;

/// PostgreSQL-backed persistent memory using `tokio-postgres` directly.
///
/// All operations are natively async — no sync wrapper crate, no OS-thread
/// trampoline, no nested-runtime panics.
pub struct PostgresMemory {
    client: Arc<Mutex<Client>>,
    qualified_table: String,
}

impl PostgresMemory {
    /// Connect to PostgreSQL and initialise the schema.
    ///
    /// Must be called from within a Tokio runtime (i.e. `.await`ed or wrapped
    /// in `tokio::task::block_in_place`).
    pub async fn connect(
        db_url: &str,
        schema: &str,
        table: &str,
        connect_timeout_secs: Option<u64>,
    ) -> Result<Self> {
        validate_identifier(schema, "storage schema")?;
        validate_identifier(table, "storage table")?;

        let schema_ident = quote_identifier(schema);
        let table_ident = quote_identifier(table);
        let qualified_table = format!("{schema_ident}.{table_ident}");

        let mut config: tokio_postgres::Config = db_url
            .parse()
            .context("invalid PostgreSQL connection URL")?;

        if let Some(timeout_secs) = connect_timeout_secs {
            let bounded = timeout_secs.min(POSTGRES_CONNECT_TIMEOUT_CAP_SECS);
            config.connect_timeout(Duration::from_secs(bounded));
        }

        let (client, connection) = config
            .connect(NoTls)
            .await
            .context("failed to connect to PostgreSQL memory backend")?;

        // Drive the connection in a background task; errors are logged but do
        // not crash the process.
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!(
                    error = %e,
                    "PostgreSQL memory connection closed with error"
                );
            }
        });

        let mut client = client;
        Self::init_schema(&mut client, &schema_ident, &qualified_table).await?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            qualified_table,
        })
    }

    async fn init_schema(
        client: &mut Client,
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
                    session_id TEXT
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
                "
            ))
            .await
            .context("failed to initialise PostgreSQL schema")?;
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

    fn row_to_entry(row: &Row) -> Result<MemoryEntry> {
        let timestamp: DateTime<Utc> = row.get(4);

        Ok(MemoryEntry {
            id: row.get(0),
            key: row.get(1),
            content: row.get(2),
            category: Self::parse_category(&row.get::<_, String>(3)),
            timestamp: timestamp.to_rfc3339(),
            session_id: row.get(5),
            score: row.try_get::<_, f64>(6).ok(),
            namespace: "default".into(),
            importance: None,
            superseded_by: None,
        })
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
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
        let stmt = format!(
            "
            INSERT INTO {qualified_table}
                (id, key, content, category, created_at, updated_at, session_id)
            VALUES
                ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (key) DO UPDATE SET
                content = EXCLUDED.content,
                category = EXCLUDED.category,
                updated_at = EXCLUDED.updated_at,
                session_id = EXCLUDED.session_id
            "
        );

        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let category = Self::category_to_str(&category);
        let sid = session_id.map(str::to_string);

        client
            .execute(&stmt, &[&id, &key, &content, &category, &now, &now, &sid])
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
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
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
                   (
                     CASE WHEN to_tsvector('simple', key) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', key), plainto_tsquery('simple', $1)) * 2.0
                       ELSE 0.0 END +
                     CASE WHEN to_tsvector('simple', content) @@ plainto_tsquery('simple', $1)
                       THEN ts_rank_cd(to_tsvector('simple', content), plainto_tsquery('simple', $1))
                       ELSE 0.0 END
                   )::FLOAT8 AS score
            FROM {qualified_table}
            WHERE ($2::TEXT IS NULL OR session_id = $2)
              AND ($1 = '' OR to_tsvector('simple', key || ' ' || content) @@ plainto_tsquery('simple', $1))
              {time_filter}
            ORDER BY score DESC, updated_at DESC
            LIMIT $3
            "
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
            .map(Self::row_to_entry)
            .collect::<Result<Vec<MemoryEntry>>>()
    }

    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>> {
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id
            FROM {qualified_table}
            WHERE key = $1
            LIMIT 1
            "
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
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
        let category = category.map(Self::category_to_str);
        let sid = session_id.map(str::to_string);

        let stmt = format!(
            "
            SELECT id, key, content, category, created_at, session_id
            FROM {qualified_table}
            WHERE ($1::TEXT IS NULL OR category = $1)
              AND ($2::TEXT IS NULL OR session_id = $2)
            ORDER BY updated_at DESC
            "
        );

        let rows = client
            .query(&stmt, &[&category, &sid])
            .await
            .context("failed to list memory entries")?;
        rows.iter()
            .map(Self::row_to_entry)
            .collect::<Result<Vec<MemoryEntry>>>()
    }

    async fn forget(&self, key: &str) -> Result<bool> {
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
        let stmt = format!("DELETE FROM {qualified_table} WHERE key = $1");
        let deleted = client
            .execute(&stmt, &[&key])
            .await
            .context("failed to forget memory entry")?;
        Ok(deleted > 0)
    }

    async fn count(&self) -> Result<usize> {
        let client = self.client.lock().await;
        let qualified_table = &self.qualified_table;
        let stmt = format!("SELECT COUNT(*) FROM {qualified_table}");
        let count: i64 = client
            .query_one(&stmt, &[])
            .await
            .context("failed to count memory entries")?
            .get(0);
        usize::try_from(count).context("PostgreSQL returned a negative memory count")
    }

    async fn health_check(&self) -> bool {
        let client = self.client.lock().await;
        client.simple_query("SELECT 1").await.is_ok()
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
    async fn connect_returns_error_not_panic_for_unreachable_db() {
        let result = PostgresMemory::connect(
            "postgres://zeroclaw:password@127.0.0.1:1/zeroclaw",
            "public",
            "memories",
            Some(1),
        )
        .await;

        assert!(
            result.is_err(),
            "connect should return an error for an unreachable endpoint"
        );
    }
}
