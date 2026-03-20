pub mod cli;
pub mod connector;
pub mod models;
pub mod schema;

use anyhow::{Context, Result};
use powersync::{ConnectionPool, PowerSyncDatabase, SyncOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::connector::{ConnectorConfig, CreateOsConnector};
use self::models::Task;
use self::schema::createos_schema;

/// Local-first createOS database backed by PowerSync + SQLite.
///
/// Wraps `PowerSyncDatabase` with typed query methods for the 5 core
/// createOS models: LifeDomain, Epic, Sprint, Task, UserStory.
pub struct CreateOsDb {
    db: PowerSyncDatabase,
    db_path: PathBuf,
}

impl CreateOsDb {
    /// Open (or create) the createOS SQLite database.
    ///
    /// Database is stored at `{workspace_dir}/createos/createos.db`.
    pub fn open(workspace_dir: &Path) -> Result<Self> {
        let db_dir = workspace_dir.join("createos");
        std::fs::create_dir_all(&db_dir).context("Failed to create createos database directory")?;

        let db_path = db_dir.join("createos.db");

        // Initialize PowerSync SQLite extension (safe to call multiple times)
        powersync::env::PowerSyncEnvironment::powersync_auto_extension()
            .map_err(|e| anyhow::anyhow!("Failed to initialize PowerSync extension: {e}"))?;

        let pool = ConnectionPool::open(&db_path)
            .map_err(|e| anyhow::anyhow!("Failed to open createOS database: {e}"))?;

        let schema = createos_schema();

        let timer = powersync::env::PowerSyncEnvironment::tokio_timer();
        let env = powersync::env::PowerSyncEnvironment::custom(
            Arc::new(ReqwestHttpClient::new()),
            pool,
            Box::new(timer),
        );

        let db = PowerSyncDatabase::new(env, schema);

        Ok(Self { db, db_path })
    }

    /// Get a reference to the underlying PowerSync database.
    pub fn inner(&self) -> &PowerSyncDatabase {
        &self.db
    }

    /// Connect to the PowerSync service for bidirectional sync.
    pub async fn connect(&self, config: ConnectorConfig) -> Result<()> {
        let connector = CreateOsConnector::new(config, self.db.clone());

        // Start async tasks (download + upload actors)
        let tasks = self.db.async_tasks();
        tokio::spawn(tasks.uploads);
        tokio::spawn(tasks.downloads);

        let options = SyncOptions::new(connector);
        self.db.connect(options).await;

        Ok(())
    }

    /// List tasks with optional filters.
    pub async fn list_tasks(
        &self,
        status: Option<&str>,
        sprint_id: Option<&str>,
    ) -> Result<Vec<Task>> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        // LeasedConnection derefs to rusqlite::Connection
        let status_owned = status.map(|s| s.to_string());
        let sprint_owned = sprint_id.map(|s| s.to_string());

        let mut sql = String::from(
            "SELECT id, title, description, acceptance_criteria, domain_id, epic_id, \
             sprint_id, user_story_id, parent_task_id, status, priority, task_type, \
             task_category, due_date, do_date, agent_status, assigned_agent, branch_name, \
             pr_url, ai_summary, note, notion_id, created_at, updated_at \
             FROM createos_task WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref s) = status_owned {
            sql.push_str(" AND status = ?");
            params.push(Box::new(s.clone()));
        }
        if let Some(ref sid) = sprint_owned {
            sql.push_str(" AND sprint_id = ?");
            params.push(Box::new(sid.clone()));
        }
        sql.push_str(" ORDER BY priority ASC, created_at DESC");

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = reader
            .prepare(&sql)
            .map_err(|e| anyhow::anyhow!("Failed to prepare task query: {e}"))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(Task {
                    id: row.get::<_, String>(0)?.parse().unwrap_or_default(),
                    title: row.get(1)?,
                    description: row.get(2)?,
                    acceptance_criteria: row.get(3)?,
                    domain_id: row.get(4)?,
                    epic_id: row.get(5)?,
                    sprint_id: row.get(6)?,
                    user_story_id: row.get(7)?,
                    parent_task_id: row.get(8)?,
                    status: row.get(9)?,
                    priority: row.get(10)?,
                    task_type: row.get(11)?,
                    task_category: row.get(12)?,
                    due_date: row.get(13)?,
                    do_date: row.get(14)?,
                    agent_status: row.get(15)?,
                    assigned_agent: row.get(16)?,
                    branch_name: row.get(17)?,
                    pr_url: row.get(18)?,
                    ai_summary: row.get(19)?,
                    note: row.get(20)?,
                    notion_id: row.get(21)?,
                    created_at: row.get(22)?,
                    updated_at: row.get(23)?,
                })
            })
            .map_err(|e| anyhow::anyhow!("Task query failed: {e}"))?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.map_err(|e| anyhow::anyhow!("Row parse error: {e}"))?);
        }

        Ok(tasks)
    }

    /// Get sync status.
    pub fn sync_status(&self) -> Arc<powersync::SyncStatusData> {
        self.db.status()
    }

    /// Disconnect from sync service.
    pub async fn disconnect(&self) {
        self.db.disconnect().await;
    }

    /// Database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }
}

// =============================================================================
// HTTP client adapter for PowerSync using reqwest
// =============================================================================

/// Wraps reqwest to implement the `http_client::HttpClient` trait
/// required by `PowerSyncEnvironment`.
#[derive(Debug)]
struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl http_client::HttpClient for ReqwestHttpClient {
    fn send(
        &self,
        req: http_client::http_types::Request,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<http_client::http_types::Response, http_client::Error>,
                > + Send,
        >,
    > {
        let client = self.client.clone();

        Box::pin(async move {
            let url = req.url().to_string();
            let method = match req.method() {
                http_client::http_types::Method::Get => reqwest::Method::GET,
                http_client::http_types::Method::Post => reqwest::Method::POST,
                http_client::http_types::Method::Put => reqwest::Method::PUT,
                http_client::http_types::Method::Delete => reqwest::Method::DELETE,
                http_client::http_types::Method::Patch => reqwest::Method::PATCH,
                http_client::http_types::Method::Head => reqwest::Method::HEAD,
                http_client::http_types::Method::Options => reqwest::Method::OPTIONS,
                _ => reqwest::Method::GET,
            };

            let mut builder = client.request(method, &url);

            // Copy headers
            for (name, values) in req.iter() {
                for value in values.iter() {
                    builder = builder.header(name.as_str(), value.as_str());
                }
            }

            // Copy body
            let body_bytes = req.into();
            let body: http_client::http_types::Body = body_bytes;
            let bytes = body.into_bytes().await.map_err(|e| {
                http_client::Error::from_str(
                    http_client::http_types::StatusCode::InternalServerError,
                    format!("Failed to read request body: {e}"),
                )
            })?;
            if !bytes.is_empty() {
                builder = builder.body(bytes.to_vec());
            }

            let resp = builder.send().await.map_err(|e| {
                http_client::Error::from_str(
                    http_client::http_types::StatusCode::InternalServerError,
                    format!("HTTP request failed: {e}"),
                )
            })?;

            let status_code = resp.status().as_u16();
            let status =
                http_client::http_types::StatusCode::try_from(status_code).map_err(|e| {
                    http_client::Error::from_str(
                        http_client::http_types::StatusCode::InternalServerError,
                        format!("Invalid status code: {e}"),
                    )
                })?;

            let mut http_resp = http_client::http_types::Response::new(status);

            // Copy response headers
            for (key, value) in resp.headers() {
                if let Ok(val) = value.to_str() {
                    let name = http_client::http_types::headers::HeaderName::from_bytes(
                        key.as_str().as_bytes().to_vec(),
                    )
                    .map_err(|e| {
                        http_client::Error::from_str(
                            http_client::http_types::StatusCode::InternalServerError,
                            format!("Invalid header name: {e}"),
                        )
                    })?;
                    let header_val = http_client::http_types::headers::HeaderValue::from_bytes(
                        val.as_bytes().to_vec(),
                    )
                    .map_err(|e| {
                        http_client::Error::from_str(
                            http_client::http_types::StatusCode::InternalServerError,
                            format!("Invalid header value: {e}"),
                        )
                    })?;
                    http_resp.insert_header(name, header_val);
                }
            }

            let body_bytes = resp.bytes().await.map_err(|e| {
                http_client::Error::from_str(
                    http_client::http_types::StatusCode::InternalServerError,
                    format!("Failed to read response body: {e}"),
                )
            })?;
            http_resp.set_body(body_bytes.as_ref());

            Ok(http_resp)
        })
    }
}
