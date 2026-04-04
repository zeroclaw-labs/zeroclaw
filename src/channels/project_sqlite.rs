//! SQLite-backed project persistence for the gateway.
//!
//! Projects live in the same `sessions.db` as gateway sessions so that
//! `session_key` references stay consistent.  The DB path is:
//!
//!   `{gateway_workspace_dir}/sessions/sessions.db`
//!
//! Terminology used throughout this module:
//!  - **gateway_workspace_dir** – `~/.zeroclaw/workspace` (zeroclaw의 공유
//!    workspace dir, 모든 gateway 세션이 공유).
//!  - **project_workspace_dir** – 실제 프로젝트 디렉토리 (e.g.
//!    `/Users/me/my-project`).  에이전트 시작 시 `config.workspace_dir`로
//!    주입되어 agent의 파일 접근 샌드박스가 된다.

use anyhow::{Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Types ─────────────────────────────────────────────────────────

/// A gateway project record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayProject {
    /// Stable UUID — primary key and WS `?project_id=` query param.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Project workspace directory (project_workspace_dir).
    /// `Some` → used as `config.workspace_dir` for the agent (project sandbox).
    /// `None` → gateway_workspace_dir is used (default zeroclaw behaviour).
    pub project_workspace_dir: Option<String>,
    /// Session key in the sessions table — always `gw_{id}`.
    pub session_key: String,
    /// RFC-3339 creation timestamp.
    pub created_at: String,
    /// RFC-3339 timestamp of the last message, if any.
    pub last_msg_at: Option<String>,
}

// ── Backend ───────────────────────────────────────────────────────

/// SQLite-backed store for gateway projects.
///
/// Opens the same `sessions.db` used by `SqliteSessionBackend` so sessions
/// and projects share one file and reference each other via `session_key`.
pub struct ProjectBackend {
    conn: Mutex<Connection>,
}

impl ProjectBackend {
    /// Open (or create) the projects table inside `sessions.db`.
    ///
    /// `gateway_workspace_dir` is the shared workspace (e.g. `~/.zeroclaw/workspace`).
    /// DB path: `{gateway_workspace_dir}/sessions/sessions.db`.
    pub fn new(gateway_workspace_dir: &Path) -> Result<Self> {
        let sessions_dir = gateway_workspace_dir.join("sessions");
        std::fs::create_dir_all(&sessions_dir)
            .context("Failed to create sessions directory")?;
        let db_path = sessions_dir.join("sessions.db");

        let conn = Connection::open(&db_path)
            .with_context(|| format!("Failed to open session DB: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects (
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL,
                project_workspace_dir TEXT,
                session_key          TEXT NOT NULL UNIQUE,
                created_at           TEXT NOT NULL,
                last_msg_at          TEXT
             );",
        )
        .context("Failed to initialize projects table")?;

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Create a new project. `session_key` is always `gw_{id}`.
    pub fn create(
        &self,
        id: &str,
        name: &str,
        project_workspace_dir: Option<&str>,
    ) -> Result<GatewayProject> {
        let session_key = format!("gw_{id}");
        let created_at = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO projects (id, name, project_workspace_dir, session_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, name, project_workspace_dir, session_key, created_at],
        )
        .context("Failed to insert project")?;
        Ok(GatewayProject {
            id: id.to_string(),
            name: name.to_string(),
            project_workspace_dir: project_workspace_dir.map(|s| s.to_string()),
            session_key,
            created_at,
            last_msg_at: None,
        })
    }

    /// List all projects ordered by creation time descending.
    pub fn list(&self) -> Result<Vec<GatewayProject>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, project_workspace_dir, session_key, created_at, last_msg_at
             FROM projects ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(GatewayProject {
                id: row.get(0)?,
                name: row.get(1)?,
                project_workspace_dir: row.get(2)?,
                session_key: row.get(3)?,
                created_at: row.get(4)?,
                last_msg_at: row.get(5)?,
            })
        })?;
        rows.map(|r| r.context("Failed to read project row")).collect()
    }

    /// Get a single project by ID.
    pub fn get(&self, id: &str) -> Result<Option<GatewayProject>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, project_workspace_dir, session_key, created_at, last_msg_at
             FROM projects WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(GatewayProject {
                id: row.get(0)?,
                name: row.get(1)?,
                project_workspace_dir: row.get(2)?,
                session_key: row.get(3)?,
                created_at: row.get(4)?,
                last_msg_at: row.get(5)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r.context("Failed to read project row")?)),
            None => Ok(None),
        }
    }

    /// Rename a project. Returns `true` if a row was updated.
    pub fn rename(&self, id: &str, name: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE projects SET name = ?1 WHERE id = ?2",
            params![name, id],
        )?;
        Ok(n > 0)
    }

    /// Update project_workspace_dir. Pass `None` to clear. Returns `true` if updated.
    pub fn set_project_workspace_dir(
        &self,
        id: &str,
        project_workspace_dir: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock();
        let n = conn.execute(
            "UPDATE projects SET project_workspace_dir = ?1 WHERE id = ?2",
            params![project_workspace_dir, id],
        )?;
        Ok(n > 0)
    }

    /// Delete a project and its session messages. Returns `true` if deleted.
    pub fn delete(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock();
        let session_key: Option<String> = conn
            .query_row(
                "SELECT session_key FROM projects WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .ok();
        if let Some(ref key) = session_key {
            let _ = conn.execute("DELETE FROM sessions WHERE session_key = ?1", params![key]);
            let _ = conn.execute(
                "DELETE FROM session_metadata WHERE session_key = ?1",
                params![key],
            );
        }
        let n = conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Touch `last_msg_at` for the project that owns `session_key`.
    pub fn touch_by_session(&self, session_key: &str) {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock();
        let _ = conn.execute(
            "UPDATE projects SET last_msg_at = ?1 WHERE session_key = ?2",
            params![now, session_key],
        );
    }
}
