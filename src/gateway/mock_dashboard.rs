//! Persistent mock dashboard backend for end-to-end UI testing.
//!
//! Enabled per-request via `X-ZeroClaw-Mock: 1`.

use anyhow::{Context, Result};
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use chrono::{Duration, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const MOCK_HEADER: &str = "x-zeroclaw-mock";
const STATE_KEY: &str = "dashboard_state";
const MASKED_SECRET: &str = "***MASKED***";

static MOCK_STORE: OnceLock<Result<DashboardMockStore, String>> = OnceLock::new();

#[derive(Debug)]
struct DashboardMockStore {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockCronJob {
    id: String,
    name: Option<String>,
    command: String,
    next_run: String,
    last_run: Option<String>,
    last_status: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockMemoryEntry {
    id: String,
    key: String,
    content: String,
    category: String,
    timestamp: String,
    session_id: Option<String>,
    score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockPairedDevice {
    id: String,
    token_fingerprint: String,
    created_at: Option<String>,
    last_seen_at: Option<String>,
    paired_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MockDashboardState {
    status: Value,
    health: Value,
    cost: Value,
    tools: Vec<Value>,
    cron_jobs: Vec<MockCronJob>,
    integrations: Vec<Value>,
    integration_settings: Value,
    memory_entries: Vec<MockMemoryEntry>,
    paired_devices: Vec<MockPairedDevice>,
    diagnostics: Vec<Value>,
    cli_tools: Vec<Value>,
    config_toml: String,
    revision_counter: u64,
    id_counter: u64,
}

impl MockDashboardState {
    fn default_config_toml() -> String {
        r#"[gateway]
host = "127.0.0.1"
port = 42617
require_pairing = true

[agent]
max_tool_iterations = 24
"#
        .to_string()
    }

    fn default_state() -> Self {
        let now = Utc::now();
        let now_iso = now.to_rfc3339();
        let one_hour_ahead = (now + Duration::hours(1)).to_rfc3339();
        let four_hours_ago = (now - Duration::hours(4)).to_rfc3339();
        let two_hours_ago = (now - Duration::hours(2)).to_rfc3339();
        let eight_hours_ago = (now - Duration::hours(8)).to_rfc3339();
        let fourteen_days_ago = (now - Duration::days(14)).to_rfc3339();
        let three_days_ago = (now - Duration::days(3)).to_rfc3339();
        let forty_minutes_ago = (now - Duration::minutes(40)).to_rfc3339();
        let six_hours_ago = (now - Duration::hours(6)).to_rfc3339();

        let health = json!({
            "pid": 4242,
            "updated_at": now_iso,
            "uptime_seconds": 68420,
            "components": {
                "gateway": {
                    "status": "ok",
                    "updated_at": now_iso,
                    "last_ok": now_iso,
                    "last_error": Value::Null,
                    "restart_count": 0,
                },
                "provider": {
                    "status": "ok",
                    "updated_at": now_iso,
                    "last_ok": now_iso,
                    "last_error": Value::Null,
                    "restart_count": 0,
                },
                "memory": {
                    "status": "degraded",
                    "updated_at": now_iso,
                    "last_ok": now_iso,
                    "last_error": now_iso,
                    "restart_count": 1,
                },
                "channels": {
                    "status": "ok",
                    "updated_at": now_iso,
                    "last_ok": now_iso,
                    "last_error": Value::Null,
                    "restart_count": 0,
                }
            }
        });

        Self {
            status: json!({
                "provider": "openai",
                "model": "gpt-5.2",
                "temperature": 0.4,
                "uptime_seconds": 68420,
                "gateway_port": 42617,
                "locale": "en-US",
                "memory_backend": "sqlite",
                "paired": true,
                "channels": {
                    "telegram": true,
                    "discord": false,
                    "whatsapp": true,
                    "github": true,
                },
                "health": health,
            }),
            health,
            cost: json!({
                "session_cost_usd": 0.0842,
                "daily_cost_usd": 1.3026,
                "monthly_cost_usd": 14.9875,
                "total_tokens": 182_342,
                "request_count": 426,
                "by_model": {
                    "gpt-5.2": {
                        "model": "gpt-5.2",
                        "cost_usd": 11.4635,
                        "total_tokens": 141_332,
                        "request_count": 292,
                    },
                    "claude-sonnet-4-5": {
                        "model": "claude-sonnet-4-5",
                        "cost_usd": 3.5240,
                        "total_tokens": 41010,
                        "request_count": 134,
                    }
                }
            }),
            tools: vec![
                json!({
                    "name": "shell",
                    "description": "Run shell commands inside the workspace",
                    "parameters": {
                        "type": "object",
                        "properties": { "command": { "type": "string" } },
                        "required": ["command"],
                    }
                }),
                json!({
                    "name": "file_read",
                    "description": "Read files from disk",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"],
                    }
                }),
                json!({
                    "name": "web_fetch",
                    "description": "Fetch and parse HTTP resources",
                    "parameters": {
                        "type": "object",
                        "properties": { "url": { "type": "string" } },
                        "required": ["url"],
                    }
                }),
            ],
            cron_jobs: vec![
                MockCronJob {
                    id: "mock-cron-1".to_string(),
                    name: Some("Daily sync".to_string()),
                    command: "zeroclaw sync --channels".to_string(),
                    next_run: one_hour_ahead,
                    last_run: Some(four_hours_ago),
                    last_status: Some("ok".to_string()),
                    enabled: true,
                },
                MockCronJob {
                    id: "mock-cron-2".to_string(),
                    name: Some("Budget audit".to_string()),
                    command: "zeroclaw cost audit".to_string(),
                    next_run: (now + Duration::hours(12)).to_rfc3339(),
                    last_run: None,
                    last_status: None,
                    enabled: false,
                },
            ],
            integrations: vec![
                json!({
                    "name": "Slack",
                    "description": "Slack bot messaging and thread orchestration",
                    "category": "Channels",
                    "status": "Active",
                }),
                json!({
                    "name": "GitHub",
                    "description": "PR and issue automation",
                    "category": "Automation",
                    "status": "Available",
                }),
                json!({
                    "name": "Linear",
                    "description": "Issue workflow sync",
                    "category": "Productivity",
                    "status": "ComingSoon",
                }),
            ],
            integration_settings: json!({
                "revision": "mock-revision-17",
                "active_default_provider_integration_id": "openai",
                "integrations": [
                    {
                        "id": "openai",
                        "name": "OpenAI",
                        "description": "Primary LLM provider",
                        "category": "Providers",
                        "status": "Active",
                        "configured": true,
                        "activates_default_provider": true,
                        "fields": [
                            {
                                "key": "api_key",
                                "label": "API Key",
                                "required": true,
                                "has_value": true,
                                "input_type": "secret",
                                "options": [],
                                "masked_value": "sk-****abcd",
                            },
                            {
                                "key": "default_model",
                                "label": "Default Model",
                                "required": false,
                                "has_value": true,
                                "input_type": "select",
                                "options": ["gpt-5.2", "gpt-5.2-codex", "gpt-4o"],
                                "current_value": "gpt-5.2",
                            }
                        ]
                    },
                    {
                        "id": "slack",
                        "name": "Slack",
                        "description": "Workspace notifications and bot relay",
                        "category": "Channels",
                        "status": "Available",
                        "configured": false,
                        "activates_default_provider": false,
                        "fields": [
                            {
                                "key": "bot_token",
                                "label": "Bot Token",
                                "required": true,
                                "has_value": false,
                                "input_type": "secret",
                                "options": [],
                            }
                        ]
                    }
                ]
            }),
            memory_entries: vec![
                MockMemoryEntry {
                    id: "mem-1".to_string(),
                    key: "ops.runbook.gateway".to_string(),
                    content:
                        "Restart gateway with `zeroclaw gateway --open-dashboard` after updates."
                            .to_string(),
                    category: "operations".to_string(),
                    timestamp: two_hours_ago,
                    session_id: Some("sess_42".to_string()),
                    score: Some(0.92),
                },
                MockMemoryEntry {
                    id: "mem-2".to_string(),
                    key: "cost.budget.daily".to_string(),
                    content: "Daily soft budget threshold is $2.50 for development environments."
                        .to_string(),
                    category: "cost".to_string(),
                    timestamp: eight_hours_ago,
                    session_id: None,
                    score: Some(0.88),
                },
            ],
            paired_devices: vec![
                MockPairedDevice {
                    id: "device-1".to_string(),
                    token_fingerprint: "zc_3f2a...19d0".to_string(),
                    created_at: Some(fourteen_days_ago),
                    last_seen_at: Some(forty_minutes_ago),
                    paired_by: Some("localhost".to_string()),
                },
                MockPairedDevice {
                    id: "device-2".to_string(),
                    token_fingerprint: "zc_09ac...7e4f".to_string(),
                    created_at: Some(three_days_ago),
                    last_seen_at: Some(six_hours_ago),
                    paired_by: Some("vpn".to_string()),
                },
            ],
            diagnostics: vec![
                json!({"severity": "ok", "category": "runtime", "message": "Gateway listeners are healthy."}),
                json!({"severity": "warn", "category": "cost", "message": "Daily spend crossed 50% threshold."}),
                json!({"severity": "ok", "category": "security", "message": "Pairing mode is enabled."}),
            ],
            cli_tools: vec![
                json!({"name": "git", "path": "/usr/bin/git", "version": "2.46.1", "category": "vcs"}),
                json!({"name": "cargo", "path": "/Users/mock/.cargo/bin/cargo", "version": "1.87.0", "category": "build"}),
                json!({"name": "npm", "path": "/opt/homebrew/bin/npm", "version": "11.3.0", "category": "package-manager"}),
            ],
            config_toml: Self::default_config_toml(),
            revision_counter: 17,
            id_counter: 2,
        }
    }
}

impl DashboardMockStore {
    fn open_default() -> Result<Self> {
        let path = std::env::var("ZEROCLAW_DASHBOARD_MOCK_DB")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_db_path());
        Self::open(&path)
    }

    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create dashboard mock DB dir at {}",
                    parent.display()
                )
            })?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("failed to open dashboard mock DB at {}", path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS dashboard_mock_state (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );",
        )
        .context("failed to initialize dashboard mock schema")?;

        let existing: Option<String> = conn
            .query_row(
                "SELECT value FROM dashboard_mock_state WHERE key = ?1",
                params![STATE_KEY],
                |row| row.get(0),
            )
            .optional()
            .context("failed to read dashboard mock state")?;

        if existing.is_none() {
            let default_state = MockDashboardState::default_state();
            let serialized = serde_json::to_string(&default_state)
                .context("failed to serialize default dashboard mock state")?;
            conn.execute(
                "INSERT INTO dashboard_mock_state (key, value) VALUES (?1, ?2)",
                params![STATE_KEY, serialized],
            )
            .context("failed to persist default dashboard mock state")?;
        }

        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn read_state(&self) -> Result<MockDashboardState> {
        let conn = self.conn.lock();
        Self::read_state_locked(&conn)
    }

    fn update_state<R, F>(&self, update: F) -> Result<R>
    where
        F: FnOnce(&mut MockDashboardState) -> Result<R>,
    {
        let conn = self.conn.lock();
        let mut state = Self::read_state_locked(&conn)?;
        let result = update(&mut state)?;
        Self::write_state_locked(&conn, &state)?;
        Ok(result)
    }

    fn read_state_locked(conn: &Connection) -> Result<MockDashboardState> {
        let raw: String = conn
            .query_row(
                "SELECT value FROM dashboard_mock_state WHERE key = ?1",
                params![STATE_KEY],
                |row| row.get(0),
            )
            .context("dashboard mock state row is missing")?;

        serde_json::from_str(&raw).context("failed to deserialize dashboard mock state")
    }

    fn write_state_locked(conn: &Connection, state: &MockDashboardState) -> Result<()> {
        let serialized =
            serde_json::to_string(state).context("failed to serialize dashboard mock state")?;
        conn.execute(
            "UPDATE dashboard_mock_state SET value = ?1 WHERE key = ?2",
            params![serialized, STATE_KEY],
        )
        .context("failed to persist dashboard mock state update")?;
        Ok(())
    }
}

fn default_db_path() -> PathBuf {
    if let Some(project_dirs) = directories::ProjectDirs::from("", "", "zeroclaw") {
        return project_dirs.config_dir().join("dashboard_mock.db");
    }

    std::env::temp_dir().join("zeroclaw-dashboard-mock.db")
}

fn parse_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn mock_store() -> Result<&'static DashboardMockStore> {
    match MOCK_STORE.get_or_init(|| DashboardMockStore::open_default().map_err(|e| e.to_string())) {
        Ok(store) => Ok(store),
        Err(err) => Err(anyhow::anyhow!(err.clone())),
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    let message = message.into();
    (status, Json(json!({ "error": message }))).into_response()
}

fn json_ok(value: Value) -> Response {
    Json(value).into_response()
}

pub fn is_enabled(headers: &HeaderMap) -> bool {
    headers
        .get(MOCK_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(parse_truthy)
}

pub fn status() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(state.status),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn health() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "health": state.health })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn cost() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "cost": state.cost })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn tools() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "tools": state.tools })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn cron_list() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "jobs": state.cron_jobs })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn cron_add(
    name: Option<String>,
    schedule: String,
    command: String,
    enabled: Option<bool>,
) -> Response {
    if schedule.trim().is_empty() || command.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "command and schedule are required");
    }

    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        state.id_counter = state.id_counter.saturating_add(1);
        let job = MockCronJob {
            id: format!("mock-cron-{}", state.id_counter),
            name,
            command,
            next_run: (Utc::now() + Duration::minutes(1)).to_rfc3339(),
            last_run: None,
            last_status: None,
            enabled: enabled.unwrap_or(true),
        };
        state.cron_jobs.push(job.clone());
        Ok(job)
    }) {
        Ok(job) => json_ok(json!({ "status": "created", "job": job })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn cron_delete(id: &str) -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        let before = state.cron_jobs.len();
        state.cron_jobs.retain(|job| job.id != id);
        Ok(before != state.cron_jobs.len())
    }) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => json_error(StatusCode::NOT_FOUND, "Cron job not found"),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn integrations() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "integrations": state.integrations })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn integrations_settings() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(state.integration_settings),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn integrations_credentials_put(id: &str, body: &Value) -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        let field_updates = body
            .get("fields")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let settings_obj = state
            .integration_settings
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("integration settings payload is invalid"))?;
        let integrations = settings_obj
            .get_mut("integrations")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow::anyhow!("integration settings list is missing"))?;

        let mut found = false;
        let mut activates_default_provider = false;

        for integration in integrations.iter_mut() {
            let Some(obj) = integration.as_object_mut() else {
                continue;
            };
            if obj.get("id").and_then(Value::as_str) != Some(id) {
                continue;
            }

            found = true;
            activates_default_provider = obj
                .get("activates_default_provider")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let Some(fields) = obj.get_mut("fields").and_then(Value::as_array_mut) else {
                continue;
            };

            for field in fields.iter_mut() {
                let Some(field_obj) = field.as_object_mut() else {
                    continue;
                };
                let Some(field_key) = field_obj.get("key").and_then(Value::as_str) else {
                    continue;
                };
                let Some(raw_value) = field_updates.get(field_key).and_then(Value::as_str) else {
                    continue;
                };
                let next_value = raw_value.trim();

                if next_value.is_empty() {
                    field_obj.insert("has_value".to_string(), Value::Bool(false));
                    field_obj.remove("masked_value");
                    field_obj.remove("current_value");
                    continue;
                }

                field_obj.insert("has_value".to_string(), Value::Bool(true));
                if field_obj
                    .get("input_type")
                    .and_then(Value::as_str)
                    .is_some_and(|v| v == "secret")
                {
                    field_obj.insert(
                        "masked_value".to_string(),
                        Value::String(MASKED_SECRET.to_string()),
                    );
                    field_obj.remove("current_value");
                } else {
                    field_obj.insert(
                        "current_value".to_string(),
                        Value::String(next_value.to_string()),
                    );
                }
            }
        }

        if !found {
            anyhow::bail!("integration not found");
        }

        if activates_default_provider {
            settings_obj.insert(
                "active_default_provider_integration_id".to_string(),
                Value::String(id.to_string()),
            );
        }

        state.revision_counter = state.revision_counter.saturating_add(1);
        let revision = format!("mock-revision-{}", state.revision_counter);
        settings_obj.insert("revision".to_string(), Value::String(revision.clone()));
        Ok(revision)
    }) {
        Ok(revision) => json_ok(json!({ "status": "ok", "revision": revision })),
        Err(err) => {
            if err.to_string().contains("integration not found") {
                return json_error(StatusCode::NOT_FOUND, err.to_string());
            }
            json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
        }
    }
}

pub fn doctor() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => {
            let ok_count = state
                .diagnostics
                .iter()
                .filter(|entry| entry.get("severity").and_then(Value::as_str) == Some("ok"))
                .count();
            let warn_count = state
                .diagnostics
                .iter()
                .filter(|entry| entry.get("severity").and_then(Value::as_str) == Some("warn"))
                .count();
            let error_count = state
                .diagnostics
                .iter()
                .filter(|entry| entry.get("severity").and_then(Value::as_str) == Some("error"))
                .count();

            json_ok(json!({
                "results": state.diagnostics,
                "summary": {
                    "ok": ok_count,
                    "warnings": warn_count,
                    "errors": error_count,
                }
            }))
        }
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn memory_list(query: Option<String>, category: Option<String>) -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => {
            let query_lower = query.map(|q| q.to_lowercase());
            let category_lower = category.map(|c| c.to_lowercase());

            let entries: Vec<MockMemoryEntry> = state
                .memory_entries
                .into_iter()
                .filter(|entry| {
                    let category_match = category_lower
                        .as_ref()
                        .map(|needle| entry.category.to_lowercase() == *needle)
                        .unwrap_or(true);
                    if !category_match {
                        return false;
                    }

                    query_lower
                        .as_ref()
                        .map(|needle| {
                            entry.key.to_lowercase().contains(needle)
                                || entry.content.to_lowercase().contains(needle)
                                || entry.category.to_lowercase().contains(needle)
                        })
                        .unwrap_or(true)
                })
                .collect();

            json_ok(json!({ "entries": entries }))
        }
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn memory_store(key: String, content: String, category: Option<String>) -> Response {
    if key.trim().is_empty() || content.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "key and content are required");
    }

    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        let timestamp = Utc::now().to_rfc3339();
        if let Some(existing) = state
            .memory_entries
            .iter_mut()
            .find(|entry| entry.key == key)
        {
            existing.content = content;
            existing.category = category.unwrap_or_else(|| existing.category.clone());
            existing.timestamp = timestamp;
            return Ok(());
        }

        state.id_counter = state.id_counter.saturating_add(1);
        state.memory_entries.push(MockMemoryEntry {
            id: format!("mem-{}", state.id_counter),
            key,
            content,
            category: category.unwrap_or_else(|| "core".to_string()),
            timestamp,
            session_id: Some(format!("sess_{}", state.id_counter)),
            score: Some(0.75),
        });
        Ok(())
    }) {
        Ok(()) => json_ok(json!({ "status": "stored" })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn memory_delete(key: &str) -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        let before = state.memory_entries.len();
        state.memory_entries.retain(|entry| entry.key != key);
        Ok(before != state.memory_entries.len())
    }) {
        Ok(deleted) => json_ok(json!({ "status": "ok", "deleted": deleted })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn pairing_devices() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "devices": state.paired_devices })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn pairing_device_revoke(id: &str) -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        let before = state.paired_devices.len();
        state.paired_devices.retain(|entry| entry.id != id);
        Ok(before != state.paired_devices.len())
    }) {
        Ok(true) => json_ok(json!({ "status": "ok", "revoked": true, "id": id })),
        Ok(false) => json_error(StatusCode::NOT_FOUND, "Paired device not found"),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn config_get() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({
            "format": "toml",
            "content": state.config_toml,
        })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn config_put(body: String) -> Response {
    if body.trim().is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "config body cannot be empty");
    }

    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.update_state(|state| {
        state.config_toml = body;
        Ok(())
    }) {
        Ok(()) => json_ok(json!({ "status": "saved" })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

pub fn cli_tools() -> Response {
    let store = match mock_store() {
        Ok(store) => store,
        Err(err) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    };

    match store.read_state() {
        Ok(state) => json_ok(json!({ "cli_tools": state.cli_tools })),
        Err(err) => json_error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_contains_required_dashboard_fields() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("dashboard_mock.db");
        let store = DashboardMockStore::open(&path).expect("open test dashboard mock store");
        let state = store.read_state().expect("read state");

        assert!(state.status.get("provider").is_some());
        assert!(state.status.get("model").is_some());
        assert!(state.status.get("channels").is_some());
        assert!(state.cost.get("session_cost_usd").is_some());
        assert!(state.cost.get("by_model").is_some());
        assert!(!state.tools.is_empty());
        assert!(!state.integrations.is_empty());
        assert!(!state.memory_entries.is_empty());
        assert!(!state.paired_devices.is_empty());
        assert!(!state.config_toml.trim().is_empty());
    }

    #[test]
    fn state_updates_persist_in_sqlite() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("dashboard_mock.db");

        let store = DashboardMockStore::open(&path).expect("open store");
        store
            .update_state(|state| {
                state.cron_jobs.push(MockCronJob {
                    id: "mock-cron-custom".to_string(),
                    name: Some("Custom".to_string()),
                    command: "echo hello".to_string(),
                    next_run: Utc::now().to_rfc3339(),
                    last_run: None,
                    last_status: None,
                    enabled: true,
                });
                state.config_toml = "[gateway]\nport = 9999\n".to_string();
                Ok(())
            })
            .expect("update state");

        let reopened = DashboardMockStore::open(&path).expect("reopen store");
        let state = reopened.read_state().expect("read reopened state");

        assert!(state
            .cron_jobs
            .iter()
            .any(|job| job.id == "mock-cron-custom"));
        assert_eq!(state.config_toml, "[gateway]\nport = 9999\n");
    }

    #[test]
    fn memory_search_filters_entries() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("dashboard_mock.db");
        let store = DashboardMockStore::open(&path).expect("open test dashboard mock store");
        let state = store.read_state().expect("read state");

        let matches: Vec<_> = state
            .memory_entries
            .into_iter()
            .filter(|entry| entry.content.to_lowercase().contains("gateway"))
            .collect();

        assert!(!matches.is_empty());
        assert!(matches
            .iter()
            .all(|entry| entry.content.to_lowercase().contains("gateway")));
    }
}
