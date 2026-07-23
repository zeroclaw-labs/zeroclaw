//! Shared JSON-RPC 2.0 types for the ACP server and runtime RPC layer.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot};

// ── Protocol constants ───────────────────────────────────────────

/// JSON-RPC protocol version string. Used in every frame's `jsonrpc` field.
pub const JSONRPC_VERSION: &str = "2.0";

/// Prefix for server-originated outbound request IDs, disjoint from any
/// client-issued id space.
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

/// A JSON-RPC 2.0 frame that can represent either a request or response.
/// Used for deserializing bidirectional RPC traffic where incoming frames
/// may be responses to our outbound requests.
///
/// The custom Deserialize impl preserves explicit `result: null` as `Some(Value::Null)`
/// so valid JSON-RPC 2.0 success responses are not silently dropped.
#[derive(Debug)]
pub struct JsonRpcFrame {
    pub jsonrpc: String,
    pub method: Option<String>,
    pub params: Option<Value>,
    pub id: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

impl<'de> Deserialize<'de> for JsonRpcFrame {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let mut value = Value::deserialize(deserializer)?;
        let obj = value
            .as_object_mut()
            .ok_or_else(|| D::Error::custom("JsonRpcFrame must be a JSON object"))?;

        let jsonrpc = match obj.remove(field::JSONRPC) {
            Some(Value::String(s)) => s,
            Some(_) => return Err(D::Error::custom("jsonrpc field must be a string")),
            None => return Err(D::Error::missing_field(field::JSONRPC)),
        };

        let method = match obj.remove(field::METHOD) {
            Some(Value::String(s)) => Some(s),
            Some(Value::Null) | None => None,
            Some(_) => return Err(D::Error::custom("method field must be a string")),
        };

        let params = obj.remove(field::PARAMS);
        let id = obj.remove(field::ID);

        // Result: remove field; explicit null is preserved as Some(Value::Null).
        let result = obj.remove(field::RESULT);

        // Error: remove field, attempt deserialization, transpose Option<Result> to Result<Option>.
        let error = obj
            .remove(field::ERROR)
            .map(serde_json::from_value::<JsonRpcError>)
            .transpose()
            .map_err(D::Error::custom)?;

        Ok(JsonRpcFrame {
            jsonrpc,
            method,
            params,
            id,
            result,
            error,
        })
    }
}

impl JsonRpcFrame {
    /// Whether this frame is a response to an outbound request: no method,
    /// and a `result` or `error` field was present (including explicit null).
    pub fn is_response(&self) -> bool {
        self.method.is_none() && (self.result.is_some() || self.error.is_some())
    }

    /// The response result value when present (including explicit null),
    /// otherwise None.
    pub fn response_result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    /// The request method (for incoming client requests).
    pub fn request_method(&self) -> Option<&str> {
        self.method.as_deref()
    }

    /// The request params (for incoming client requests).
    pub fn request_params(&self) -> Option<&Value> {
        self.params.as_ref()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    #[serde(default)]
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

    /// Route an inbound JSON-RPC response to its pending caller.
    pub fn dispatch_response(
        &self,
        id_str: &str,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) {
        let responder = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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

        assert!(frame.is_response());
        assert_eq!(frame.response_result(), Some(&json!({"answer": 42})));
        assert!(frame.request_method().is_none());
    }

    #[test]
    fn frame_parses_error_response() {
        let json =
            r#"{"jsonrpc":"2.0","id":"zc-out-3","error":{"code":-32601,"message":"not found"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(frame.is_response());
        assert!(frame.response_result().is_none());
        assert!(frame.error.is_some());
        assert_eq!(frame.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn frame_parses_request_with_string_id() {
        let json = r#"{"jsonrpc":"2.0","method":"ping","params":{"v":1},"id":"client-1"}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(!frame.is_response());
        assert_eq!(frame.request_method(), Some("ping"));
        assert_eq!(frame.request_params(), Some(&json!({"v": 1})));
        assert_eq!(frame.id, Some(json!("client-1")));
    }

    #[test]
    fn frame_parses_request_with_numeric_id() {
        // Numeric IDs are valid per JSON-RPC 2.0 and must not be silently dropped.
        let json = r#"{"jsonrpc":"2.0","method":"ping","id":42}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(!frame.is_response());
        assert_eq!(frame.request_method(), Some("ping"));
        assert_eq!(frame.id, Some(json!(42)));
    }

    #[test]
    fn frame_parses_notification_without_id() {
        let json = r#"{"jsonrpc":"2.0","method":"log","params":{"msg":"hello"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(!frame.is_response());
        assert_eq!(frame.request_method(), Some("log"));
        assert!(frame.id.is_none());
    }

    #[test]
    fn frame_parses_request_without_params() {
        let json = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(!frame.is_response());
        assert_eq!(frame.request_method(), Some("ping"));
        assert!(frame.request_params().is_none());
    }

    #[test]
    fn frame_explicit_null_result_is_valid_response() {
        let json = r#"{"jsonrpc":"2.0","id":"x","result":null}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(frame.result.is_some());
        assert_eq!(frame.result, Some(Value::Null));
        assert!(frame.is_response());
        assert_eq!(frame.response_result(), Some(&Value::Null));
    }

    #[test]
    fn frame_empty_method_no_result_not_response() {
        // No method, no result, no error => not a response.
        let json = r#"{"jsonrpc":"2.0"}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(!frame.is_response());
    }

    #[test]
    fn frame_string_id_to_string_roundtrips() {
        // Verify the dispatch path: string IDs survive to_string().
        let json = r#"{"jsonrpc":"2.0","id":"zc-out-7","result":true}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        let id_str = frame.id.as_ref().unwrap().as_str().unwrap().to_string();
        assert_eq!(id_str, "zc-out-7");
    }

    #[test]
    fn frame_numeric_id_to_string_for_dispatch() {
        // Verify the dispatch path: numeric IDs convert via to_string().
        let json = r#"{"jsonrpc":"2.0","id":99,"result":"done"}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        // This is what dispatch.rs does for non-string IDs:
        let id_str = match frame.id.as_ref().unwrap() {
            Value::String(s) => s.clone(),
            _ => frame.id.as_ref().unwrap().to_string(),
        };
        assert_eq!(id_str, "99");
    }

    #[test]
    fn frame_response_result_returns_none_for_error_responses() {
        let json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32700,"message":"parse error"}}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(frame.is_response());
        // response_result should be None even though is_response() is true
        assert!(frame.response_result().is_none());
    }

    #[test]
    fn frame_missing_jsonrpc_field_fails_deserialization() {
        // jsonrpc field has no serde(default) so it must be present.
        let json = r#"{"method":"ping","id":1}"#;
        let result: Result<JsonRpcFrame, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn frame_result_null_is_detected_as_response() {
        let json = r#"{"jsonrpc":"2.0","id":"k","result":null}"#;
        let frame: JsonRpcFrame = serde_json::from_str(json).unwrap();

        assert!(frame.is_response());
        assert!(frame.result.is_some());
        assert_eq!(frame.result, Some(Value::Null));
        assert_eq!(frame.response_result(), Some(&Value::Null));
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
