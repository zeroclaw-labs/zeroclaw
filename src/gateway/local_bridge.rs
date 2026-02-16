use crate::agent::executor::{ExternalToolCall, ExternalToolExecutor, ExternalToolResult};
use crate::aria::db::AriaDb;
use crate::security::SecurityPolicy;
use anyhow::Result;
use async_trait::async_trait;
use axum::extract::ws;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::time::{timeout, Duration};

const ACK_TIMEOUT_MS: u64 = 3_000;
const RESULT_WAIT_TIMEOUT_MS: u64 = 70_000;
const APPROVAL_WAIT_SECS: u64 = 45;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalToolPolicy {
    pub workspace_root: String,
    pub workspace_only: bool,
    pub allowed_commands: Vec<String>,
    pub forbidden_paths: Vec<String>,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalEnvelope {
    pub mode: String,
    pub risk: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalToolRequestEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    pub request_id: String,
    pub run_id: String,
    pub chat_id: String,
    pub tenant_id: String,
    pub tool_call: LocalToolCall,
    pub policy: LocalToolPolicy,
    pub approval: ApprovalEnvelope,
    pub sent_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalToolAckEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    pub request_id: String,
    pub accepted: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletedToolCall {
    pub id: String,
    pub name: String,
    pub status: String,
    pub duration: String,
    pub result: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalExecutionMetadata {
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
    pub policy_denied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalToolResultEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    pub request_id: String,
    pub run_id: String,
    pub chat_id: String,
    pub tool_call: CompletedToolCall,
    pub execution: LocalExecutionMetadata,
}

#[derive(Debug, Clone)]
struct BridgeClientConnection {
    sender: mpsc::UnboundedSender<String>,
    device_id: String,
}

#[derive(Debug)]
pub struct LocalToolBridge {
    clients: RwLock<HashMap<String, HashMap<String, BridgeClientConnection>>>,
    pending_results: Mutex<HashMap<String, oneshot::Sender<LocalToolResultEnvelope>>>,
    pending_acks: Mutex<HashMap<String, oneshot::Sender<LocalToolAckEnvelope>>>,
    result_cache: Mutex<HashMap<String, LocalToolResultEnvelope>>,
    security: Arc<SecurityPolicy>,
    registry_db: AriaDb,
}

impl LocalToolBridge {
    pub fn new(security: Arc<SecurityPolicy>, registry_db: AriaDb) -> Self {
        Self {
            clients: RwLock::new(HashMap::new()),
            pending_results: Mutex::new(HashMap::new()),
            pending_acks: Mutex::new(HashMap::new()),
            result_cache: Mutex::new(HashMap::new()),
            security,
            registry_db,
        }
    }

    pub async fn register_client(
        &self,
        tenant_id: String,
        device_id: String,
        sender: mpsc::UnboundedSender<String>,
    ) -> String {
        let connection_id = uuid::Uuid::new_v4().to_string();
        let mut guard = self.clients.write().await;
        guard.entry(tenant_id).or_default().insert(
            connection_id.clone(),
            BridgeClientConnection { sender, device_id },
        );
        connection_id
    }

    pub async fn unregister_client(&self, tenant_id: &str, connection_id: &str) {
        let mut guard = self.clients.write().await;
        if let Some(pool) = guard.get_mut(tenant_id) {
            pool.remove(connection_id);
            if pool.is_empty() {
                guard.remove(tenant_id);
            }
        }
    }

    async fn pick_sender(
        &self,
        tenant_id: &str,
        preferred_device: Option<&str>,
    ) -> Option<mpsc::UnboundedSender<String>> {
        let guard = self.clients.read().await;
        let pool = guard.get(tenant_id)?;
        if let Some(device) = preferred_device {
            for conn in pool.values() {
                if conn.device_id == device {
                    return Some(conn.sender.clone());
                }
            }
        }
        pool.values().next().map(|c| c.sender.clone())
    }

    pub async fn on_ack(&self, ack: LocalToolAckEnvelope) {
        if let Some(tx) = self.pending_acks.lock().await.remove(&ack.request_id) {
            let _ = tx.send(ack);
        }
    }

    pub async fn on_result(&self, result: LocalToolResultEnvelope) {
        self.result_cache
            .lock()
            .await
            .insert(result.request_id.clone(), result.clone());
        if let Some(tx) = self.pending_results.lock().await.remove(&result.request_id) {
            let _ = tx.send(result);
        }
    }

    async fn execute_request(
        &self,
        req: LocalToolRequestEnvelope,
        preferred_device: Option<&str>,
    ) -> Result<LocalToolResultEnvelope> {
        if let Some(cached) = self.result_cache.lock().await.get(&req.request_id).cloned() {
            return Ok(cached);
        }

        let sender = self
            .pick_sender(&req.tenant_id, preferred_device)
            .await
            .ok_or_else(|| anyhow::anyhow!("bridge_disconnected"))?;

        let (ack_tx, ack_rx) = oneshot::channel::<LocalToolAckEnvelope>();
        let (result_tx, result_rx) = oneshot::channel::<LocalToolResultEnvelope>();
        self.pending_acks
            .lock()
            .await
            .insert(req.request_id.clone(), ack_tx);
        self.pending_results
            .lock()
            .await
            .insert(req.request_id.clone(), result_tx);

        let payload = serde_json::to_string(&req)?;
        if sender.send(payload).is_err() {
            self.pending_acks.lock().await.remove(&req.request_id);
            self.pending_results.lock().await.remove(&req.request_id);
            return Err(anyhow::anyhow!("bridge_disconnected"));
        }

        match timeout(Duration::from_millis(ACK_TIMEOUT_MS), ack_rx).await {
            Ok(Ok(ack)) => {
                if !ack.accepted {
                    self.pending_results.lock().await.remove(&req.request_id);
                    let reason = ack.reason.unwrap_or_else(|| "bridge_rejected".to_string());
                    return Err(anyhow::anyhow!(reason));
                }
            }
            _ => {
                self.pending_results.lock().await.remove(&req.request_id);
                return Err(anyhow::anyhow!("bridge_ack_timeout"));
            }
        }

        match timeout(Duration::from_millis(RESULT_WAIT_TIMEOUT_MS), result_rx).await {
            Ok(Ok(result)) => Ok(result),
            _ => {
                self.pending_results.lock().await.remove(&req.request_id);
                Err(anyhow::anyhow!("execution_timeout"))
            }
        }
    }

    fn is_eligible_tool(name: &str) -> bool {
        matches!(name, "shell" | "file_read" | "file_write")
    }

    fn classify_approval(name: &str, args: &serde_json::Value) -> ApprovalEnvelope {
        if name == "file_write" {
            return ApprovalEnvelope {
                mode: "require_user".to_string(),
                risk: "high".to_string(),
            };
        }

        if name == "shell" {
            let command = args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            let risky = [
                " rm ",
                " mv ",
                " chmod ",
                " chown ",
                "sudo",
                " git push",
                " git reset",
                " tee ",
                ">",
                "mkdir",
                "rmdir",
                "touch",
                "npm publish",
                "cargo publish",
            ]
            .iter()
            .any(|s| command.contains(s.trim()));
            if risky {
                return ApprovalEnvelope {
                    mode: "require_user".to_string(),
                    risk: "high".to_string(),
                };
            }
        }

        ApprovalEnvelope {
            mode: "auto".to_string(),
            risk: "low".to_string(),
        }
    }

    async fn await_approval(
        &self,
        tenant_id: &str,
        run_id: &str,
        chat_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
        approval: &ApprovalEnvelope,
    ) -> Result<()> {
        if approval.mode != "require_user" {
            return Ok(());
        }

        let approval_id = uuid::Uuid::new_v4().to_string();
        let command = if tool_name == "shell" {
            args.get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(tool_name)
                .to_string()
        } else {
            format!("{} {}", tool_name, args)
        };
        let now = chrono::Utc::now().timestamp_millis();
        let expires_at = now + (APPROVAL_WAIT_SECS as i64) * 1000;
        let metadata = json!({
            "session": chat_id,
            "source": "local-bridge",
            "risk": approval.risk,
            "description": format!("Approve local execution for run {}", run_id),
        });

        self.registry_db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO approvals (id, tenant_id, command, metadata_json, countdown, status, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
                rusqlite::params![
                    approval_id,
                    tenant_id,
                    command,
                    metadata.to_string(),
                    APPROVAL_WAIT_SECS as i64,
                    now,
                    expires_at
                ],
            )?;
            Ok(())
        })?;

        crate::status_events::emit(
            "approval_request",
            json!({
                "id": approval_id,
                "command": command,
                "metadata": metadata,
                "countdown": APPROVAL_WAIT_SECS,
                "createdAt": chrono::Utc::now().to_rfc3339(),
                "expiresAt": chrono::DateTime::<chrono::Utc>::from_timestamp_millis(expires_at).map(|d| d.to_rfc3339()),
                "status": "pending",
            }),
        );

        let start = std::time::Instant::now();
        loop {
            if start.elapsed() >= Duration::from_secs(APPROVAL_WAIT_SECS) {
                let _ = self.registry_db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE approvals SET status='expired' WHERE tenant_id=?1 AND id=?2 AND status='pending'",
                        rusqlite::params![tenant_id, approval_id],
                    )?;
                    Ok(())
                });
                return Err(anyhow::anyhow!("approval_denied"));
            }

            let status = self.registry_db.with_conn(|conn| {
                let mut stmt = conn
                    .prepare("SELECT status FROM approvals WHERE tenant_id=?1 AND id=?2 LIMIT 1")?;
                let status = stmt
                    .query_row(rusqlite::params![tenant_id, approval_id], |row| {
                        row.get::<_, String>(0)
                    })
                    .unwrap_or_else(|_| "pending".to_string());
                Ok(status)
            })?;

            if status == "approved" {
                return Ok(());
            }
            if status == "denied" || status == "expired" {
                return Err(anyhow::anyhow!("approval_denied"));
            }

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn handle_socket(
        self: Arc<Self>,
        mut socket: ws::WebSocket,
        tenant_id: String,
        device_id: String,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let connection_id = self
            .register_client(tenant_id.clone(), device_id.clone(), tx)
            .await;
        tracing::info!(
            tenant_id,
            device_id,
            connection_id,
            "local bridge client connected"
        );

        loop {
            tokio::select! {
                outbound = rx.recv() => {
                    match outbound {
                        Some(json_text) => {
                            if socket.send(ws::Message::Text(json_text.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                inbound = socket.recv() => {
                    let Some(Ok(message)) = inbound else {
                        break;
                    };
                    let ws::Message::Text(text) = message else {
                        continue;
                    };
                    let parsed: serde_json::Value = match serde_json::from_str(text.as_ref()) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    let Some(kind) = parsed.get("type").and_then(serde_json::Value::as_str) else {
                        continue;
                    };
                    match kind {
                        "local_tool.ack" => {
                            if let Ok(ack) = serde_json::from_value::<LocalToolAckEnvelope>(parsed.clone()) {
                                self.on_ack(ack).await;
                            }
                        }
                        "local_tool.result" => {
                            if let Ok(result) = serde_json::from_value::<LocalToolResultEnvelope>(parsed) {
                                self.on_result(result).await;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        self.unregister_client(&tenant_id, &connection_id).await;
        tracing::info!(
            tenant_id,
            device_id,
            connection_id,
            "local bridge client disconnected"
        );
    }

    fn policy(&self) -> LocalToolPolicy {
        LocalToolPolicy {
            workspace_root: self.security.workspace_dir.display().to_string(),
            workspace_only: self.security.workspace_only,
            allowed_commands: self.security.allowed_commands.clone(),
            forbidden_paths: self.security.forbidden_paths.clone(),
            timeout_ms: 60_000,
            max_output_bytes: 1_048_576,
        }
    }
}

#[async_trait]
impl ExternalToolExecutor for LocalToolBridge {
    async fn execute_external_tool(
        &self,
        call: &ExternalToolCall,
    ) -> Result<Option<ExternalToolResult>> {
        if !Self::is_eligible_tool(&call.tool_name) {
            return Ok(None);
        }

        let approval = Self::classify_approval(&call.tool_name, &call.tool_input);
        if let Err(e) = self
            .await_approval(
                &call.tenant_id,
                &call.run_id,
                &call.chat_id,
                &call.tool_name,
                &call.tool_input,
                &approval,
            )
            .await
        {
            return Ok(Some(ExternalToolResult {
                output: format!("Local tool denied: {e}"),
                is_error: true,
            }));
        }

        let req = LocalToolRequestEnvelope {
            envelope_type: "local_tool.request".to_string(),
            request_id: uuid::Uuid::new_v4().to_string(),
            run_id: call.run_id.clone(),
            chat_id: call.chat_id.clone(),
            tenant_id: call.tenant_id.clone(),
            tool_call: LocalToolCall {
                id: call.call_id.clone(),
                name: call.tool_name.clone(),
                args: call.tool_input.clone(),
            },
            policy: self.policy(),
            approval,
            sent_at: chrono::Utc::now().to_rfc3339(),
        };

        let res = self.execute_request(req, None).await;
        match res {
            Ok(envelope) => {
                let is_error = envelope.tool_call.status == "error";
                Ok(Some(ExternalToolResult {
                    output: envelope.tool_call.result,
                    is_error,
                }))
            }
            Err(e) => Ok(Some(ExternalToolResult {
                output: format!("Local bridge error: {e}"),
                is_error: true,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use serde_json::json;
    use tokio::sync::mpsc;

    fn test_security(workspace: std::path::PathBuf) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: workspace,
            ..SecurityPolicy::default()
        })
    }

    async fn setup_bridge() -> Arc<LocalToolBridge> {
        let workspace =
            std::env::temp_dir().join(format!("zc_bridge_test_{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let db = AriaDb::open_in_memory().unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS approvals (
                    id TEXT PRIMARY KEY,
                    tenant_id TEXT NOT NULL,
                    command TEXT NOT NULL,
                    metadata_json TEXT,
                    countdown INTEGER DEFAULT 30,
                    status TEXT DEFAULT 'pending',
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER
                );",
            )?;
            Ok(())
        })
        .unwrap();
        Arc::new(LocalToolBridge::new(test_security(workspace), db))
    }

    #[tokio::test]
    async fn external_tool_executes_via_bridge() {
        let bridge = setup_bridge().await;
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        bridge
            .register_client("tenant-a".to_string(), "device-a".to_string(), tx)
            .await;

        let bridge_clone = bridge.clone();
        tokio::spawn(async move {
            let request_json = rx.recv().await.unwrap();
            let request: LocalToolRequestEnvelope = serde_json::from_str(&request_json).unwrap();

            bridge_clone
                .on_ack(LocalToolAckEnvelope {
                    envelope_type: "local_tool.ack".to_string(),
                    request_id: request.request_id.clone(),
                    accepted: true,
                    reason: None,
                })
                .await;

            bridge_clone
                .on_result(LocalToolResultEnvelope {
                    envelope_type: "local_tool.result".to_string(),
                    request_id: request.request_id,
                    run_id: request.run_id,
                    chat_id: request.chat_id,
                    tool_call: CompletedToolCall {
                        id: request.tool_call.id,
                        name: request.tool_call.name,
                        status: "success".to_string(),
                        duration: "12ms".to_string(),
                        result: "bridge-ok".to_string(),
                        completed_at: chrono::Utc::now().to_rfc3339(),
                    },
                    execution: LocalExecutionMetadata {
                        exit_code: Some(0),
                        timed_out: false,
                        truncated: false,
                        policy_denied: false,
                    },
                })
                .await;
        });

        let result = bridge
            .execute_external_tool(&ExternalToolCall {
                tenant_id: "tenant-a".to_string(),
                chat_id: "chat-a".to_string(),
                run_id: "run-a".to_string(),
                call_id: "call-a".to_string(),
                tool_name: "shell".to_string(),
                tool_input: json!({"command": "ls"}),
            })
            .await
            .unwrap();

        let result = result.expect("eligible tool should be routed");
        assert!(!result.is_error);
        assert_eq!(result.output, "bridge-ok");
    }

    #[tokio::test]
    async fn execute_request_returns_cached_on_duplicate_request_id() {
        let bridge = setup_bridge().await;
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn_id = bridge
            .register_client("tenant-a".to_string(), "device-a".to_string(), tx)
            .await;

        let request = LocalToolRequestEnvelope {
            envelope_type: "local_tool.request".to_string(),
            request_id: "req-dup-1".to_string(),
            run_id: "run-a".to_string(),
            chat_id: "chat-a".to_string(),
            tenant_id: "tenant-a".to_string(),
            tool_call: LocalToolCall {
                id: "call-1".to_string(),
                name: "shell".to_string(),
                args: json!({"command": "ls"}),
            },
            policy: bridge.policy(),
            approval: ApprovalEnvelope {
                mode: "auto".to_string(),
                risk: "low".to_string(),
            },
            sent_at: chrono::Utc::now().to_rfc3339(),
        };

        let bridge_clone = bridge.clone();
        tokio::spawn(async move {
            let request_json = rx.recv().await.unwrap();
            let request: LocalToolRequestEnvelope = serde_json::from_str(&request_json).unwrap();
            bridge_clone
                .on_ack(LocalToolAckEnvelope {
                    envelope_type: "local_tool.ack".to_string(),
                    request_id: request.request_id.clone(),
                    accepted: true,
                    reason: None,
                })
                .await;
            bridge_clone
                .on_result(LocalToolResultEnvelope {
                    envelope_type: "local_tool.result".to_string(),
                    request_id: request.request_id,
                    run_id: request.run_id,
                    chat_id: request.chat_id,
                    tool_call: CompletedToolCall {
                        id: request.tool_call.id,
                        name: request.tool_call.name,
                        status: "success".to_string(),
                        duration: "3ms".to_string(),
                        result: "cached-value".to_string(),
                        completed_at: chrono::Utc::now().to_rfc3339(),
                    },
                    execution: LocalExecutionMetadata {
                        exit_code: Some(0),
                        timed_out: false,
                        truncated: false,
                        policy_denied: false,
                    },
                })
                .await;
        });

        let first = bridge.execute_request(request.clone(), None).await.unwrap();
        assert_eq!(first.tool_call.result, "cached-value");

        bridge.unregister_client("tenant-a", &conn_id).await;
        let second = bridge.execute_request(request, None).await.unwrap();
        assert_eq!(second.tool_call.result, "cached-value");
    }

    #[tokio::test]
    async fn approval_denied_short_circuits_external_execution() {
        let bridge = setup_bridge().await;
        let call = ExternalToolCall {
            tenant_id: "tenant-a".to_string(),
            chat_id: "chat-a".to_string(),
            run_id: "run-a".to_string(),
            call_id: "call-w".to_string(),
            tool_name: "file_write".to_string(),
            tool_input: json!({"path":"a.txt","content":"hello"}),
        };

        let bridge_clone = bridge.clone();
        tokio::spawn(async move {
            // wait until approval row appears, then deny it
            for _ in 0..30 {
                let id = bridge_clone
                    .registry_db
                    .with_conn(|conn| {
                        let mut stmt = conn.prepare(
                            "SELECT id FROM approvals WHERE tenant_id='tenant-a' ORDER BY created_at DESC LIMIT 1",
                        )?;
                        let id = stmt
                            .query_row([], |row| row.get::<_, String>(0))
                            .unwrap_or_default();
                        Ok(id)
                    })
                    .unwrap();
                if !id.is_empty() {
                    let _ = bridge_clone.registry_db.with_conn(|conn| {
                        conn.execute(
                            "UPDATE approvals SET status='denied' WHERE id=?1",
                            rusqlite::params![id],
                        )?;
                        Ok(())
                    });
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });

        let res = bridge.execute_external_tool(&call).await.unwrap().unwrap();
        assert!(res.is_error);
        assert!(res.output.contains("approval_denied"));
    }

    #[tokio::test]
    async fn live_websocket_roundtrip_end_to_end() {
        use axum::{extract::State, routing::get, Router};
        use futures_util::{SinkExt, StreamExt};

        async fn ws_handler(
            ws: axum::extract::WebSocketUpgrade,
            State(bridge): State<Arc<LocalToolBridge>>,
        ) -> impl axum::response::IntoResponse {
            ws.on_upgrade(move |socket| {
                bridge.handle_socket(socket, "tenant-live".to_string(), "device-live".to_string())
            })
        }

        let bridge = setup_bridge().await;
        let app = Router::new()
            .route("/ws/local-bridge", get(ws_handler))
            .with_state(bridge.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("ws://{}/ws/local-bridge", addr);
        let (ws_stream, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let (mut write, mut read) = ws_stream.split();

        let client_task = tokio::spawn(async move {
            let incoming = read.next().await.unwrap().unwrap();
            let text = incoming.into_text().unwrap();
            let req: LocalToolRequestEnvelope = serde_json::from_str(&text).unwrap();

            let ack = LocalToolAckEnvelope {
                envelope_type: "local_tool.ack".to_string(),
                request_id: req.request_id.clone(),
                accepted: true,
                reason: None,
            };
            write
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::to_string(&ack).unwrap().into(),
                ))
                .await
                .unwrap();

            let result = LocalToolResultEnvelope {
                envelope_type: "local_tool.result".to_string(),
                request_id: req.request_id,
                run_id: req.run_id,
                chat_id: req.chat_id,
                tool_call: CompletedToolCall {
                    id: req.tool_call.id,
                    name: req.tool_call.name,
                    status: "success".to_string(),
                    duration: "5ms".to_string(),
                    result: "live-e2e-ok".to_string(),
                    completed_at: chrono::Utc::now().to_rfc3339(),
                },
                execution: LocalExecutionMetadata {
                    exit_code: Some(0),
                    timed_out: false,
                    truncated: false,
                    policy_denied: false,
                },
            };
            write
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::to_string(&result).unwrap().into(),
                ))
                .await
                .unwrap();
        });

        let out = bridge
            .execute_external_tool(&ExternalToolCall {
                tenant_id: "tenant-live".to_string(),
                chat_id: "chat-live".to_string(),
                run_id: "run-live".to_string(),
                call_id: "call-live".to_string(),
                tool_name: "shell".to_string(),
                tool_input: json!({"command":"ls"}),
            })
            .await
            .unwrap()
            .unwrap();

        assert!(!out.is_error);
        assert_eq!(out.output, "live-e2e-ok");

        client_task.await.unwrap();
        server.abort();
    }
}
