pub mod cli;
pub mod connector;
pub mod models;
pub mod orchestrator;
pub mod schema;

use anyhow::{Context, Result};
use powersync::{ConnectionPool, PowerSyncDatabase, SyncOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::connector::{ConnectorConfig, CreateOsConnector};
use self::models::{NewSprint, NewTask, Sprint, Task, TaskUpdate};
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
        tasks.spawn_with(|future| tokio::spawn(future));

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

    /// Get a single task by ID.
    pub async fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        let mut stmt = reader
            .prepare(
                "SELECT id, title, description, acceptance_criteria, domain_id, epic_id, \
                 sprint_id, user_story_id, parent_task_id, status, priority, task_type, \
                 task_category, due_date, do_date, agent_status, assigned_agent, branch_name, \
                 pr_url, ai_summary, note, notion_id, created_at, updated_at \
                 FROM createos_task WHERE id = ?",
            )
            .map_err(|e| anyhow::anyhow!("Failed to prepare query: {e}"))?;

        let mut rows = stmt
            .query_map(rusqlite::params![id], |row| {
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

        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(anyhow::anyhow!("Row parse error: {e}")),
            None => Ok(None),
        }
    }

    /// Get a single sprint by ID.
    pub async fn get_sprint(&self, id: &str) -> Result<Option<Sprint>> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        let mut stmt = reader
            .prepare(
                "SELECT id, name, domain_id, epic_id, status, objectives, \
                 start_date, end_date, quality_score, notion_id, created_at, updated_at \
                 FROM createos_sprint WHERE id = ?",
            )
            .map_err(|e| anyhow::anyhow!("Failed to prepare query: {e}"))?;

        let mut rows = stmt
            .query_map(rusqlite::params![id], |row| {
                Ok(Sprint {
                    id: row.get::<_, String>(0)?.parse().unwrap_or_default(),
                    name: row.get(1)?,
                    domain_id: row.get(2)?,
                    epic_id: row.get(3)?,
                    status: row.get(4)?,
                    objectives: row.get(5)?,
                    start_date: row.get(6)?,
                    end_date: row.get(7)?,
                    quality_score: row.get(8)?,
                    notion_id: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            })
            .map_err(|e| anyhow::anyhow!("Sprint query failed: {e}"))?;

        match rows.next() {
            Some(Ok(sprint)) => Ok(Some(sprint)),
            Some(Err(e)) => Err(anyhow::anyhow!("Row parse error: {e}")),
            None => Ok(None),
        }
    }

    /// List sprints with optional status filter.
    pub async fn list_sprints(&self, status: Option<&str>) -> Result<Vec<Sprint>> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        let status_owned = status.map(|s| s.to_string());

        let mut sql = String::from(
            "SELECT id, name, domain_id, epic_id, status, objectives, \
             start_date, end_date, quality_score, notion_id, created_at, updated_at \
             FROM createos_sprint WHERE 1=1",
        );
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref s) = status_owned {
            sql.push_str(" AND status = ?");
            params.push(Box::new(s.clone()));
        }
        sql.push_str(" ORDER BY start_date DESC NULLS LAST");

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let mut stmt = reader
            .prepare(&sql)
            .map_err(|e| anyhow::anyhow!("Failed to prepare sprint query: {e}"))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(Sprint {
                    id: row.get::<_, String>(0)?.parse().unwrap_or_default(),
                    name: row.get(1)?,
                    domain_id: row.get(2)?,
                    epic_id: row.get(3)?,
                    status: row.get(4)?,
                    objectives: row.get(5)?,
                    start_date: row.get(6)?,
                    end_date: row.get(7)?,
                    quality_score: row.get(8)?,
                    notion_id: row.get(9)?,
                    created_at: row.get(10)?,
                    updated_at: row.get(11)?,
                })
            })
            .map_err(|e| anyhow::anyhow!("Sprint query failed: {e}"))?;

        let mut sprints = Vec::new();
        for row in rows {
            sprints.push(row.map_err(|e| anyhow::anyhow!("Row parse error: {e}"))?);
        }
        Ok(sprints)
    }

    /// Count completed and total tasks in a sprint. Returns (done_count, total_count).
    pub async fn count_sprint_tasks(&self, sprint_id: &str) -> Result<(usize, usize)> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        let total: usize = reader
            .query_row(
                "SELECT COUNT(*) FROM createos_task WHERE sprint_id = ?",
                rusqlite::params![sprint_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("Count query failed: {e}"))?;

        let done: usize = reader
            .query_row(
                "SELECT COUNT(*) FROM createos_task WHERE sprint_id = ? AND status IN ('archived', 'cancelled')",
                rusqlite::params![sprint_id],
                |row| row.get(0),
            )
            .map_err(|e| anyhow::anyhow!("Done count query failed: {e}"))?;

        Ok((done, total))
    }

    /// Pick the next task to work on: highest-priority approved task (P1 first, then oldest).
    pub async fn pick_next_task(&self) -> Result<Option<Task>> {
        let reader = self
            .db
            .reader()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire reader: {e}"))?;

        let mut stmt = reader
            .prepare(
                "SELECT id, title, description, acceptance_criteria, domain_id, epic_id, \
                 sprint_id, user_story_id, parent_task_id, status, priority, task_type, \
                 task_category, due_date, do_date, agent_status, assigned_agent, branch_name, \
                 pr_url, ai_summary, note, notion_id, created_at, updated_at \
                 FROM createos_task \
                 WHERE status = 'approved' \
                 ORDER BY priority ASC, created_at ASC \
                 LIMIT 1",
            )
            .map_err(|e| anyhow::anyhow!("Failed to prepare query: {e}"))?;

        let mut rows = stmt
            .query_map([], |row| {
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
            .map_err(|e| anyhow::anyhow!("Pick next task query failed: {e}"))?;

        match rows.next() {
            Some(Ok(task)) => Ok(Some(task)),
            Some(Err(e)) => Err(anyhow::anyhow!("Row parse error: {e}")),
            None => Ok(None),
        }
    }

    // =========================================================================
    // Write operations (queued for PowerSync upstream sync)
    // =========================================================================

    /// Create a new task. Returns the created Task.
    pub async fn create_task(&self, new: NewTask) -> Result<Task> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let writer = self
            .db
            .writer()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire writer: {e}"))?;

        writer
            .execute(
                "INSERT INTO createos_task (id, title, description, acceptance_criteria, \
                 domain_id, epic_id, sprint_id, user_story_id, parent_task_id, status, \
                 priority, task_type, task_category, due_date, do_date, agent_status, \
                 assigned_agent, branch_name, pr_url, ai_summary, note, notion_id, \
                 created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    id,
                    new.title,
                    new.description,
                    new.acceptance_criteria,
                    new.domain_id,
                    new.epic_id,
                    new.sprint_id,
                    new.user_story_id,
                    None::<String>, // parent_task_id
                    new.status,
                    new.priority,
                    new.task_type,
                    new.task_category,
                    new.due_date,
                    None::<String>, // do_date
                    "",             // agent_status
                    "",             // assigned_agent
                    "",             // branch_name
                    "",             // pr_url
                    "",             // ai_summary
                    "",             // note
                    "",             // notion_id
                    now,
                    now,
                ],
            )
            .map_err(|e| anyhow::anyhow!("Failed to insert task: {e}"))?;

        drop(writer);
        self.get_task(&id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Task was inserted but could not be read back"))
    }

    /// Update a task's status.
    pub async fn update_task_status(&self, id: &str, status: &str) -> Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let writer = self
            .db
            .writer()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire writer: {e}"))?;

        let changed = writer
            .execute(
                "UPDATE createos_task SET status = ?, updated_at = ? WHERE id = ?",
                rusqlite::params![status, now, id],
            )
            .map_err(|e| anyhow::anyhow!("Failed to update task status: {e}"))?;

        if changed == 0 {
            anyhow::bail!("Task {id} not found");
        }
        Ok(())
    }

    /// General task update — applies only the fields that are Some.
    pub async fn update_task(&self, id: &str, updates: TaskUpdate) -> Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let mut sets = vec!["updated_at = ?".to_string()];
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(now)];

        macro_rules! maybe_set {
            ($field:ident, $col:expr) => {
                if let Some(val) = updates.$field {
                    sets.push(format!("{} = ?", $col));
                    params.push(Box::new(val));
                }
            };
        }

        maybe_set!(title, "title");
        maybe_set!(description, "description");
        maybe_set!(acceptance_criteria, "acceptance_criteria");
        maybe_set!(sprint_id, "sprint_id");
        maybe_set!(status, "status");
        maybe_set!(priority, "priority");
        maybe_set!(assigned_agent, "assigned_agent");
        maybe_set!(agent_status, "agent_status");
        maybe_set!(branch_name, "branch_name");
        maybe_set!(pr_url, "pr_url");
        maybe_set!(ai_summary, "ai_summary");
        maybe_set!(note, "note");

        params.push(Box::new(id.to_string()));

        let sql = format!("UPDATE createos_task SET {} WHERE id = ?", sets.join(", "));

        let writer = self
            .db
            .writer()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire writer: {e}"))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|p| p.as_ref()).collect();
        let changed = writer
            .execute(&sql, param_refs.as_slice())
            .map_err(|e| anyhow::anyhow!("Failed to update task: {e}"))?;

        if changed == 0 {
            anyhow::bail!("Task {id} not found");
        }
        Ok(())
    }

    /// Create a new sprint. Returns the created Sprint.
    pub async fn create_sprint(&self, new: NewSprint) -> Result<Sprint> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let writer = self
            .db
            .writer()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire writer: {e}"))?;

        writer
            .execute(
                "INSERT INTO createos_sprint (id, name, domain_id, epic_id, status, objectives, \
                 start_date, end_date, quality_score, notion_id, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    id,
                    new.name,
                    new.domain_id,
                    new.epic_id,
                    "not_started",
                    new.objectives,
                    new.start_date,
                    new.end_date,
                    None::<f64>, // quality_score
                    "",          // notion_id
                    now,
                    now,
                ],
            )
            .map_err(|e| anyhow::anyhow!("Failed to insert sprint: {e}"))?;

        drop(writer);
        self.get_sprint(&id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Sprint was inserted but could not be read back"))
    }

    /// Update a sprint's status.
    pub async fn update_sprint_status(&self, id: &str, status: &str) -> Result<()> {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let writer = self
            .db
            .writer()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to acquire writer: {e}"))?;

        let changed = writer
            .execute(
                "UPDATE createos_sprint SET status = ?, updated_at = ? WHERE id = ?",
                rusqlite::params![status, now, id],
            )
            .map_err(|e| anyhow::anyhow!("Failed to update sprint status: {e}"))?;

        if changed == 0 {
            anyhow::bail!("Sprint {id} not found");
        }
        Ok(())
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

#[http_client::async_trait]
impl http_client::HttpClient for ReqwestHttpClient {
    async fn send(
        &self,
        mut req: http_client::Request,
    ) -> Result<http_client::Response, http_client::Error> {
        let url = req.url().to_string();
        let method = match req.method() {
            http_client::http_types::Method::Post => reqwest::Method::POST,
            http_client::http_types::Method::Put => reqwest::Method::PUT,
            http_client::http_types::Method::Delete => reqwest::Method::DELETE,
            http_client::http_types::Method::Patch => reqwest::Method::PATCH,
            http_client::http_types::Method::Head => reqwest::Method::HEAD,
            http_client::http_types::Method::Options => reqwest::Method::OPTIONS,
            _ => reqwest::Method::GET,
        };

        let mut builder = self.client.request(method, &url);

        // Copy headers
        for (name, values) in &req {
            for value in values {
                builder = builder.header(name.as_str(), value.as_str());
            }
        }

        // Copy body
        let body = req.take_body();
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
        let status = http_client::http_types::StatusCode::try_from(status_code).map_err(|e| {
            http_client::Error::from_str(
                http_client::http_types::StatusCode::InternalServerError,
                format!("Invalid status code: {e}"),
            )
        })?;

        let mut http_resp = http_client::Response::new(status);

        // Copy response headers
        for (key, value) in resp.headers() {
            if let Ok(val) = value.to_str() {
                http_resp.append_header(key.as_str(), val);
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
    }
}
