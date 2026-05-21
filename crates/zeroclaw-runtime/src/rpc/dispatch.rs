//! JSON-RPC 2.0 method dispatch. Transport-agnostic.

use super::session::SessionStore;
use super::transport::RpcTransport;
use super::turn::{TurnOutcome, execute_turn};
use crate::agent::agent::TurnEvent;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

use zeroclaw_api::jsonrpc::error_codes::*;
use zeroclaw_api::jsonrpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, RpcOutbound};
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::session_backend::SessionBackend;

/// Wire protocol version. Bump on breaking changes.
pub const RPC_PROTOCOL_VERSION: u64 = 1;

// ── Method registry ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    Initialize,
    SessionNew,
    SessionClose,
    SessionPrompt,
    SessionConfigure,
    SessionCancel,
    Status,
}

impl Method {
    const ALL: &[(Method, &str)] = &[
        (Method::Initialize, "initialize"),
        (Method::SessionNew, "session/new"),
        (Method::SessionClose, "session/close"),
        (Method::SessionPrompt, "session/prompt"),
        (Method::SessionConfigure, "session/configure"),
        (Method::SessionCancel, "session/cancel"),
        (Method::Status, "status"),
    ];

    fn from_wire(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, wire)| *wire == s)
            .map(|(m, _)| *m)
    }
}

type RpcResult = Result<Value, JsonRpcError>;

fn rpc_err(code: i32, msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code,
        message: msg.into(),
        data: None,
    }
}

/// Per-connection dispatcher. Shared state lives in [`SessionStore`].
pub struct RpcDispatcher {
    config: Config,
    sessions: Arc<SessionStore>,
    rpc: Arc<RpcOutbound>,
    session_backend: Option<Arc<dyn SessionBackend>>,
    authenticated: bool,
}

impl RpcDispatcher {
    pub fn new(
        config: Config,
        sessions: Arc<SessionStore>,
        session_backend: Option<Arc<dyn SessionBackend>>,
        writer_tx: mpsc::Sender<String>,
    ) -> Self {
        Self {
            config,
            sessions,
            rpc: Arc::new(RpcOutbound::new(writer_tx)),
            session_backend,
            authenticated: false,
        }
    }

    /// Read frames from transport, dispatch, repeat.
    pub async fn run(&mut self, transport: &mut (dyn RpcTransport + Send)) {
        while let Some(line) = transport.next_frame().await {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            self.process_line(trimmed).await;
        }
    }

    async fn process_line(&mut self, line: &str) {
        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                self.send_error(Value::Null, PARSE_ERROR, &format!("Parse error: {e}"))
                    .await;
                return;
            }
        };

        // Bidirectional RPC: responses to our outbound requests.
        if req.method.is_empty() {
            if let Some(id) = req.id.as_ref().and_then(Value::as_str) {
                self.rpc.dispatch_response(id, Some(req.params), None);
            }
            return;
        }

        let id = req.id.clone().unwrap_or(Value::Null);
        let is_notification = req.id.is_none();

        let method = match Method::from_wire(&req.method) {
            Some(m) => m,
            None => {
                if !is_notification {
                    self.send_error(
                        id,
                        METHOD_NOT_FOUND,
                        &format!("Unknown method: {}", req.method),
                    )
                    .await;
                }
                return;
            }
        };

        if !self.authenticated && method != Method::Initialize {
            if !is_notification {
                self.send_error(id, AUTH_REQUIRED, "First call must be 'initialize'")
                    .await;
            }
            return;
        }

        let result = match method {
            Method::Initialize => self.handle_initialize(&req.params).await,
            Method::SessionNew => self.handle_session_new(&req.params).await,
            Method::SessionClose => self.handle_session_close(&req.params).await,
            Method::SessionPrompt => self.handle_session_prompt(&req.params).await,
            Method::SessionConfigure => self.handle_session_configure(&req.params).await,
            Method::SessionCancel => self.handle_session_cancel(&req.params),
            Method::Status => self.handle_status().await,
        };

        if is_notification {
            return;
        }

        match result {
            Ok(v) => self.send_result(id, v).await,
            Err(e) => self.send_error(id, e.code, &e.message).await,
        }
    }

    // ── Handlers ─────────────────────────────────────────────────

    async fn handle_initialize(&mut self, params: &Value) -> RpcResult {
        let protocol_version = params
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(RPC_PROTOCOL_VERSION);

        if protocol_version != RPC_PROTOCOL_VERSION {
            return Err(rpc_err(
                VERSION_MISMATCH,
                format!(
                    "Protocol version mismatch: server={RPC_PROTOCOL_VERSION}, client={protocol_version}"
                ),
            ));
        }

        self.authenticated = true;

        Ok(json!({
            "protocolVersion": RPC_PROTOCOL_VERSION,
            "serverVersion": env!("CARGO_PKG_VERSION"),
        }))
    }

    async fn handle_session_new(&self, params: &Value) -> RpcResult {
        let agent_alias = params
            .get("agentAlias")
            .and_then(Value::as_str)
            .ok_or_else(|| rpc_err(INVALID_PARAMS, "agentAlias is required"))?;
        let session_cwd = params.get("cwd").and_then(Value::as_str);
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let cwd_path = session_cwd.map(std::path::Path::new);
        let agent = crate::agent::agent::Agent::from_config_with_session_cwd_and_mcp(
            &self.config,
            agent_alias,
            cwd_path,
            false,
        )
        .await
        .map_err(|e| rpc_err(INTERNAL_ERROR, format!("Failed to create agent: {e}")))?;

        self.sessions
            .insert(
                session_id.clone(),
                super::session::RpcSession::new(agent, agent_alias, session_cwd.unwrap_or(".")),
            )
            .await
            .map_err(|_| rpc_err(SESSION_LIMIT_REACHED, "Session limit reached"))?;

        // Restore persisted history if available.
        let mut message_count = 0;
        if let Some(ref backend) = self.session_backend {
            let stored = backend.load(&format!("rpc_{session_id}"));
            if !stored.is_empty() {
                self.sessions.seed_history(&session_id, &stored).await;
                message_count = stored.len();
            }
        }

        Ok(json!({
            "sessionId": session_id,
            "agentAlias": agent_alias,
            "messageCount": message_count,
        }))
    }

    async fn handle_session_close(&self, params: &Value) -> RpcResult {
        let sid = require_str(params, "sessionId")?;
        if !self.sessions.remove(sid).await {
            return Err(rpc_err(SESSION_NOT_FOUND, "Session not found"));
        }
        Ok(json!({ "sessionId": sid, "closed": true }))
    }

    async fn handle_session_prompt(&self, params: &Value) -> RpcResult {
        let sid = require_str(params, "sessionId")?;
        let prompt = require_str(params, "prompt")?;

        let agent = self
            .sessions
            .get_agent(sid)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        let _guard = self
            .sessions
            .session_queue
            .acquire(sid)
            .await
            .map_err(|e| rpc_err(SESSION_BUSY, format!("Session busy: {e}")))?;

        let cancel = tokio_util::sync::CancellationToken::new();
        self.sessions.register_cancel_token(sid, cancel.clone());
        self.sessions.touch(sid).await;

        let rpc = self.rpc.clone();
        let sid_owned = sid.to_string();
        let outcome = execute_turn(
            agent,
            prompt.to_string(),
            cancel,
            Some(format!("rpc_{sid}")),
            move |event| {
                let rpc = rpc.clone();
                let sid = sid_owned.clone();
                async move {
                    if let Some(n) = notification_for_turn_event(&sid, &event) {
                        let _ = rpc.send_raw(n).await;
                    }
                }
            },
        )
        .await;

        self.sessions.remove_cancel_token(sid);

        // Persist.
        if let Some(ref backend) = self.session_backend {
            let key = format!("rpc_{sid}");
            let _ = backend.append(&key, &ChatMessage::user(prompt));
            match &outcome {
                Ok(TurnOutcome::Completed { text, .. }) => {
                    let _ = backend.append(&key, &ChatMessage::assistant(text));
                }
                Ok(TurnOutcome::Cancelled { partial_text }) if !partial_text.is_empty() => {
                    let _ = backend.append(&key, &ChatMessage::assistant(partial_text));
                }
                _ => {}
            }
        }

        match outcome {
            Ok(TurnOutcome::Completed { text, .. }) => Ok(json!({
                "sessionId": sid,
                "stopReason": "end_turn",
                "content": text,
            })),
            Ok(TurnOutcome::Cancelled { partial_text }) => Ok(json!({
                "sessionId": sid,
                "stopReason": "cancelled",
                "content": partial_text,
            })),
            Err(e) => Err(rpc_err(INTERNAL_ERROR, e.to_string())),
        }
    }

    async fn handle_session_configure(&self, params: &Value) -> RpcResult {
        let sid = require_str(params, "sessionId")?;
        let patch: super::session::SessionOverrides =
            serde_json::from_value(params.get("overrides").cloned().unwrap_or_default())
                .map_err(|e| rpc_err(INVALID_PARAMS, format!("Invalid overrides: {e}")))?;

        let merged = self
            .sessions
            .set_overrides(sid, patch)
            .await
            .ok_or_else(|| rpc_err(SESSION_NOT_FOUND, "Session not found"))?;

        Ok(json!({
            "sessionId": sid,
            "overrides": merged,
        }))
    }

    fn handle_session_cancel(&self, params: &Value) -> RpcResult {
        let sid = require_str(params, "sessionId")?;
        if self.sessions.cancel_session(sid) {
            Ok(json!({ "sessionId": sid, "cancelled": true }))
        } else {
            Err(rpc_err(
                SESSION_NOT_FOUND,
                "No active turn for this session",
            ))
        }
    }

    async fn handle_status(&self) -> RpcResult {
        let ids = self.sessions.list_ids().await;
        Ok(json!({
            "serverVersion": env!("CARGO_PKG_VERSION"),
            "protocolVersion": RPC_PROTOCOL_VERSION,
            "activeSessions": ids.len(),
            "sessionIds": ids,
        }))
    }

    // ── Wire helpers ─────────────────────────────────────────────

    async fn send_result(&self, id: Value, result: Value) {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(result),
            error: None,
            id,
        };
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = self.rpc.send_raw(json).await;
        }
    }

    async fn send_error(&self, id: Value, code: i32, message: &str) {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
                data: None,
            }),
            id,
        };
        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = self.rpc.send_raw(json).await;
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn require_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, JsonRpcError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| rpc_err(INVALID_PARAMS, format!("{key} is required")))
}

fn notification_for_turn_event(session_id: &str, event: &TurnEvent) -> Option<String> {
    let params = match event {
        TurnEvent::Chunk { delta } => json!({
            "sessionId": session_id,
            "type": "agent_message_chunk",
            "text": delta,
        }),
        TurnEvent::Thinking { delta } => json!({
            "sessionId": session_id,
            "type": "agent_thought_chunk",
            "text": delta,
        }),
        TurnEvent::ToolCall { id, name, args } => json!({
            "sessionId": session_id,
            "type": "tool_call",
            "toolCallId": id,
            "name": name,
            "rawInput": args,
        }),
        TurnEvent::ToolResult { id, name, output } => json!({
            "sessionId": session_id,
            "type": "tool_result",
            "toolCallId": id,
            "name": name,
            "rawOutput": output,
        }),
        TurnEvent::ApprovalRequest {
            request_id,
            tool_name,
            arguments_summary,
            timeout_secs,
        } => json!({
            "sessionId": session_id,
            "type": "approval_request",
            "requestId": request_id,
            "toolName": tool_name,
            "argumentsSummary": arguments_summary,
            "timeoutSecs": timeout_secs,
        }),
        TurnEvent::Usage { .. } => return None,
    };

    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": params,
    }))
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn chunk_notification() {
        let event = TurnEvent::Chunk {
            delta: "hello".into(),
        };
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(v["method"], "session/update");
        assert_eq!(v["params"]["sessionId"], "s1");
        assert_eq!(v["params"]["type"], "agent_message_chunk");
        assert_eq!(v["params"]["text"], "hello");
    }

    #[test]
    fn thinking_notification() {
        let event = TurnEvent::Thinking {
            delta: "hmm".into(),
        };
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "agent_thought_chunk");
        assert_eq!(v["params"]["text"], "hmm");
    }

    #[test]
    fn tool_call_notification() {
        let event = TurnEvent::ToolCall {
            id: "tc_1".into(),
            name: "bash".into(),
            args: json!({"cmd": "ls"}),
        };
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "tool_call");
        assert_eq!(v["params"]["toolCallId"], "tc_1");
        assert_eq!(v["params"]["name"], "bash");
        assert_eq!(v["params"]["rawInput"]["cmd"], "ls");
    }

    #[test]
    fn tool_result_notification() {
        let event = TurnEvent::ToolResult {
            id: "tc_1".into(),
            name: "bash".into(),
            output: "file.txt".into(),
        };
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "tool_result");
        assert_eq!(v["params"]["toolCallId"], "tc_1");
        assert_eq!(v["params"]["rawOutput"], "file.txt");
    }

    #[test]
    fn approval_request_notification() {
        let event = TurnEvent::ApprovalRequest {
            request_id: "ar_1".into(),
            tool_name: "bash".into(),
            arguments_summary: "rm -rf /".into(),
            timeout_secs: 30,
        };
        let json = notification_for_turn_event("s1", &event).unwrap();
        let v = parse(&json);
        assert_eq!(v["params"]["type"], "approval_request");
        assert_eq!(v["params"]["requestId"], "ar_1");
        assert_eq!(v["params"]["toolName"], "bash");
        assert_eq!(v["params"]["timeoutSecs"], 30);
    }

    #[test]
    fn usage_event_returns_none() {
        let event = TurnEvent::Usage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            cost_usd: Some(0.01),
        };
        assert!(notification_for_turn_event("s1", &event).is_none());
    }

    #[test]
    fn require_str_present() {
        let v = json!({"foo": "bar"});
        assert_eq!(require_str(&v, "foo").unwrap(), "bar");
    }

    #[test]
    fn require_str_missing() {
        let v = json!({});
        let err = require_str(&v, "foo").unwrap_err();
        assert_eq!(err.code, INVALID_PARAMS);
        assert!(err.message.contains("foo"));
    }
}
