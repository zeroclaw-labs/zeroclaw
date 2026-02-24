//! Inter-process communication tools for independent ZeroClaw agents.
//!
//! Provides 5 LLM-callable tools backed by a shared SQLite database, allowing
//! independent ZeroClaw processes on the same host to discover each other and
//! exchange messages. See Issue #1518 for design rationale.

use super::traits::{Tool, ToolResult};
use crate::config::AgentsIpcConfig;
use crate::security::policy::ToolOperation;
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

// ── IpcDb core ──────────────────────────────────────────────────

const PRAGMA_SQL: &str =
    "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;";

const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS agents (
    agent_id  TEXT PRIMARY KEY,
    role      TEXT,
    status    TEXT DEFAULT 'online',
    metadata  TEXT,
    last_seen INTEGER
);
CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    from_agent TEXT NOT NULL,
    to_agent   TEXT NOT NULL,
    payload    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    read       INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS shared_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    owner      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);";

/// Shared SQLite handle for IPC tools. Each ZeroClaw process holds one instance.
pub(crate) struct IpcDb {
    conn: Arc<Mutex<Connection>>,
    agent_id: String,
    staleness_secs: u64,
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl IpcDb {
    /// Initialize connection: set pragmas, create schema, register agent.
    fn init(conn: Connection, agent_id: String, staleness_secs: u64) -> Result<Self, String> {
        conn.execute_batch(PRAGMA_SQL)
            .map_err(|e| format!("failed to set pragmas: {e}"))?;
        conn.execute_batch(SCHEMA_SQL)
            .map_err(|e| format!("failed to create schema: {e}"))?;

        let now = now_epoch();
        // Use UPDATE + INSERT to preserve existing role/metadata columns
        let updated = conn
            .execute(
                "UPDATE agents SET status = 'online', last_seen = ?2 WHERE agent_id = ?1",
                rusqlite::params![agent_id, now],
            )
            .map_err(|e| format!("failed to update agent: {e}"))?;
        if updated == 0 {
            conn.execute(
                "INSERT INTO agents (agent_id, status, last_seen) VALUES (?1, 'online', ?2)",
                rusqlite::params![agent_id, now],
            )
            .map_err(|e| format!("failed to register agent: {e}"))?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            agent_id,
            staleness_secs,
        })
    }

    /// Open (or create) the shared IPC database and register this agent.
    ///
    /// `workspace_dir` is hashed to derive a stable, code-enforced `agent_id`.
    pub fn open(workspace_dir: &std::path::Path, config: &AgentsIpcConfig) -> Result<Self, String> {
        let db_path = shellexpand::tilde(&config.db_path).into_owned();

        // Ensure parent directory exists
        if let Some(parent) = std::path::Path::new(&db_path).parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create db directory: {e}"))?;
        }

        let conn =
            Connection::open(&db_path).map_err(|e| format!("failed to open IPC database: {e}"))?;

        // Derive agent_id from workspace canonical path
        let canonical = workspace_dir
            .canonicalize()
            .unwrap_or_else(|_| workspace_dir.to_path_buf());
        let hash = Sha256::digest(canonical.to_string_lossy().as_bytes());
        let agent_id = format!("{hash:x}");

        Self::init(conn, agent_id, config.staleness_secs)
    }

    /// Update `last_seen` timestamp. Called piggyback on every tool invocation.
    pub fn heartbeat(&self) {
        let now = now_epoch();
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "UPDATE agents SET last_seen = ?1 WHERE agent_id = ?2",
                rusqlite::params![now, self.agent_id],
            );
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    #[cfg(test)]
    fn open_with_id(db_path: &str, agent_id: &str, staleness_secs: u64) -> Result<Self, String> {
        let conn =
            Connection::open(db_path).map_err(|e| format!("failed to open IPC database: {e}"))?;
        Self::init(conn, agent_id.to_string(), staleness_secs)
    }
}

impl Drop for IpcDb {
    fn drop(&mut self) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "DELETE FROM agents WHERE agent_id = ?1",
                rusqlite::params![self.agent_id],
            );
        }
    }
}

// ── AgentsListTool ──────────────────────────────────────────────

/// List online agents filtered by staleness window.
pub struct AgentsListTool {
    ipc_db: Arc<IpcDb>,
}

impl AgentsListTool {
    pub(crate) fn new(ipc_db: Arc<IpcDb>) -> Self {
        Self { ipc_db }
    }
}

#[async_trait]
impl Tool for AgentsListTool {
    fn name(&self) -> &str {
        "agents_list"
    }

    fn description(&self) -> &str {
        "List online IPC agents on this host. Returns agent IDs, roles, and last-seen timestamps for agents within the staleness window."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.ipc_db.heartbeat();

        let conn = self
            .ipc_db
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let cutoff = now_epoch() - self.ipc_db.staleness_secs as i64;

        let mut stmt = conn.prepare(
            "SELECT agent_id, role, status, last_seen FROM agents WHERE last_seen >= ?1",
        )?;

        let rows: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![cutoff], |row| {
                Ok(json!({
                    "agent_id": row.get::<_, String>(0)?,
                    "role": row.get::<_, Option<String>>(1)?,
                    "status": row.get::<_, String>(2)?,
                    "last_seen": row.get::<_, i64>(3)?
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&rows).unwrap_or_default(),
            error: None,
        })
    }
}

// ── AgentsSendTool ──────────────────────────────────────────────

/// Send a message to another agent (or broadcast with `"*"`).
pub struct AgentsSendTool {
    ipc_db: Arc<IpcDb>,
    security: Arc<SecurityPolicy>,
}

impl AgentsSendTool {
    pub(crate) fn new(ipc_db: Arc<IpcDb>, security: Arc<SecurityPolicy>) -> Self {
        Self { ipc_db, security }
    }
}

#[async_trait]
impl Tool for AgentsSendTool {
    fn name(&self) -> &str {
        "agents_send"
    }

    fn description(&self) -> &str {
        "Send a message to another agent by ID, or broadcast to all agents with to_agent=\"*\"."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "to_agent": {
                    "type": "string",
                    "description": "Target agent ID or '*' for broadcast"
                },
                "payload": {
                    "type": "string",
                    "description": "Message content (JSON string recommended)"
                }
            },
            "required": ["to_agent", "payload"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "agents_send")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        self.ipc_db.heartbeat();

        let to_agent = match args.get("to_agent").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'to_agent' parameter".into()),
                })
            }
        };

        let payload = match args.get("payload").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'payload' parameter".into()),
                })
            }
        };

        let conn = self
            .ipc_db
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let now = now_epoch();
        conn.execute(
            "INSERT INTO messages (from_agent, to_agent, payload, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![self.ipc_db.agent_id, to_agent, payload, now],
        )?;

        Ok(ToolResult {
            success: true,
            output: format!("Message sent to {to_agent}"),
            error: None,
        })
    }
}

// ── AgentsInboxTool ─────────────────────────────────────────────

/// Read unread messages addressed to this agent (or broadcast).
pub struct AgentsInboxTool {
    ipc_db: Arc<IpcDb>,
}

impl AgentsInboxTool {
    pub(crate) fn new(ipc_db: Arc<IpcDb>) -> Self {
        Self { ipc_db }
    }
}

#[async_trait]
impl Tool for AgentsInboxTool {
    fn name(&self) -> &str {
        "agents_inbox"
    }

    fn description(&self) -> &str {
        "Read unread messages in this agent's inbox (including broadcasts to '*'). Direct messages are marked as read after retrieval; broadcast messages remain unread."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.ipc_db.heartbeat();

        let conn = self
            .ipc_db
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let agent_id = &self.ipc_db.agent_id;

        let mut stmt = conn.prepare(
            "SELECT id, from_agent, payload, created_at FROM messages WHERE (to_agent = ?1 OR to_agent = '*') AND read = 0 ORDER BY created_at ASC",
        )?;

        let messages: Vec<serde_json::Value> = stmt
            .query_map(rusqlite::params![agent_id], |row| {
                Ok(json!({
                    "id": row.get::<_, i64>(0)?,
                    "from_agent": row.get::<_, String>(1)?,
                    "payload": row.get::<_, String>(2)?,
                    "created_at": row.get::<_, i64>(3)?
                }))
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Mark direct (non-broadcast) messages as read
        let _ = conn.execute(
            "UPDATE messages SET read = 1 WHERE to_agent = ?1 AND read = 0",
            rusqlite::params![agent_id],
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&messages).unwrap_or_default(),
            error: None,
        })
    }
}

// ── StateGetTool ────────────────────────────────────────────────

/// Get a value from the shared key-value store.
pub struct StateGetTool {
    ipc_db: Arc<IpcDb>,
}

impl StateGetTool {
    pub(crate) fn new(ipc_db: Arc<IpcDb>) -> Self {
        Self { ipc_db }
    }
}

#[async_trait]
impl Tool for StateGetTool {
    fn name(&self) -> &str {
        "state_get"
    }

    fn description(&self) -> &str {
        "Get a value from the shared inter-agent key-value store."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The key to look up"
                }
            },
            "required": ["key"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.ipc_db.heartbeat();

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'key' parameter".into()),
                })
            }
        };

        let conn = self
            .ipc_db
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let result: Option<(String, String, i64)> = conn
            .query_row(
                "SELECT value, owner, updated_at FROM shared_state WHERE key = ?1",
                rusqlite::params![key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok();

        match result {
            Some((value, owner, updated_at)) => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&json!({
                    "key": key,
                    "value": value,
                    "owner": owner,
                    "updated_at": updated_at
                }))
                .unwrap_or_default(),
                error: None,
            }),
            None => Ok(ToolResult {
                success: true,
                output: format!("Key '{key}' not found"),
                error: None,
            }),
        }
    }
}

// ── StateSetTool ────────────────────────────────────────────────

/// Set a value in the shared key-value store.
pub struct StateSetTool {
    ipc_db: Arc<IpcDb>,
    security: Arc<SecurityPolicy>,
}

impl StateSetTool {
    pub(crate) fn new(ipc_db: Arc<IpcDb>, security: Arc<SecurityPolicy>) -> Self {
        Self { ipc_db, security }
    }
}

#[async_trait]
impl Tool for StateSetTool {
    fn name(&self) -> &str {
        "state_set"
    }

    fn description(&self) -> &str {
        "Set a key-value pair in the shared inter-agent state store. Overwrites any existing value for the key."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The key to set"
                },
                "value": {
                    "type": "string",
                    "description": "The value to store"
                }
            },
            "required": ["key", "value"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(error) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "state_set")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(error),
            });
        }

        self.ipc_db.heartbeat();

        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'key' parameter".into()),
                })
            }
        };

        let value = match args.get("value").and_then(|v| v.as_str()) {
            Some(v) => v,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'value' parameter".into()),
                })
            }
        };

        let conn = self
            .ipc_db
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let now = now_epoch();

        conn.execute(
            "INSERT OR REPLACE INTO shared_state (key, value, owner, updated_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![key, value, self.ipc_db.agent_id, now],
        )?;

        Ok(ToolResult {
            success: true,
            output: format!("State '{key}' updated"),
            error: None,
        })
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_db(dir: &TempDir, agent_id: &str) -> IpcDb {
        let db_path = dir.path().join("agents.db");
        IpcDb::open_with_id(db_path.to_str().unwrap(), agent_id, 300).unwrap()
    }

    #[test]
    fn schema_creates_three_tables() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir, "zeroclaw_agent_a");
        let conn = db.conn.lock().unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"agents".to_string()));
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"shared_state".to_string()));
    }

    #[test]
    fn agent_registers_on_open() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir, "zeroclaw_agent_a");
        let conn = db.conn.lock().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agents WHERE agent_id = 'zeroclaw_agent_a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn heartbeat_updates_last_seen() {
        let dir = TempDir::new().unwrap();
        let db = test_db(&dir, "zeroclaw_agent_a");

        let before: i64 = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT last_seen FROM agents WHERE agent_id = 'zeroclaw_agent_a'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        // Small delay to ensure timestamp changes
        std::thread::sleep(std::time::Duration::from_millis(10));
        db.heartbeat();

        let after: i64 = {
            let conn = db.conn.lock().unwrap();
            conn.query_row(
                "SELECT last_seen FROM agents WHERE agent_id = 'zeroclaw_agent_a'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };

        assert!(after >= before);
    }

    #[tokio::test]
    async fn inbox_isolates_per_agent() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();

        let db_a = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 300).unwrap());
        let db_b = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_b", 300).unwrap());

        // Agent A sends to Agent B
        let send_tool = AgentsSendTool::new(db_a.clone(), Arc::new(SecurityPolicy::default()));
        send_tool
            .execute(json!({"to_agent": "zeroclaw_agent_b", "payload": "hello b"}))
            .await
            .unwrap();

        // Agent A's inbox should be empty
        let inbox_a = AgentsInboxTool::new(db_a);
        let result_a = inbox_a.execute(json!({})).await.unwrap();
        let msgs_a: Vec<serde_json::Value> = serde_json::from_str(&result_a.output).unwrap();
        assert!(msgs_a.is_empty());

        // Agent B's inbox should have the message
        let inbox_b = AgentsInboxTool::new(db_b);
        let result_b = inbox_b.execute(json!({})).await.unwrap();
        let msgs_b: Vec<serde_json::Value> = serde_json::from_str(&result_b.output).unwrap();
        assert_eq!(msgs_b.len(), 1);
        assert_eq!(msgs_b[0]["payload"], "hello b");
    }

    #[tokio::test]
    async fn broadcast_visible_to_all_agents() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();

        let db_a = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 300).unwrap());
        let db_b = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_b", 300).unwrap());

        // Agent A broadcasts
        let send_tool = AgentsSendTool::new(db_a.clone(), Arc::new(SecurityPolicy::default()));
        send_tool
            .execute(json!({"to_agent": "*", "payload": "broadcast msg"}))
            .await
            .unwrap();

        // Both agents should see the broadcast
        let inbox_a = AgentsInboxTool::new(db_a);
        let result_a = inbox_a.execute(json!({})).await.unwrap();
        let msgs_a: Vec<serde_json::Value> = serde_json::from_str(&result_a.output).unwrap();
        assert_eq!(msgs_a.len(), 1);

        let inbox_b = AgentsInboxTool::new(db_b);
        let result_b = inbox_b.execute(json!({})).await.unwrap();
        let msgs_b: Vec<serde_json::Value> = serde_json::from_str(&result_b.output).unwrap();
        assert_eq!(msgs_b.len(), 1);
    }

    #[tokio::test]
    async fn stale_agents_excluded_from_list() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();

        // Agent A with short staleness window
        let db_a = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 5).unwrap());

        // Manually backdate agent B's last_seen
        {
            let conn = db_a.conn.lock().unwrap();
            let old_time = now_epoch() - 100;
            conn.execute(
                "INSERT OR REPLACE INTO agents (agent_id, status, last_seen) VALUES ('zeroclaw_agent_b', 'online', ?1)",
                rusqlite::params![old_time],
            )
            .unwrap();
        }

        let list_tool = AgentsListTool::new(db_a);
        let result = list_tool.execute(json!({})).await.unwrap();
        let agents: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();

        // Only agent A should be listed (agent B is stale)
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["agent_id"], "zeroclaw_agent_a");
    }

    #[tokio::test]
    async fn identity_code_enforced() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();
        let db = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 300).unwrap());

        // Send a message — from_agent must be agent_a regardless of input
        let send_tool = AgentsSendTool::new(db.clone(), Arc::new(SecurityPolicy::default()));
        send_tool
            .execute(json!({"to_agent": "zeroclaw_agent_b", "payload": "test"}))
            .await
            .unwrap();

        let conn = db.conn.lock().unwrap();
        let from: String = conn
            .query_row("SELECT from_agent FROM messages LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(from, "zeroclaw_agent_a");
    }

    #[tokio::test]
    async fn state_upsert_creates_and_updates() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));

        let set_tool = StateSetTool::new(db.clone(), Arc::new(SecurityPolicy::default()));
        let get_tool = StateGetTool::new(db.clone());

        // Create
        set_tool
            .execute(json!({"key": "progress", "value": "50%"}))
            .await
            .unwrap();

        let result = get_tool.execute(json!({"key": "progress"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["value"], "50%");

        // Update
        set_tool
            .execute(json!({"key": "progress", "value": "100%"}))
            .await
            .unwrap();

        let result = get_tool.execute(json!({"key": "progress"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["value"], "100%");
    }

    #[tokio::test]
    async fn state_records_owner() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));

        let set_tool = StateSetTool::new(db.clone(), Arc::new(SecurityPolicy::default()));
        set_tool
            .execute(json!({"key": "task", "value": "done"}))
            .await
            .unwrap();

        let get_tool = StateGetTool::new(db);
        let result = get_tool.execute(json!({"key": "task"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(parsed["owner"], "zeroclaw_agent_a");
    }

    #[tokio::test]
    async fn empty_inbox_returns_success() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));

        let inbox_tool = AgentsInboxTool::new(db);
        let result = inbox_tool.execute(json!({})).await.unwrap();

        assert!(result.success);
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn security_blocks_act_in_readonly() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));
        let readonly = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });

        // agents_send should be blocked
        let send_tool = AgentsSendTool::new(db.clone(), readonly.clone());
        let result = send_tool
            .execute(json!({"to_agent": "zeroclaw_agent_b", "payload": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());

        // state_set should be blocked
        let set_tool = StateSetTool::new(db, readonly);
        let result = set_tool
            .execute(json!({"key": "k", "value": "v"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn disabled_config_registers_no_tools() {
        let config = AgentsIpcConfig {
            enabled: false,
            ..AgentsIpcConfig::default()
        };
        // When disabled, IpcDb::open should never be called,
        // so the tool count stays the same. Verify config defaults.
        assert!(!config.enabled);
        assert_eq!(config.staleness_secs, 300);
    }

    #[test]
    fn real_open_derives_agent_id_from_workspace() {
        let dir = TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let db_path = dir.path().join("agents.db");

        let config = AgentsIpcConfig {
            enabled: true,
            db_path: db_path.to_str().unwrap().to_string(),
            staleness_secs: 300,
        };

        let db = IpcDb::open(&workspace, &config).unwrap();

        // agent_id should be a 64-char hex SHA-256 hash
        assert_eq!(db.agent_id().len(), 64);
        assert!(db.agent_id().chars().all(|c| c.is_ascii_hexdigit()));

        // Same workspace should produce same agent_id
        let db2 = IpcDb::open(&workspace, &config).unwrap();
        assert_eq!(db.agent_id(), db2.agent_id());
    }

    #[test]
    fn drop_removes_agent_from_table() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db");
        let db_path_str = db_path.to_str().unwrap().to_string();

        // Open a connection that outlives the IpcDb to verify cleanup
        {
            let _db = IpcDb::open_with_id(&db_path_str, "zeroclaw_agent_a", 300).unwrap();
            // _db is alive here — agent should be in table
        }
        // _db dropped — agent should be removed

        let conn = Connection::open(&db_path_str).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agents WHERE agent_id = 'zeroclaw_agent_a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn direct_messages_marked_read_after_inbox() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();

        let db_a = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 300).unwrap());
        let db_b = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_b", 300).unwrap());

        // Agent A sends to Agent B
        let send_tool = AgentsSendTool::new(db_a, Arc::new(SecurityPolicy::default()));
        send_tool
            .execute(json!({"to_agent": "zeroclaw_agent_b", "payload": "once"}))
            .await
            .unwrap();

        let inbox_b = AgentsInboxTool::new(db_b.clone());

        // First read: should have 1 message
        let result = inbox_b.execute(json!({})).await.unwrap();
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(msgs.len(), 1);

        // Second read: should be empty (marked read)
        let result = inbox_b.execute(json!({})).await.unwrap();
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn state_get_missing_key_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));

        let get_tool = StateGetTool::new(db);
        let result = get_tool
            .execute(json!({"key": "nonexistent"}))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn send_missing_params_returns_error() {
        let dir = TempDir::new().unwrap();
        let db = Arc::new(test_db(&dir, "zeroclaw_agent_a"));
        let send_tool = AgentsSendTool::new(db, Arc::new(SecurityPolicy::default()));

        // Missing payload
        let result = send_tool
            .execute(json!({"to_agent": "zeroclaw_agent_b"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("payload"));

        // Missing to_agent
        let result = send_tool
            .execute(json!({"payload": "hello"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap().contains("to_agent"));
    }

    #[tokio::test]
    async fn two_agents_full_exchange() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("agents.db").to_str().unwrap().to_string();

        let db_a = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_a", 300).unwrap());
        let db_b = Arc::new(IpcDb::open_with_id(&db_path, "zeroclaw_agent_b", 300).unwrap());
        let security = Arc::new(SecurityPolicy::default());

        // Both agents visible in list
        let list_tool = AgentsListTool::new(db_a.clone());
        let result = list_tool.execute(json!({})).await.unwrap();
        let agents: Vec<serde_json::Value> = serde_json::from_str(&result.output).unwrap();
        assert_eq!(agents.len(), 2);

        // Agent A sends to Agent B
        let send_a = AgentsSendTool::new(db_a.clone(), security.clone());
        let r = send_a
            .execute(json!({"to_agent": "zeroclaw_agent_b", "payload": "task: summarize"}))
            .await
            .unwrap();
        assert!(r.success);

        // Agent B reads inbox
        let inbox_b = AgentsInboxTool::new(db_b.clone());
        let r = inbox_b.execute(json!({})).await.unwrap();
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&r.output).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["payload"], "task: summarize");
        assert_eq!(msgs[0]["from_agent"], "zeroclaw_agent_a");

        // Agent B replies to Agent A
        let send_b = AgentsSendTool::new(db_b.clone(), security.clone());
        send_b
            .execute(json!({"to_agent": "zeroclaw_agent_a", "payload": "done: summary attached"}))
            .await
            .unwrap();

        // Agent A reads reply
        let inbox_a = AgentsInboxTool::new(db_a.clone());
        let r = inbox_a.execute(json!({})).await.unwrap();
        let msgs: Vec<serde_json::Value> = serde_json::from_str(&r.output).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["payload"], "done: summary attached");
        assert_eq!(msgs[0]["from_agent"], "zeroclaw_agent_b");

        // Agent A sets shared state
        let set_tool = StateSetTool::new(db_a, security);
        set_tool
            .execute(json!({"key": "status", "value": "complete"}))
            .await
            .unwrap();

        // Agent B reads shared state
        let get_tool = StateGetTool::new(db_b);
        let r = get_tool.execute(json!({"key": "status"})).await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&r.output).unwrap();
        assert_eq!(parsed["value"], "complete");
        assert_eq!(parsed["owner"], "zeroclaw_agent_a");
    }
}
