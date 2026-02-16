use crate::aria::db::AriaDb;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::Value;
use std::path::Path;

const DASHBOARD_SCHEMA_VERSION: i64 = 12;

pub fn ensure_schema(db: &AriaDb) -> Result<()> {
    db.with_conn(|conn| {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS meta (
              key TEXT PRIMARY KEY,
              value TEXT
            );

            CREATE TABLE IF NOT EXISTS chats (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              title TEXT,
              preview TEXT,
              session_id TEXT,
              message_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chats_tenant ON chats(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_chats_updated ON chats(tenant_id, updated_at DESC);

            CREATE TABLE IF NOT EXISTS messages (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              chat_id TEXT NOT NULL,
              role TEXT NOT NULL,
              content TEXT,
              run_json TEXT,
              mentions_json TEXT,
              command_json TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id);
            CREATE INDEX IF NOT EXISTS idx_messages_tenant ON messages(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(chat_id, created_at ASC);

            CREATE TABLE IF NOT EXISTS conversations (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              title TEXT,
              session_id TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_tenant ON conversations(tenant_id);

            CREATE TABLE IF NOT EXISTS approvals (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              command TEXT NOT NULL,
              metadata_json TEXT,
              countdown INTEGER DEFAULT 30,
              status TEXT DEFAULT 'pending',
              created_at INTEGER NOT NULL,
              expires_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_approvals_tenant ON approvals(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(tenant_id, status);

            CREATE TABLE IF NOT EXISTS sessions (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              last_activity INTEGER,
              run_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              user_id TEXT,
              source TEXT,
              platform TEXT,
              device_name TEXT,
              location TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_tenant ON sessions(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(tenant_id, status);

            CREATE TABLE IF NOT EXISTS events (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              type TEXT NOT NULL,
              title TEXT NOT NULL,
              description TEXT,
              source TEXT,
              metadata_json TEXT,
              timestamp INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_events_tenant ON events(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_events_type ON events(tenant_id, type);
            CREATE INDEX IF NOT EXISTS idx_events_time ON events(tenant_id, timestamp DESC);

            CREATE TABLE IF NOT EXISTS inbox_items (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              source_type TEXT NOT NULL,
              source_id TEXT,
              run_id TEXT,
              chat_id TEXT,
              title TEXT NOT NULL,
              preview TEXT,
              body TEXT,
              metadata_json TEXT,
              status TEXT NOT NULL DEFAULT 'unread',
              created_at INTEGER NOT NULL,
              read_at INTEGER,
              archived_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_inbox_tenant_status ON inbox_items(tenant_id, status, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_inbox_tenant_source ON inbox_items(tenant_id, source_type, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_inbox_run ON inbox_items(run_id);

            CREATE TABLE IF NOT EXISTS nodes (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              type TEXT NOT NULL,
              status TEXT DEFAULT 'online',
              load INTEGER DEFAULT 0,
              memory_usage INTEGER DEFAULT 0,
              cpu_usage INTEGER DEFAULT 0,
              last_heartbeat INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_nodes_tenant ON nodes(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_status ON nodes(tenant_id, status);

            CREATE TABLE IF NOT EXISTS channels (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              type TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              requests_per_min INTEGER DEFAULT 0,
              endpoint TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_channels_tenant ON channels(tenant_id);

            CREATE TABLE IF NOT EXISTS cron_jobs (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              schedule TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              last_run INTEGER,
              next_run INTEGER,
              handler TEXT,
              description TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_dash_cron_tenant ON cron_jobs(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_dash_cron_status ON cron_jobs(tenant_id, status);

            CREATE TABLE IF NOT EXISTS skills (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              description TEXT,
              enabled INTEGER DEFAULT 1,
              call_count INTEGER DEFAULT 0,
              version TEXT,
              category TEXT,
              permissions_json TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_skills_tenant ON skills(tenant_id);

            CREATE TABLE IF NOT EXISTS system_config (
              tenant_id TEXT NOT NULL,
              key TEXT NOT NULL,
              value_json TEXT,
              updated_at INTEGER NOT NULL,
              PRIMARY KEY (tenant_id, key)
            );

            CREATE TABLE IF NOT EXISTS logs (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              timestamp INTEGER NOT NULL,
              level TEXT NOT NULL,
              source TEXT,
              message TEXT,
              metadata_json TEXT,
              trace_id TEXT,
              span_id TEXT,
              session_id TEXT,
              agent_id TEXT,
              duration INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_logs_tenant ON logs(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(tenant_id, level);
            CREATE INDEX IF NOT EXISTS idx_logs_time ON logs(tenant_id, timestamp DESC);
            CREATE INDEX IF NOT EXISTS idx_logs_source ON logs(tenant_id, source);
            CREATE INDEX IF NOT EXISTS idx_logs_trace ON logs(trace_id);

            CREATE TABLE IF NOT EXISTS tool_calls (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              session_id TEXT,
              run_id TEXT,
              agent_id TEXT,
              tool_name TEXT NOT NULL,
              status TEXT NOT NULL,
              args_json TEXT,
              result_json TEXT,
              error TEXT,
              duration_ms INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tool_calls_tenant ON tool_calls(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(tenant_id, session_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_run ON tool_calls(tenant_id, run_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_status ON tool_calls(tenant_id, status);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_time ON tool_calls(tenant_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS api_keys (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              key_hash TEXT NOT NULL,
              key_preview TEXT NOT NULL,
              status TEXT DEFAULT 'active',
              scopes_json TEXT,
              rate_limit INTEGER DEFAULT 1000,
              request_count INTEGER DEFAULT 0,
              created_at INTEGER NOT NULL,
              last_used_at INTEGER,
              expires_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON api_keys(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);
            CREATE INDEX IF NOT EXISTS idx_api_keys_status ON api_keys(tenant_id, status);

            CREATE TABLE IF NOT EXISTS feed_items (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              feed_id TEXT NOT NULL,
              card_type TEXT NOT NULL,
              title TEXT NOT NULL,
              body TEXT,
              source TEXT,
              url TEXT,
              metadata_json TEXT NOT NULL,
              run_id TEXT,
              created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_feed_items_tenant ON feed_items(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_feed_items_feed ON feed_items(tenant_id, feed_id);
            CREATE INDEX IF NOT EXISTS idx_feed_items_time ON feed_items(tenant_id, feed_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS feed_files (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              name TEXT NOT NULL,
              extension TEXT NOT NULL,
              content_type TEXT NOT NULL,
              size INTEGER NOT NULL,
              blob_key TEXT NOT NULL,
              source_id TEXT,
              description TEXT,
              tags_json TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_feed_files_tenant ON feed_files(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_feed_files_source ON feed_files(tenant_id, source_id);
            CREATE INDEX IF NOT EXISTS idx_feed_files_time ON feed_files(tenant_id, created_at DESC);

            CREATE TABLE IF NOT EXISTS tenant_config (
              tenant_id TEXT PRIMARY KEY,
              system_prompt_custom TEXT,
              system_prompt_version INTEGER DEFAULT 1,
              runtime_config_json TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS magic_numbers (
              id TEXT PRIMARY KEY,
              tenant_id TEXT NOT NULL,
              user_id TEXT NOT NULL,
              email TEXT,
              key_hash TEXT NOT NULL UNIQUE,
              key_preview TEXT NOT NULL,
              name TEXT NOT NULL DEFAULT 'default',
              status TEXT NOT NULL DEFAULT 'active',
              created_at INTEGER NOT NULL,
              last_used_at INTEGER,
              revoked_at INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_tenant ON magic_numbers(tenant_id);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_hash ON magic_numbers(key_hash);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_status ON magic_numbers(tenant_id, status);
            CREATE INDEX IF NOT EXISTS idx_magic_numbers_user ON magic_numbers(tenant_id, user_id);
            "#,
        )?;

        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('dashboard_schema_version', ?1)",
            params![DASHBOARD_SCHEMA_VERSION.to_string()],
        )?;

        // Backward-compatible column adds for existing databases.
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN source TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN platform TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN device_name TEXT", []);
        let _ = conn.execute("ALTER TABLE sessions ADD COLUMN location TEXT", []);

        Ok(())
    })
}

pub fn import_from_cloud_db(db: &AriaDb, source_path: &Path) -> Result<usize> {
    if !source_path.exists() {
        return Ok(0);
    }

    let tables = [
        "chats",
        "messages",
        "conversations",
        "approvals",
        "sessions",
        "events",
        "inbox_items",
        "nodes",
        "channels",
        "cron_jobs",
        "skills",
        "system_config",
        "logs",
        "tool_calls",
        "api_keys",
        "feed_items",
        "feed_files",
        "tenant_config",
        "magic_numbers",
    ];

    db.with_conn(|conn| {
        conn.execute(
            "ATTACH DATABASE ?1 AS cloud",
            params![source_path.to_string_lossy()],
        )
        .context("Failed to attach cloud dashboard DB")?;

        let mut imported = 0usize;
        for table in tables {
            let exists: Option<String> = conn
                .query_row(
                    "SELECT name FROM cloud.sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .ok();
            if exists.is_none() {
                continue;
            }
            let sql = format!("INSERT OR REPLACE INTO {table} SELECT * FROM cloud.{table}");
            conn.execute_batch(&sql)
                .with_context(|| format!("Failed importing table {table}"))?;
            imported += 1;
        }

        conn.execute_batch("DETACH DATABASE cloud")
            .context("Failed to detach cloud DB")?;
        Ok(imported)
    })
}

fn parse_event_millis(timestamp_rfc3339: &str) -> i64 {
    DateTime::parse_from_rfc3339(timestamp_rfc3339)
        .map(|dt| dt.with_timezone(&Utc).timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis())
}

fn source_for_event(event_type: &str) -> &'static str {
    if event_type.starts_with("cron.") {
        "cron"
    } else if event_type.starts_with("feed.") {
        "feed"
    } else if event_type.starts_with("subagent.") {
        "subagent"
    } else if event_type.starts_with("task.") || event_type.starts_with("heartbeat.") {
        "task"
    } else {
        "agent"
    }
}

fn title_for_event(event_type: &str, data: &Value) -> String {
    match event_type {
        "cron.completed" => format!(
            "Cron completed: {}",
            data["jobId"].as_str().unwrap_or("unknown")
        ),
        "cron.failed" => format!(
            "Cron failed: {}",
            data["jobId"].as_str().unwrap_or("unknown")
        ),
        "feed.run.completed" => format!(
            "Feed run completed: {}",
            data["feedName"]
                .as_str()
                .or_else(|| data["feedId"].as_str())
                .unwrap_or("unknown")
        ),
        "feed.run.failed" => format!(
            "Feed run failed: {}",
            data["feedName"]
                .as_str()
                .or_else(|| data["feedId"].as_str())
                .unwrap_or("unknown")
        ),
        "task.completed" => format!(
            "Task completed: {}",
            data["name"].as_str().unwrap_or("task")
        ),
        "task.failed" => format!("Task failed: {}", data["name"].as_str().unwrap_or("task")),
        "heartbeat.task.completed" => "Heartbeat task completed".to_string(),
        "heartbeat.task.failed" => format!(
            "Heartbeat task failed: {}",
            data["task"].as_str().unwrap_or("task")
        ),
        "heartbeat.run.completed" => format!(
            "Heartbeat run completed ({}/{})",
            data["successCount"].as_i64().unwrap_or(0),
            data["taskCount"].as_i64().unwrap_or(0)
        ),
        "heartbeat.run.failed" => format!(
            "Heartbeat run failed ({}/{})",
            data["failedCount"].as_i64().unwrap_or(0),
            data["taskCount"].as_i64().unwrap_or(0)
        ),
        "subagent.started" => format!(
            "Subagent started: {}",
            data["taskLabel"].as_str().unwrap_or("task")
        ),
        "subagent.completed" => format!(
            "Subagent completed: {}",
            data["taskLabel"].as_str().unwrap_or("task")
        ),
        "subagent.failed" => format!(
            "Subagent failed: {}",
            data["taskLabel"].as_str().unwrap_or("task")
        ),
        "subagent.timeout" => format!(
            "Subagent timed out: {}",
            data["taskLabel"].as_str().unwrap_or("task")
        ),
        _ => event_type.to_string(),
    }
}

fn summarize_status_error_for_inbox(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Task failed".to_string();
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("does not have access to claude") || lower.contains("please login again") {
        return "Claude access denied. Run `claude login` and verify your organization access."
            .to_string();
    }

    trimmed.lines().next().unwrap_or(trimmed).to_string()
}

fn preview_for_event(event_type: &str, data: &Value) -> String {
    match event_type {
        "cron.completed" => data["summary"].as_str().unwrap_or("Completed").to_string(),
        "cron.failed" => data["error"].as_str().unwrap_or("Failed").to_string(),
        "feed.run.completed" => format!(
            "{} item(s) inserted",
            data["inserted"].as_i64().unwrap_or(0)
        ),
        "feed.run.failed" => data["error"]
            .as_str()
            .unwrap_or("Feed execution failed")
            .to_string(),
        "task.completed" => {
            let d = data["durationMs"].as_i64().unwrap_or(0);
            if d > 0 {
                format!("{d}ms")
            } else {
                "Completed".to_string()
            }
        }
        "task.failed" => summarize_status_error_for_inbox(
            data["errorMessage"].as_str().unwrap_or("Task failed"),
        ),
        "heartbeat.task.failed" => summarize_status_error_for_inbox(
            data["error"].as_str().unwrap_or("Heartbeat task failed"),
        ),
        "heartbeat.task.completed" => data["task"]
            .as_str()
            .unwrap_or("Heartbeat task")
            .to_string(),
        "heartbeat.run.completed" => format!(
            "{} task(s), {} failed",
            data["taskCount"].as_i64().unwrap_or(0),
            data["failedCount"].as_i64().unwrap_or(0)
        ),
        "heartbeat.run.failed" => summarize_status_error_for_inbox(
            data["error"].as_str().unwrap_or("Heartbeat run failed"),
        ),
        "subagent.started" => data["subagentType"]
            .as_str()
            .or_else(|| data["toolName"].as_str())
            .unwrap_or("Subagent task started")
            .to_string(),
        "subagent.completed" => data["result"]
            .as_str()
            .unwrap_or("Subagent task completed")
            .chars()
            .take(240)
            .collect(),
        "subagent.failed" => data["error"]
            .as_str()
            .unwrap_or("Subagent task failed")
            .chars()
            .take(240)
            .collect(),
        "subagent.timeout" => data["error"]
            .as_str()
            .unwrap_or("Subagent task timed out")
            .chars()
            .take(240)
            .collect(),
        _ => String::new(),
    }
}

fn source_id_for_status_event(event_type: &str, data: &Value) -> Option<String> {
    if event_type == "heartbeat.run.failed" || event_type == "heartbeat.run.completed" {
        return Some("heartbeat:run".to_string());
    }

    if event_type == "heartbeat.task.failed" || event_type == "heartbeat.task.completed" {
        if let Some(task) = data["task"].as_str() {
            let normalized = task.trim().to_lowercase();
            if !normalized.is_empty() {
                return Some(format!("heartbeat:{normalized}"));
            }
        }
    }

    data["jobId"]
        .as_str()
        .or_else(|| data["feedId"].as_str())
        .or_else(|| data["id"].as_str())
        .map(ToString::to_string)
}

#[derive(Debug, Clone)]
pub struct StatusInboxCreated {
    pub id: String,
    pub tenant_id: String,
    pub source_type: String,
    pub title: String,
}

pub fn maybe_create_inbox_for_status_event(
    db: &AriaDb,
    default_tenant_id: &str,
    event_type: &str,
    data: &Value,
) -> Result<Option<StatusInboxCreated>> {
    let tenant = data["tenantId"]
        .as_str()
        .unwrap_or(default_tenant_id)
        .to_string();

    let source_type = if event_type.starts_with("cron.") {
        "cron"
    } else if event_type.starts_with("feed.") {
        "feed"
    } else if event_type.starts_with("subagent.") {
        "subagent"
    } else if event_type.starts_with("task.") || event_type.starts_with("heartbeat.") {
        "task"
    } else {
        return Ok(None);
    };

    // Production signal/noise policy:
    // - create inbox for failures
    // - create inbox for cron completions (often intentional background work)
    // - ignore low-signal high-frequency completions (e.g. most feed/task success)
    let create = match event_type {
        "cron.failed"
        | "feed.run.failed"
        | "task.failed"
        | "heartbeat.run.failed"
        | "subagent.failed"
        | "subagent.timeout"
        | "subagent.completed" => true,
        "cron.completed" => true,
        _ => false,
    };
    if !create {
        return Ok(None);
    }

    let title = title_for_event(event_type, data);
    let preview = preview_for_event(event_type, data);
    let run_id = data["runId"].as_str().map(ToString::to_string);
    let chat_id = data["chatId"].as_str().map(ToString::to_string);
    let source_id = source_id_for_status_event(event_type, data);
    let status = if event_type.ends_with(".failed") || event_type.ends_with(".timeout") {
        "unread"
    } else {
        "read"
    };

    let item = NewInboxItem {
        source_type: source_type.to_string(),
        source_id: source_id.clone(),
        run_id,
        chat_id,
        title: title.clone(),
        preview: if preview.is_empty() {
            None
        } else {
            Some(preview.clone())
        },
        body: if preview.is_empty() {
            None
        } else {
            Some(preview)
        },
        metadata: serde_json::json!({
            "eventType": event_type,
            "event": data,
        }),
        status: Some(status.to_string()),
    };

    if event_type == "heartbeat.run.failed" {
        if let Some(ref existing_source_id) = source_id {
            let metadata_json =
                serde_json::to_string(&item.metadata).unwrap_or_else(|_| "{}".to_string());
            let now_ms = Utc::now().timestamp_millis();
            let existing_id = db.with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT id FROM inbox_items
                     WHERE tenant_id=?1
                       AND source_type=?2
                       AND source_id=?3
                       AND status!='archived'
                     ORDER BY created_at DESC
                     LIMIT 1",
                )?;
                let row = stmt.query_row(
                    params![tenant, source_type, existing_source_id],
                    |r| r.get::<_, String>(0),
                );
                match row {
                    Ok(id) => Ok(Some(id)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(e) => Err(e.into()),
                }
            })?;

            if let Some(existing_id) = existing_id {
                db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE inbox_items
                         SET title=?1,
                             preview=?2,
                             body=?3,
                             metadata_json=?4,
                             status='unread',
                             read_at=NULL,
                             created_at=?5
                         WHERE tenant_id=?6
                           AND id=?7",
                        params![
                            item.title,
                            item.preview,
                            item.body,
                            metadata_json,
                            now_ms,
                            tenant,
                            existing_id
                        ],
                    )?;
                    Ok(())
                })?;

                return Ok(Some(StatusInboxCreated {
                    id: existing_id,
                    tenant_id: tenant,
                    source_type: source_type.to_string(),
                    title,
                }));
            }
        }
    }

    let id = create_inbox_item(db, &tenant, &item)?;
    Ok(Some(StatusInboxCreated {
        id,
        tenant_id: tenant,
        source_type: source_type.to_string(),
        title,
    }))
}

#[derive(Debug, Clone)]
pub struct NewInboxItem {
    pub source_type: String,
    pub source_id: Option<String>,
    pub run_id: Option<String>,
    pub chat_id: Option<String>,
    pub title: String,
    pub preview: Option<String>,
    pub body: Option<String>,
    pub metadata: Value,
    pub status: Option<String>,
}

fn normalized_inbox_status(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or("unread") {
        "read" => "read",
        "archived" => "archived",
        _ => "unread",
    }
}

pub fn create_inbox_item(db: &AriaDb, tenant_id: &str, item: &NewInboxItem) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let ts_ms = Utc::now().timestamp_millis();
    let metadata = serde_json::to_string(&item.metadata).unwrap_or_else(|_| "{}".to_string());
    let status = normalized_inbox_status(item.status.as_deref());
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO inbox_items
             (id, tenant_id, source_type, source_id, run_id, chat_id, title, preview, body, metadata_json, status, created_at, read_at, archived_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
               CASE WHEN ?11='read' THEN ?12 ELSE NULL END,
               CASE WHEN ?11='archived' THEN ?12 ELSE NULL END)",
            params![
                id,
                tenant_id,
                item.source_type,
                item.source_id,
                item.run_id,
                item.chat_id,
                item.title,
                item.preview,
                item.body,
                metadata,
                status,
                ts_ms
            ]
        )?;
        Ok(())
    })?;
    Ok(id)
}

pub fn seed_inbox_examples(db: &AriaDb, tenant_id: &str) -> Result<Vec<String>> {
    let examples = [
        NewInboxItem {
            source_type: "system".to_string(),
            source_id: Some("ops-announce".to_string()),
            run_id: None,
            chat_id: None,
            title: "Welcome to Inbox".to_string(),
            preview: Some("Inbox stores intentional agent-to-user updates.".to_string()),
            body: Some("This is a seeded example message with full body content so you can validate expansion behavior and metadata rendering.".to_string()),
            metadata: serde_json::json!({
                "kind": "onboarding",
                "priority": "low",
                "tags": ["inbox", "seed", "ux"]
            }),
            status: Some("unread".to_string()),
        },
        NewInboxItem {
            source_type: "agent".to_string(),
            source_id: Some("agent:planner".to_string()),
            run_id: Some(uuid::Uuid::new_v4().to_string()),
            chat_id: None,
            title: "Action Recommended: Refresh API key".to_string(),
            preview: Some("Gateway requests to external provider are nearing quota limits.".to_string()),
            body: Some("I detected repeated near-limit responses from the upstream provider. Rotate the key and confirm gateway health after rotation.".to_string()),
            metadata: serde_json::json!({
                "priority": "high",
                "category": "operations",
                "confidence": 0.92,
                "suggestedActions": [
                    "Rotate provider key",
                    "Verify /health",
                    "Run quick smoke test"
                ]
            }),
            status: Some("unread".to_string()),
        },
        NewInboxItem {
            source_type: "subagent".to_string(),
            source_id: Some("subagent:research".to_string()),
            run_id: Some(uuid::Uuid::new_v4().to_string()),
            chat_id: None,
            title: "Research Summary Ready".to_string(),
            preview: Some("3 source docs indexed with follow-up questions.".to_string()),
            body: Some("Completed research pass. The metadata includes document IDs and extracted entities for quick review.".to_string()),
            metadata: serde_json::json!({
                "priority": "medium",
                "documents": ["doc-17", "doc-44", "doc-99"],
                "entities": ["Cron", "Heartbeat", "Gateway"],
                "followups": ["validate schedule", "review alerting"]
            }),
            status: Some("read".to_string()),
        },
        NewInboxItem {
            source_type: "system".to_string(),
            source_id: Some("audit-log".to_string()),
            run_id: None,
            chat_id: None,
            title: "Archived Example".to_string(),
            preview: Some("Demonstrates archived inbox state.".to_string()),
            body: Some("This seeded row exists so archive filtering can be verified end-to-end.".to_string()),
            metadata: serde_json::json!({
                "priority": "low",
                "category": "demo"
            }),
            status: Some("archived".to_string()),
        },
    ];

    let mut ids = Vec::new();
    for item in &examples {
        ids.push(create_inbox_item(db, tenant_id, item)?);
    }
    Ok(ids)
}

pub fn persist_status_event(
    db: &AriaDb,
    default_tenant_id: &str,
    event_type: &str,
    data: &Value,
    timestamp_rfc3339: &str,
) -> Result<()> {
    let ts_ms = parse_event_millis(timestamp_rfc3339);
    let tenant = data["tenantId"].as_str().unwrap_or(default_tenant_id);
    let title = title_for_event(event_type, data);
    let description = preview_for_event(event_type, data);
    let source = source_for_event(event_type);
    let metadata = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_string());
    let event_id = uuid::Uuid::new_v4().to_string();

    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO events (id, tenant_id, type, title, description, source, metadata_json, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event_id,
                tenant,
                event_type,
                title,
                if description.is_empty() {
                    None::<String>
                } else {
                    Some(description)
                },
                source,
                metadata,
                ts_ms
            ],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_inbox_item_persists_row() {
        let db = AriaDb::open_in_memory().unwrap();
        ensure_schema(&db).unwrap();

        let item = NewInboxItem {
            source_type: "agent".to_string(),
            source_id: Some("agent:test".to_string()),
            run_id: None,
            chat_id: None,
            title: "Hello".to_string(),
            preview: Some("Preview".to_string()),
            body: Some("Body".to_string()),
            metadata: serde_json::json!({"priority":"low"}),
            status: Some("unread".to_string()),
        };
        let id = create_inbox_item(&db, "dev-tenant", &item).unwrap();

        let row = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT title, source_type, status FROM inbox_items WHERE id=?1",
                    params![id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )?)
            })
            .unwrap();

        assert_eq!(row.0, "Hello");
        assert_eq!(row.1, "agent");
        assert_eq!(row.2, "unread");
    }

    #[test]
    fn persist_status_event_only_writes_events_table() {
        let db = AriaDb::open_in_memory().unwrap();
        ensure_schema(&db).unwrap();

        persist_status_event(
            &db,
            "dev-tenant",
            "cron.completed",
            &serde_json::json!({"jobId":"j1","summary":"done"}),
            "2026-02-16T00:00:00Z",
        )
        .unwrap();

        let (events_count, inbox_count) = db
            .with_conn(|conn| {
                let events_count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
                let inbox_count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0))?;
                Ok((events_count, inbox_count))
            })
            .unwrap();

        assert_eq!(events_count, 1);
        assert_eq!(inbox_count, 0);
    }

    #[test]
    fn maybe_create_inbox_for_subagent_completed_creates_read_item() {
        let db = AriaDb::open_in_memory().unwrap();
        ensure_schema(&db).unwrap();

        let created = maybe_create_inbox_for_status_event(
            &db,
            "dev-tenant",
            "subagent.completed",
            &serde_json::json!({
                "tenantId": "dev-tenant",
                "taskLabel": "Research trend",
                "result": "Summary output",
                "runId": "run-1",
                "chatId": "chat-1",
                "toolId": "tool-1"
            }),
        )
        .unwrap()
        .expect("should create inbox row");

        assert_eq!(created.source_type, "subagent");

        let row = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT source_type, status, title FROM inbox_items WHERE id=?1",
                    params![created.id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )?)
            })
            .unwrap();

        assert_eq!(row.0, "subagent");
        assert_eq!(row.1, "read");
        assert!(row.2.contains("Subagent completed"));
    }

    #[test]
    fn maybe_create_inbox_for_subagent_timeout_creates_unread_item() {
        let db = AriaDb::open_in_memory().unwrap();
        ensure_schema(&db).unwrap();

        let created = maybe_create_inbox_for_status_event(
            &db,
            "dev-tenant",
            "subagent.timeout",
            &serde_json::json!({
                "tenantId": "dev-tenant",
                "taskLabel": "Long running step",
                "error": "timed out waiting for response",
                "runId": "run-2",
                "chatId": "chat-2",
                "toolId": "tool-2"
            }),
        )
        .unwrap()
        .expect("should create inbox row");

        let row = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT status, preview FROM inbox_items WHERE id=?1",
                    params![created.id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
                )?)
            })
            .unwrap();

        assert_eq!(row.0, "unread");
        assert!(row.1.unwrap_or_default().contains("timed out"));
    }

    #[test]
    fn heartbeat_run_failed_upserts_existing_inbox_item() {
        let db = AriaDb::open_in_memory().unwrap();
        ensure_schema(&db).unwrap();

        let first = maybe_create_inbox_for_status_event(
            &db,
            "dev-tenant",
            "heartbeat.run.failed",
            &serde_json::json!({
                "tenantId": "dev-tenant",
                "taskCount": 3,
                "successCount": 0,
                "failedCount": 3,
                "error": "All providers failed. Attempts: claude-cli attempt 1/3: Your organization does not have access to Claude.",
            }),
        )
        .unwrap()
        .expect("first insert");

        let second = maybe_create_inbox_for_status_event(
            &db,
            "dev-tenant",
            "heartbeat.run.failed",
            &serde_json::json!({
                "tenantId": "dev-tenant",
                "taskCount": 3,
                "successCount": 0,
                "failedCount": 3,
                "error": "All providers failed. Attempts: claude-cli attempt 1/3: Your organization does not have access to Claude.",
            }),
        )
        .unwrap()
        .expect("second upsert");

        assert_eq!(first.id, second.id);

        let row = db
            .with_conn(|conn| {
                let count: i64 = conn.query_row("SELECT COUNT(*) FROM inbox_items", [], |r| r.get(0))?;
                let tuple = conn.query_row(
                    "SELECT title, preview, status FROM inbox_items WHERE id=?1",
                    params![first.id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )?;
                Ok((count, tuple.0, tuple.1.unwrap_or_default(), tuple.2))
            })
            .unwrap();

        assert_eq!(row.0, 1);
        assert!(row.1.contains("Heartbeat run failed"));
        assert!(row.2.contains("Claude access denied"));
        assert_eq!(row.3, "unread");
    }
}
