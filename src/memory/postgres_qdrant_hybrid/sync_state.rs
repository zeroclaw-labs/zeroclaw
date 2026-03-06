use anyhow::{Context, Result};
use chrono::Utc;
use postgres::{Client, NoTls};
use std::time::Duration;

const POSTGRES_CONNECT_TIMEOUT_CAP_SECS: u64 = 300;
const LAST_ERROR_MAX_CHARS: usize = 2048;

#[derive(Debug, Clone, Copy)]
pub enum SyncOp {
    Upsert,
    Delete,
}

impl SyncOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Upsert => "upsert",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone)]
pub struct SyncStateStore {
    db_url: String,
    connect_timeout_secs: Option<u64>,
    tls_mode: bool,
    qualified_table: String,
}

impl SyncStateStore {
    pub fn new(
        db_url: &str,
        schema: &str,
        connect_timeout_secs: Option<u64>,
        tls_mode: bool,
    ) -> Result<Self> {
        validate_identifier(schema, "storage schema")?;
        let qualified_table = format!("{}.\"memories_qdrant_sync\"", quote_identifier(schema));
        let store = Self {
            db_url: db_url.to_string(),
            connect_timeout_secs,
            tls_mode,
            qualified_table,
        };
        store.init_schema()?;
        Ok(store)
    }

    pub async fn set_pending(
        &self,
        key: &str,
        op: SyncOp,
        content_hash: Option<&str>,
    ) -> Result<()> {
        let key = key.to_string();
        let op = op.as_str().to_string();
        let content_hash = content_hash.map(str::to_string);
        let table = self.qualified_table.clone();
        self.run_db_task(move |client| {
            let now = Utc::now();
            let stmt = format!(
                "\
                INSERT INTO {table} (key, op, status, attempt_count, last_error, updated_at, last_attempt_at, last_synced_at, content_hash)
                VALUES ($1, $2, 'pending', 0, NULL, $3, NULL, NULL, $4)
                ON CONFLICT (key) DO UPDATE SET
                    op = EXCLUDED.op,
                    status = 'pending',
                    updated_at = EXCLUDED.updated_at,
                    content_hash = EXCLUDED.content_hash,
                    attempt_count = 0,
                    last_error = NULL,
                    last_attempt_at = NULL
                "
            );
            client.execute(&stmt, &[&key, &op, &now, &content_hash])?;
            Ok(())
        })
        .await
    }

    pub async fn mark_synced(
        &self,
        key: &str,
        expected_op: SyncOp,
        expected_content_hash: Option<&str>,
    ) -> Result<()> {
        let key = key.to_string();
        let expected_op = expected_op.as_str().to_string();
        let expected_content_hash = expected_content_hash.map(str::to_string);
        let table = self.qualified_table.clone();
        self.run_db_task(move |client| {
            let now = Utc::now();
            let stmt = format!(
                "UPDATE {table}
                 SET status='synced', last_error=NULL, last_synced_at=$2, updated_at=$2
                 WHERE key=$1
                   AND status='pending'
                   AND op=$3
                   AND (
                       ($4 IS NULL AND content_hash IS NULL)
                       OR content_hash=$4
                   )"
            );
            let affected = client.execute(&stmt, &[&key, &now, &expected_op, &expected_content_hash])?;
            if affected == 0 {
                anyhow::bail!("sync state changed concurrently for key '{key}' in {table}");
            }
            Ok(())
        })
        .await
    }

    pub async fn mark_failed(
        &self,
        key: &str,
        error: &str,
        expected_op: SyncOp,
        expected_content_hash: Option<&str>,
    ) -> Result<()> {
        let key = key.to_string();
        let error = sanitize_error_for_storage(error);
        let expected_op = expected_op.as_str().to_string();
        let expected_content_hash = expected_content_hash.map(str::to_string);
        let table = self.qualified_table.clone();
        self.run_db_task(move |client| {
            let now = Utc::now();
            let stmt = format!(
                "UPDATE {table}
                 SET status='failed', last_error=$2, attempt_count=attempt_count+1, last_attempt_at=$3, updated_at=$3
                 WHERE key=$1
                   AND status='pending'
                   AND op=$4
                   AND (
                       ($5 IS NULL AND content_hash IS NULL)
                       OR content_hash=$5
                   )"
            );
            let affected = client.execute(
                &stmt,
                &[&key, &error, &now, &expected_op, &expected_content_hash],
            )?;
            if affected == 0 {
                anyhow::bail!("sync state changed concurrently for key '{key}' in {table}");
            }
            Ok(())
        })
        .await
    }

    pub async fn is_pending_upsert_hash(&self, key: &str, expected_hash: &str) -> Result<bool> {
        let key = key.to_string();
        let expected_hash = expected_hash.to_string();
        let table = self.qualified_table.clone();
        self.run_db_task(move |client| {
            let stmt = format!("SELECT op, status, content_hash FROM {table} WHERE key=$1");
            let row = client.query_opt(&stmt, &[&key])?;
            let Some(row) = row else {
                return Ok(false);
            };
            let op: String = row.get(0);
            let status: String = row.get(1);
            let hash: Option<String> = row.get(2);
            Ok(op == SyncOp::Upsert.as_str()
                && status == "pending"
                && hash.as_deref() == Some(expected_hash.as_str()))
        })
        .await
    }

    fn init_schema(&self) -> Result<()> {
        let table = self.qualified_table.clone();
        self.run_db_task_sync(move |client| {
            let lock_key = format!("{table}:init");
            client.query("SELECT pg_advisory_lock(hashtext($1))", &[&lock_key])?;

            let init_result = client.batch_execute(&format!(
                "\
                CREATE TABLE IF NOT EXISTS {table} (
                    key TEXT PRIMARY KEY,
                    op TEXT NOT NULL,
                    status TEXT NOT NULL,
                    attempt_count INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT,
                    updated_at TIMESTAMPTZ NOT NULL,
                    last_attempt_at TIMESTAMPTZ,
                    last_synced_at TIMESTAMPTZ,
                    content_hash TEXT
                );
                CREATE INDEX IF NOT EXISTS idx_memories_qdrant_sync_status_updated ON {table}(status, updated_at DESC);
                CREATE INDEX IF NOT EXISTS idx_memories_qdrant_sync_op_status ON {table}(op, status);
                CREATE INDEX IF NOT EXISTS idx_memories_qdrant_sync_last_attempt ON {table}(last_attempt_at);
                "
            ));

            let _ = client.query("SELECT pg_advisory_unlock(hashtext($1))", &[&lock_key]);
            init_result?;
            Ok(())
        })
    }

    async fn run_db_task<T, F>(&self, task: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Client) -> Result<T> + Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.run_db_task_sync(task))
            .await
            .context("failed to join sync state task")?
    }

    fn run_db_task_sync<T, F>(&self, task: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Client) -> Result<T> + Send + 'static,
    {
        let db_url = self.db_url.clone();
        let connect_timeout_secs = self.connect_timeout_secs;
        let tls_mode = self.tls_mode;
        let mut client = connect_client(&db_url, connect_timeout_secs, tls_mode)?;
        task(&mut client)
    }
}

fn connect_client(
    db_url: &str,
    connect_timeout_secs: Option<u64>,
    tls_mode: bool,
) -> Result<Client> {
    let mut config: postgres::Config = db_url
        .parse()
        .context("invalid PostgreSQL connection URL")?;
    if let Some(timeout_secs) = connect_timeout_secs {
        config.connect_timeout(Duration::from_secs(
            timeout_secs.min(POSTGRES_CONNECT_TIMEOUT_CAP_SECS),
        ));
    }

    if tls_mode {
        let tls_insecure_skip_verify = storage_tls_insecure_skip_verify();
        let tls_config = if tls_insecure_skip_verify {
            tracing::warn!(
                "ZEROCLAW_STORAGE_TLS_INSECURE_SKIP_VERIFY is enabled; TLS certificate verification is disabled"
            );
            let mut config = rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth();
            config
                .dangerous()
                .set_certificate_verifier(super::tls::insecure_verifier());
            config
        } else {
            let root_store: rustls::RootCertStore =
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };
        let tls = tokio_postgres_rustls::MakeRustlsConnect::new(tls_config);
        config
            .connect(tls)
            .context("failed to connect PostgreSQL sync state (TLS)")
    } else {
        config
            .connect(NoTls)
            .context("failed to connect PostgreSQL sync state")
    }
}

fn storage_tls_insecure_skip_verify() -> bool {
    std::env::var("ZEROCLAW_STORAGE_TLS_INSECURE_SKIP_VERIFY")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn sanitize_error_for_storage(error: &str) -> String {
    let mut out = String::with_capacity(error.len().min(LAST_ERROR_MAX_CHARS));
    let mut count = 0usize;
    for ch in error.chars() {
        if count >= LAST_ERROR_MAX_CHARS {
            break;
        }
        if ch.is_control() {
            out.push(' ');
        } else {
            out.push(ch);
        }
        count += 1;
    }
    out.trim().to_string()
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
