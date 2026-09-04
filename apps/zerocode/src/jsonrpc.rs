//! JSON-RPC 2.0 transport.
//!
//! `apps/zerocode` keeps local ownership of its transport plumbing.
//! Wire shape is the contract; if the daemon evolves its envelope, this file
//! evolves to match.
//!
//! `RpcOutbound` carries the writer channel + a pending-request map
//! so concurrent notifications and outbound calls cannot interleave
//! bytes. The TUI uses it both for client-issued requests
//! (`session/turn`, `quickstart/apply`, …) and for routing
//! daemon-originated notifications.
//!
//! Constants in `error_codes` cover the full set the daemon may emit
//! — some are only consumed by error-routing branches that may not
//! exercise every code today.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

// ── Protocol constants ───────────────────────────────────────────

pub const JSONRPC_VERSION: &str = "2.0";
pub const OUTBOUND_ID_PREFIX: &str = "zc-out-";

// ── Wire field name constants ────────────────────────────────────

pub mod field {
    pub const JSONRPC: &str = "jsonrpc";
    pub const METHOD: &str = "method";
    pub const PARAMS: &str = "params";
    pub const ID: &str = "id";
    pub const RESULT: &str = "result";
    pub const ERROR: &str = "error";
}

// ── Wire types ───────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(method: &str, params: Value, id: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.to_string(),
            params,
            id: Some(id),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: &'static str,
    pub params: Value,
}

impl JsonRpcNotification {
    pub fn new(method: &'static str, params: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION,
            method,
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ── Error codes ──────────────────────────────────────────────────

pub mod error_codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    pub const SESSION_NOT_FOUND: i32 = -32000;
    pub const SESSION_LIMIT_REACHED: i32 = -32001;
    pub const SESSION_BUSY: i32 = -32002;
    pub const AUTH_REQUIRED: i32 = -32010;
    pub const VERSION_MISMATCH: i32 = -32011;

    pub const FS_NOT_FOUND: i32 = 4001;
    pub const FS_PERMISSION_DENIED: i32 = 4002;
    pub const FS_INVALID_PATH: i32 = 4003;

    pub const FS_NOT_FOUND_STR: &str = "fs.not_found";
    pub const FS_PERMISSION_DENIED_STR: &str = "fs.permission_denied";
    pub const FS_INVALID_PATH_STR: &str = "fs.invalid_path";
}

pub const ACP_PROTOCOL_VERSION: u64 = 1;

// ── Outbound RPC plumbing ────────────────────────────────────────

type PendingResponder = oneshot::Sender<std::result::Result<Value, JsonRpcError>>;

#[derive(Debug, Default)]
struct OutboundState {
    pending: HashMap<String, PendingResponder>,
    closed_error: Option<JsonRpcError>,
}

/// Writer + outbound-call tracker shared between the read loop and
/// the calling tasks. All writes go through `writer_tx` so concurrent
/// notifications and outbound requests cannot interleave bytes.
#[derive(Debug)]
pub struct RpcOutbound {
    writer_tx: mpsc::Sender<String>,
    state: std::sync::Mutex<OutboundState>,
    next_id: AtomicU64,
}

struct PendingRequestGuard<'a> {
    state: &'a std::sync::Mutex<OutboundState>,
    id: String,
}

impl Drop for PendingRequestGuard<'_> {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(&self.id);
    }
}

impl RpcOutbound {
    pub fn new(writer_tx: mpsc::Sender<String>) -> Self {
        Self {
            writer_tx,
            state: std::sync::Mutex::new(OutboundState::default()),
            next_id: AtomicU64::new(0),
        }
    }

    pub async fn send_raw(&self, json: String) -> bool {
        self.writer_tx.send(json).await.is_ok()
    }

    /// Write a JSON-RPC response (success or error) keyed to a
    /// server-originated request id. Used by the TUI when the daemon
    /// invokes a method on us (e.g. `elicitation/create`) and we need
    /// to ship back an `Accept` / `Decline` / `Cancel`.
    fn response_body(
        id: Value,
        result: std::result::Result<Value, JsonRpcError>,
    ) -> std::result::Result<String, String> {
        let resp = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: result.as_ref().ok().cloned(),
            error: result.err(),
            id,
        };
        serde_json::to_string(&resp).map_err(|error| format!("failed to encode response: {error}"))
    }

    pub async fn respond(
        &self,
        id: Value,
        result: std::result::Result<Value, JsonRpcError>,
    ) -> std::result::Result<(), String> {
        let body = Self::response_body(id, result)?;
        self.writer_tx
            .send(body)
            .await
            .map_err(|_| "writer task is closed".to_string())
    }

    pub fn try_respond(
        &self,
        id: Value,
        result: std::result::Result<Value, JsonRpcError>,
    ) -> std::result::Result<(), String> {
        let body = Self::response_body(id, result)?;
        self.writer_tx.try_send(body).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => "writer queue is full".to_string(),
            mpsc::error::TrySendError::Closed(_) => "writer task is closed".to_string(),
        })
    }

    pub async fn notify(&self, method: &'static str, params: Value) {
        let n = JsonRpcNotification::new(method, params);
        if let Ok(s) = serde_json::to_string(&n) {
            let _ = self.writer_tx.send(s).await;
        }
    }

    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, JsonRpcError> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("{OUTBOUND_ID_PREFIX}{n}");
        let (tx, rx) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(error) = &state.closed_error {
                return Err(error.clone());
            }
            state.pending.insert(id.clone(), tx);
        }
        let _pending_guard = PendingRequestGuard {
            state: &self.state,
            id: id.clone(),
        };
        let req = JsonRpcRequest::new(method, params, Value::String(id));
        let body = match serde_json::to_string(&req) {
            Ok(s) => s,
            Err(e) => {
                return Err(JsonRpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("Failed to encode request: {e}"),
                    data: None,
                });
            }
        };
        if self.writer_tx.send(body).await.is_err() {
            return Err(JsonRpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "Writer task closed".to_string(),
                data: None,
            });
        }
        rx.await.unwrap_or_else(|_| {
            Err(JsonRpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "Outbound RPC dropped".to_string(),
                data: None,
            })
        })
    }

    pub fn dispatch_response(
        &self,
        id_str: &str,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) {
        let responder = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .remove(id_str);
        if let Some(tx) = responder {
            let payload = if let Some(err) = error {
                Err(err)
            } else {
                Ok(result.unwrap_or(Value::Null))
            };
            let _ = tx.send(payload);
        }
    }

    /// Close request admission and fail every request still awaiting a response
    /// when the owning transport terminates. Responders are woken after release.
    pub fn fail_pending(&self, message: &str) {
        let (responders, error) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let error = state
                .closed_error
                .get_or_insert_with(|| JsonRpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: message.to_string(),
                    data: None,
                })
                .clone();
            (std::mem::take(&mut state.pending), error)
        };
        for (_, responder) in responders {
            let _ = responder.send(Err(error.clone()));
        }
    }

    pub fn pending_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .len()
    }
}
