/// OpenClaw Gateway WebSocket Protocol (v3)
///
/// Implements the OpenClaw node protocol for connecting ZeroClaw instances
/// to an OpenClaw gateway as agent-delegation nodes.
///
/// Three frame types:
/// - EventFrame: type="event" (server → client)
/// - RequestFrame: type="req" (client → server)
/// - ResponseFrame: type="res" (server → client)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol version supported by this client
pub const PROTOCOL_VERSION: u32 = 3;

/// Heartbeat tick interval (milliseconds) — server sends these every 30s
pub const TICK_INTERVAL_MS: u64 = 30000;

/// Client should reconnect if no tick received within this multiple of the interval
pub const TICK_STALL_MULTIPLIER: u64 = 2;

/// Gateway policy limits
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayPolicy {
    #[serde(default = "default_max_payload")]
    pub max_payload: usize,
    #[serde(default = "default_max_buffered_bytes")]
    pub max_buffered_bytes: usize,
    #[serde(default = "default_tick_interval_ms")]
    pub tick_interval_ms: u64,
}

fn default_max_payload() -> usize {
    26214400 // 25 MiB
}

fn default_max_buffered_bytes() -> usize {
    52428800 // 50 MiB
}

fn default_tick_interval_ms() -> u64 {
    30000
}

/// All frames are JSON with a "type" field
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Frame {
    #[serde(rename = "event")]
    Event(EventFrame),
    #[serde(rename = "req")]
    Request(RequestFrame),
    #[serde(rename = "res")]
    Response(ResponseFrame),
}

/// Server → Client event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventFrame {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_version: Option<StateVersion>,
}

/// Client → Server request (RPC)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// Server → Client response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<::std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateVersion {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<u32>,
}

/// Challenge event from gateway (first message)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectChallenge {
    pub nonce: String,
    pub ts: u64,
}

/// Node device identity for authentication
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceAuth {
    pub id: String,
    #[serde(rename = "publicKey")]
    pub public_key: String, // base64url-encoded raw public key
    pub signature: String,  // base64url-encoded ed25519 signature
    #[serde(rename = "signedAt")]
    pub signed_at: u64, // milliseconds since epoch
    pub nonce: String,
}

/// Auth credentials in connect request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthCredentials {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// Client info for connect request
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub version: String,
    pub platform: String,
    pub mode: String, // "node"
    #[serde(rename = "instanceId")]
    pub instance_id: String,
}

/// Connect request parameters (sent to gateway on connect method)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectParams {
    pub min_protocol: u32,
    pub max_protocol: u32,
    pub client: ClientInfo,
    #[serde(default)]
    pub caps: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    pub role: String, // "node"
    #[serde(default)]
    pub scopes: Vec<String>,
    pub device: DeviceAuth,
    pub auth: AuthCredentials,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<HashMap<String, bool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_env: Option<String>,
}

/// HelloOk response from gateway
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloOk {
    #[serde(rename = "type")]
    pub msg_type: String, // "hello-ok"
    pub protocol: u32,
    pub server: ServerInfo,
    pub features: Features,
    pub snapshot: Snapshot,
    pub canvas_host_url: String,
    pub auth: AuthResponse,
    pub policy: GatewayPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    pub version: String,
    #[serde(rename = "connId")]
    pub conn_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Features {
    pub methods: Vec<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub presence: Vec<serde_json::Value>,
    pub health: HashMap<String, serde_json::Value>,
    #[serde(rename = "stateVersion")]
    pub state_version: StateVersion,
    #[serde(rename = "uptimeMs")]
    pub uptime_ms: u64,
    #[serde(rename = "authMode")]
    pub auth_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    #[serde(rename = "deviceToken")]
    pub device_token: String,
    pub role: String,
    pub scopes: Vec<String>,
    #[serde(rename = "issuedAtMs")]
    pub issued_at_ms: u64,
}

/// Node pairing request
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodePairRequest {
    pub node_id: String,
    pub display_name: String,
    pub platform: String,
    pub version: String,
    #[serde(default)]
    pub caps: Vec<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub silent: bool,
}

/// Node invocation request (sent by gateway to node)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInvokeRequest {
    pub id: String,
    pub node_id: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Node invocation result (sent by node to gateway)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeInvokeResultParams {
    pub id: String,
    pub node_id: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Tick (heartbeat) event from gateway
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    pub ts: u64, // milliseconds since epoch
}
