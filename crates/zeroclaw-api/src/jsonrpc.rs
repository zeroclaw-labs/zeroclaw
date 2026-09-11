//! Shared JSON-RPC 2.0 types for the ACP server and runtime RPC layer.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

// ── Protocol constants ───────────────────────────────────────────

/// JSON-RPC protocol version string. Used in every frame's `jsonrpc` field.
pub const JSONRPC_VERSION: &str = "2.0";

/// Prefix for locally originated outbound request IDs.
///
/// Both peers may use this prefix, so correlation is directional rather than
/// globally unique across the socket.
pub const OUTBOUND_ID_PREFIX: &str = "zc-out-";

// ── Wire field name constants ────────────────────────────────────
// Used when parsing raw `Value` frames (e.g. in the client read loop).

pub mod field {
    pub const JSONRPC: &str = "jsonrpc";
    pub const METHOD: &str = "method";
    pub const PARAMS: &str = "params";
    pub const ID: &str = "id";
    pub const RESULT: &str = "result";
    pub const ERROR: &str = "error";
}

// ── Wire types ───────────────────────────────────────────────────

/// A validated inbound JSON-RPC 2.0 frame.
#[derive(Debug)]
pub enum JsonRpcFrame {
    Request(JsonRpcRequest),
    Response {
        id: Value,
        result: Result<Value, JsonRpcError>,
    },
}

/// Direction inferred from the members of an invalid JSON-RPC envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonRpcFrameErrorKind {
    Request,
    Response,
    Ambiguous,
}

/// A semantic JSON-RPC envelope error that preserves its inferred direction.
#[derive(Debug)]
pub struct JsonRpcFrameError {
    kind: JsonRpcFrameErrorKind,
    request_id: Option<Value>,
    message: String,
}

impl JsonRpcFrameError {
    pub fn kind(&self) -> JsonRpcFrameErrorKind {
        self.kind
    }

    /// A valid ID may be echoed only when the invalid frame is request-shaped.
    pub fn request_id(&self) -> Option<&Value> {
        self.request_id.as_ref()
    }
}

impl fmt::Display for JsonRpcFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsonRpcFrameError {}

impl JsonRpcFrame {
    /// Validate a syntactically valid JSON value while retaining the direction
    /// needed by transports to handle malformed envelopes safely.
    pub fn from_value(mut value: Value) -> Result<Self, JsonRpcFrameError> {
        let (kind, request_id) = invalid_frame_context(&value);
        let invalid = |message: &str| JsonRpcFrameError {
            kind,
            request_id: request_id.clone(),
            message: message.to_string(),
        };

        let obj = value
            .as_object_mut()
            .ok_or_else(|| invalid("JsonRpcFrame must be a JSON object"))?;

        match obj.remove(field::JSONRPC) {
            Some(Value::String(version)) if version == JSONRPC_VERSION => {}
            Some(Value::String(_)) => return Err(invalid("jsonrpc field must equal 2.0")),
            Some(_) => return Err(invalid("jsonrpc field must be a string")),
            None => return Err(invalid("missing field jsonrpc")),
        }

        let method = obj.remove(field::METHOD);
        let params = obj.remove(field::PARAMS);
        let id = obj.remove(field::ID);
        let result = obj.remove(field::RESULT);
        let error = obj.remove(field::ERROR);

        if let Some(method) = method {
            let Value::String(method) = method else {
                return Err(invalid("method field must be a string"));
            };
            if result.is_some() || error.is_some() {
                return Err(invalid("request frames cannot contain result or error"));
            }
            if id.as_ref().is_some_and(|id| !is_valid_jsonrpc_id(id)) {
                return Err(invalid("id must be a string, number, or null"));
            }
            if params
                .as_ref()
                .is_some_and(|params| !params.is_object() && !params.is_array())
            {
                return Err(invalid("params must be an object or array when present"));
            }
            return Ok(Self::Request(JsonRpcRequest {
                jsonrpc: JSONRPC_VERSION.to_string(),
                method,
                params: params.unwrap_or_default(),
                id,
            }));
        }

        if params.is_some() {
            return Err(invalid("response frames cannot contain params"));
        }
        let id = id.ok_or_else(|| invalid("missing field id"))?;
        if !is_valid_jsonrpc_id(&id) {
            return Err(invalid("id must be a string, number, or null"));
        }

        match (result, error) {
            (Some(result), None) => Ok(Self::Response {
                id,
                result: Ok(result),
            }),
            (None, Some(error)) => Ok(Self::Response {
                id,
                result: Err(
                    serde_json::from_value(error).map_err(|error| invalid(&error.to_string()))?
                ),
            }),
            (Some(_), Some(_)) => Err(invalid(
                "response frames must not contain both result and error",
            )),
            (None, None) => Err(invalid(
                "frame must contain a method or exactly one response member",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for JsonRpcFrame {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        Self::from_value(Value::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

fn invalid_frame_context(value: &Value) -> (JsonRpcFrameErrorKind, Option<Value>) {
    let Some(obj) = value.as_object() else {
        return (JsonRpcFrameErrorKind::Ambiguous, None);
    };
    let has_response_member = obj.contains_key(field::RESULT) || obj.contains_key(field::ERROR);

    let kind = match (obj.get(field::METHOD), has_response_member) {
        (Some(Value::String(_)), false) => JsonRpcFrameErrorKind::Request,
        (None, true) => JsonRpcFrameErrorKind::Response,
        _ => JsonRpcFrameErrorKind::Ambiguous,
    };
    let request_id = (kind == JsonRpcFrameErrorKind::Request)
        .then(|| {
            obj.get(field::ID)
                .filter(|id| is_valid_jsonrpc_id(id))
                .cloned()
        })
        .flatten();
    (kind, request_id)
}

fn is_valid_jsonrpc_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_) | Value::Null)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: Option<Value>,
}

impl JsonRpcRequest {
    /// Build a request with an auto-incremented numeric id.
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
    // Standard JSON-RPC 2.0
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;

    // ZeroClaw custom
    pub const SESSION_NOT_FOUND: i32 = -32000;
    pub const SESSION_LIMIT_REACHED: i32 = -32001;
    pub const SESSION_BUSY: i32 = -32002;
    pub const SESSION_NOT_OWNED: i32 = -32003;
    pub const AUTH_REQUIRED: i32 = -32010;
    pub const VERSION_MISMATCH: i32 = -32011;

    // SOP authoring
    pub const SOP_ALREADY_EXISTS: i32 = -32020;
    pub const SOP_NOT_FOUND: i32 = -32021;
    // Filesystem RPC errors (internal numeric codes; wire uses string codes e.g. "fs.not_found")
    pub const FS_NOT_FOUND: i32 = 4001;
    pub const FS_PERMISSION_DENIED: i32 = 4002;
    pub const FS_INVALID_PATH: i32 = 4003;

    // String error codes for fs.* methods
    pub const FS_NOT_FOUND_STR: &str = "fs.not_found";
    pub const FS_PERMISSION_DENIED_STR: &str = "fs.permission_denied";
    pub const FS_INVALID_PATH_STR: &str = "fs.invalid_path";
}

pub const ACP_PROTOCOL_VERSION: u64 = 1;

// ── Outbound RPC plumbing ────────────────────────────────────────

type PendingResponder = oneshot::Sender<std::result::Result<Value, JsonRpcError>>;

#[derive(Debug)]
pub struct RpcOutbound {
    writer_tx: mpsc::Sender<String>,
    pending: std::sync::Mutex<HashMap<String, PendingResponder>>,
    next_id: AtomicU64,
}

struct PendingRequestGuard<'a> {
    pending: &'a std::sync::Mutex<HashMap<String, PendingResponder>>,
    id: String,
}

impl Drop for PendingRequestGuard<'_> {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl RpcOutbound {
    pub fn new(writer_tx: mpsc::Sender<String>) -> Self {
        Self {
            writer_tx,
            pending: std::sync::Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }

    /// Send a raw pre-serialized JSON line. Returns `true` on success.
    pub async fn send_raw(&self, json: String) -> bool {
        self.writer_tx.send(json).await.is_ok()
    }

    /// Resolve when the writer end is closed (peer dropped). Useful for
    /// long-lived forwarders that need to exit on disconnect even when
    /// there is no payload to send.
    pub async fn closed(&self) {
        self.writer_tx.closed().await;
    }

    /// Send a JSON-RPC notification (no `id`, no response expected).
    pub async fn notify(&self, method: &'static str, params: Value) {
        let n = JsonRpcNotification::new(method, params);
        if let Ok(s) = serde_json::to_string(&n) {
            let _ = self.writer_tx.send(s).await;
        }
    }

    /// Send a JSON-RPC request and await the response.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, JsonRpcError> {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("{OUTBOUND_ID_PREFIX}{n}");
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.insert(id.clone(), tx);
        }
        let _pending_guard = PendingRequestGuard {
            pending: &self.pending,
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

    /// Route an already validated JSON-RPC response to its pending caller.
    /// Returns whether a matching pending request existed.
    pub fn dispatch_validated_response(
        &self,
        id_str: &str,
        result: std::result::Result<Value, JsonRpcError>,
    ) -> bool {
        let responder = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id_str);
        let Some(tx) = responder else {
            return false;
        };
        let _ = tx.send(result);
        true
    }

    /// Route a response assembled by an internal caller.
    pub fn dispatch_response(
        &self,
        id_str: &str,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) {
        let result = error.map_or_else(|| Ok(result.unwrap_or(Value::Null)), Err);
        let _ = self.dispatch_validated_response(id_str, result);
    }

    /// Number of in-flight outbound requests awaiting responses.
    pub fn pending_count(&self) -> usize {
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

// ── Locale RPC types ─────────────────────────────────────────────

/// One selectable locale from the build's embedded `locales.toml` registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocaleOption {
    pub code: String,
    pub label: String,
}

/// Response for `locales/list` — the in-memory locale registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalesListResponse {
    pub locales: Vec<LocaleOption>,
}

/// Request payload for `locales/fetch`. `catalog` restricts which catalogues
/// are downloaded; `None`/empty means all. The daemon validates `locale`
/// against the embedded registry and `catalog` against the fixed catalog set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalesFetchRequest {
    pub locale: String,
    #[serde(default)]
    pub catalog: Vec<String>,
}

/// One fetched catalogue's bytes, returned over the wire so the client writes
/// them into its own config dir (keeping the write in the caller's permission
/// scope).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchedCatalog {
    pub name: String,
    /// Output filename (e.g. `cli.ftl`).
    pub filename: String,
    /// The FTL file contents.
    pub content: String,
}

/// Response for `locales/fetch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalesFetchResponse {
    pub locale: String,
    pub catalogs: Vec<FetchedCatalog>,
    /// Catalogue names that had no file on upstream and were skipped.
    pub skipped: Vec<String>,
}

// ── SOP authoring RPC types ──────────────────────────────────────

/// Request payload for SOP read/delete methods that select one SOP by name:
/// `sops/get`, `sops/graph`, `sops/validate` (by name), `sops/delete`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopSelectRequest {
    pub name: String,
}

/// Request payload for `sops/run-overlay`: project a run's state onto a SOP's
/// graph. Selects the SOP by `name` and the run by `run_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRunOverlayRequest {
    pub name: String,
    pub run_id: String,
}

/// Request payload for `sops/decide`: resolve a paused checkpoint. `name` and
/// `run_id` select the run; `decision` is the raw `ApprovalDecision` wire value
/// (`"approve"` or `{"deny": {"reason": "..."}}`), deserialized into the
/// canonical runtime enum by the handler so no parallel decision enum exists
/// here to drift from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopDecideRequest {
    pub name: String,
    pub run_id: String,
    pub decision: serde_json::Value,
}

/// Request payload for `sops/run`: fire a Manual trigger for the named SOP.
/// `payload` is an optional JSON string handed to the run as the step-1 input;
/// omitting it starts the run with no payload. The daemon builds the Manual
/// `SopEvent` and dispatches it on the same path as the `sop_execute` tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRunRequest {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Optional semantic work-item key shared with another trigger producer.
    /// Dispatch coalesces matching keys only while the first run remains active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
}

/// Response payload for `sops/run`: the id of the run that was started, which
/// feeds straight into `sops/run-overlay` to animate the run on the canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopRunResponse {
    pub run_id: String,
}

/// Request payload for `sops/runs`: enumerate runs the engine currently holds
/// (active plus retained terminal), newest first. `sop` optionally scopes the
/// listing to a single SOP by name; omitting it lists every SOP's runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SopRunsRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sop: Option<String>,
}

/// Request payload for `sops/save` and `sops/create`. The `sop` field is the
/// wire form of the runtime `Sop`; the daemon deserializes and validates it.
/// `sops/validate` also accepts this form to validate an unsaved draft.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SopSaveRequest {
    pub sop: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
}

/// Request payload for `fs.list_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsListDirRequest {
    /// Relative or absolute path within the agent workspace.
    pub path: String,
    #[serde(default)]
    pub show_hidden: bool,
}

/// Response for `fs.list_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsListDirResponse {
    pub entries: Vec<FsEntry>,
    pub cwd: String,
}

/// A single directory entry returned by `fs.list_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsEntry {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

/// Filesystem stat result (success case). Matches FsEntry shape with extra fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsStatResult {
    pub name: String,
    pub full_path: String,
    pub is_dir: bool,
    pub is_hidden: bool,
    pub size: u64,
    pub mtime: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Filesystem stat error payload (used inside `JsonRpcError.data`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FsStatError {
    pub path: String,
    pub code: &'static str, // e.g. "fs.not_found"
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn frame_parses_success_response() {
        let json = r#"{"jsonrpc":"2.0","id":"zc-out-5","result":{"answer":42}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Response { id, result } = frame else {
            panic!("expected response");
        };
        assert_eq!(id, json!("zc-out-5"));
        assert_eq!(result.unwrap(), json!({"answer": 42}));
    }

    #[test]
    fn frame_parses_error_response() {
        let json =
            r#"{"jsonrpc":"2.0","id":"zc-out-3","error":{"code":-32601,"message":"not found"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Response { id, result } = frame else {
            panic!("expected response");
        };
        assert_eq!(id, json!("zc-out-3"));
        assert_eq!(result.unwrap_err().code, -32601);
    }

    #[test]
    fn frame_parses_request_with_string_id() {
        let json = r#"{"jsonrpc":"2.0","method":"ping","params":{"v":1},"id":"client-1"}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Request(req) = frame else {
            panic!("expected request");
        };
        assert_eq!(req.method, "ping");
        assert_eq!(req.params, json!({"v": 1}));
        assert_eq!(req.id, Some(json!("client-1")));
    }

    #[test]
    fn frame_parses_request_with_numeric_id() {
        // Numeric IDs are valid per JSON-RPC 2.0 and must not be silently dropped.
        let json = r#"{"jsonrpc":"2.0","method":"ping","id":42}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Request(req) = frame else {
            panic!("expected request");
        };
        assert_eq!(req.method, "ping");
        assert_eq!(req.id, Some(json!(42)));
    }

    #[test]
    fn frame_parses_notification_without_id() {
        let json = r#"{"jsonrpc":"2.0","method":"log","params":{"msg":"hello"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Request(req) = frame else {
            panic!("expected notification");
        };
        assert_eq!(req.method, "log");
        assert_eq!(req.params, json!({"msg": "hello"}));
        assert!(req.id.is_none());
    }

    #[test]
    fn frame_parses_request_without_params() {
        let json = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Request(req) = frame else {
            panic!("expected request");
        };
        assert_eq!(req.method, "ping");
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn frame_explicit_null_result_is_valid_response() {
        let json = r#"{"jsonrpc":"2.0","id":"x","result":null}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let JsonRpcFrame::Response { result, .. } = frame else {
            panic!("expected response");
        };
        assert_eq!(result.unwrap(), Value::Null);
    }

    #[test]
    fn frame_missing_jsonrpc_field_fails_deserialization() {
        // jsonrpc field has no serde(default) so it must be present.
        let json = r#"{"method":"ping","id":1}"#;
        let result: Result<JsonRpcFrame, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn frame_rejects_invalid_envelopes() {
        let invalid = [
            r#"{"jsonrpc":"1.0","method":"ping","id":1}"#,
            r#"{"jsonrpc":"2.0"}"#,
            r#"{"jsonrpc":"2.0","method":null,"id":1}"#,
            r#"{"jsonrpc":"2.0","method":"ping","id":1,"result":true}"#,
            r#"{"jsonrpc":"2.0","method":"ping","params":true,"id":1}"#,
            r#"{"jsonrpc":"2.0","method":"ping","params":null,"id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":true,"error":{"code":-1,"message":"bad"}}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","result":true}"#,
            r#"{"jsonrpc":"2.0","id":{"nested":1},"result":true}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":true,"params":{}}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":"bad","message":"bad"}}"#,
        ];

        for json in invalid {
            assert!(
                serde_json::from_str::<JsonRpcFrame>(json).is_err(),
                "invalid frame accepted: {json}"
            );
        }
    }

    #[test]
    fn invalid_frame_errors_preserve_direction_and_safe_request_id() {
        let cases = [
            (Value::Null, JsonRpcFrameErrorKind::Ambiguous, None),
            (
                json!({"jsonrpc":"2.0","method":null,"id":7}),
                JsonRpcFrameErrorKind::Ambiguous,
                None,
            ),
            (
                json!({"jsonrpc":"2.0","method":"status","params":null}),
                JsonRpcFrameErrorKind::Request,
                None,
            ),
            (
                json!({"jsonrpc":"2.0","method":null,"id":null}),
                JsonRpcFrameErrorKind::Ambiguous,
                None,
            ),
            (
                json!({"jsonrpc":"2.0","method":null,"id":7,"result":true}),
                JsonRpcFrameErrorKind::Ambiguous,
                None,
            ),
            (
                json!({
                    "jsonrpc":"2.0",
                    "id":"zc-out-0",
                    "result":true,
                    "error":{"code":-1,"message":"bad"}
                }),
                JsonRpcFrameErrorKind::Response,
                None,
            ),
            (
                json!({"id":"zc-out-0","result":true}),
                JsonRpcFrameErrorKind::Response,
                None,
            ),
            (
                json!({"jsonrpc":"1.0","id":"zc-out-0","result":true}),
                JsonRpcFrameErrorKind::Response,
                None,
            ),
            (
                json!({
                    "jsonrpc":"2.0",
                    "id":null,
                    "result":true,
                    "error":{"code":-1,"message":"bad"}
                }),
                JsonRpcFrameErrorKind::Response,
                None,
            ),
            (
                json!({"jsonrpc":"2.0","method":"status","id":9,"result":true}),
                JsonRpcFrameErrorKind::Ambiguous,
                None,
            ),
            (
                json!({"jsonrpc":"2.0","id":11}),
                JsonRpcFrameErrorKind::Ambiguous,
                None,
            ),
        ];

        for (value, kind, request_id) in cases {
            let error = JsonRpcFrame::from_value(value).expect_err("invalid frame");
            assert_eq!(error.kind(), kind);
            assert_eq!(error.request_id(), request_id.as_ref());
        }
    }

    #[test]
    fn request_new_sets_version_and_wraps_id() {
        let req = JsonRpcRequest::new("ping", json!({"x": 1}), json!(7));
        assert_eq!(req.jsonrpc, JSONRPC_VERSION);
        assert_eq!(req.method, "ping");
        assert_eq!(req.params, json!({"x": 1}));
        assert_eq!(req.id, Some(json!(7)));
    }

    #[test]
    fn request_deserializes_with_default_params_when_omitted() {
        let req: JsonRpcRequest =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"m","id":1}"#).unwrap();
        assert_eq!(req.method, "m");
        assert_eq!(req.params, Value::Null);
        assert_eq!(req.id, Some(json!(1)));
    }

    #[test]
    fn request_rejects_missing_method() {
        assert!(serde_json::from_str::<JsonRpcRequest>(r#"{"jsonrpc":"2.0","id":1}"#).is_err());
    }

    #[test]
    fn notification_new_sets_version_and_carries_no_id() {
        let n = JsonRpcNotification::new("event", json!([1, 2]));
        assert_eq!(n.jsonrpc, JSONRPC_VERSION);
        assert_eq!(n.method, "event");
        let v = serde_json::to_value(&n).unwrap();
        assert!(v.get("id").is_none(), "notifications carry no id");
        assert_eq!(v["jsonrpc"].as_str(), Some(JSONRPC_VERSION));
    }

    #[test]
    fn response_omits_none_result_and_error() {
        let ok = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: Some(json!("ok")),
            error: None,
            id: json!(1),
        };
        let v = serde_json::to_value(&ok).unwrap();
        assert_eq!(v["result"], json!("ok"));
        assert!(v.get("error").is_none(), "error omitted when None");

        let err = JsonRpcResponse {
            jsonrpc: JSONRPC_VERSION,
            result: None,
            error: Some(JsonRpcError {
                code: error_codes::METHOD_NOT_FOUND,
                message: "nope".to_string(),
                data: None,
            }),
            id: json!(2),
        };
        let v = serde_json::to_value(&err).unwrap();
        assert!(v.get("result").is_none(), "result omitted when None");
        assert_eq!(
            v["error"]["code"].as_i64(),
            Some(error_codes::METHOD_NOT_FOUND as i64)
        );
        assert!(
            v["error"].get("data").is_none(),
            "error.data omitted when None"
        );
    }

    #[test]
    fn standard_error_codes_match_jsonrpc_spec() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    #[test]
    fn error_roundtrips_through_serde() {
        let e = JsonRpcError {
            code: error_codes::INVALID_PARAMS,
            message: "bad".to_string(),
            data: Some(json!({"field": "x"})),
        };
        let s = serde_json::to_string(&e).unwrap();
        let back: JsonRpcError = serde_json::from_str(&s).unwrap();
        assert_eq!(back.code, error_codes::INVALID_PARAMS);
        assert_eq!(back.message, "bad");
        assert_eq!(back.data, Some(json!({"field": "x"})));
    }
}
