//! Shared A2A v1.0 wire types for both inbound and outbound.
//!
//! Pure-Serde protocol DTOs for the A2A (Agent2Agent) v1.0 protobuf-JSON
//! payload. Inbound (`zeroclaw-gateway/src/a2a.rs`) uses these to construct
//! responses; outbound (`zeroclaw-tools/src/a2a_client.rs`) uses them to
//! deserialize peer responses. Router/server-only types stay gateway-local.
//!
//! This follows the established precedent of [`crate::jsonrpc`]: a
//! dependency-light, pure-Serde wire-model source shared across crates
//! without cross-crate coupling. No `a2a-rs`/protobuf footprint.
//!
//! `JsonSchema` derives are gated behind the `schema-export` feature (mirrors
//! `zeroclaw-config`), so consuming crates can generate OpenAPI/OPTIONS
//! schemas from these types directly.

#[cfg(feature = "schema-export")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A2A `AgentInterface` — a declared transport interface (spec §4.4). The
/// first entry of `supportedInterfaces` is the preferred transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: String,
    pub protocol_version: String,
}

/// A2A `AgentCapabilities` — optional feature flags. Only `Some` values
/// serialize; `None` fields are omitted.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

/// A2A `AgentSkill` (spec §4.4). `id`/`name`/`description` are spec-required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// A2A `AgentCard` (spec §4.4) — the discovery surface for an agent.
/// Serializes to the protobuf-JSON wire shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub supported_interfaces: Vec<AgentInterface>,
    pub version: String,
    #[serde(default)]
    pub capabilities: AgentCapabilities,
    #[serde(default)]
    pub default_input_modes: Vec<String>,
    #[serde(default)]
    pub default_output_modes: Vec<String>,
    #[serde(default)]
    pub skills: Vec<AgentSkill>,
}

/// A2A `TextPart` — a text payload part. Only `text` parts are handled for
/// the MVP; other part kinds (`data`, `file`, ...) deserialize but their
/// payload is ignored, matching the inbound artifact symmetry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TextPart {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
}

/// A2A `Artifact` — a single artifact carried by a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    #[serde(default)]
    pub artifact_id: String,
    #[serde(default)]
    pub parts: Vec<TextPart>,
}

/// A2A `TaskState` (spec §4.1.3) — task lifecycle state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    #[serde(default)]
    pub state: String,
}

/// A2A `Task` (spec §4.1.3) — returned by `message/send` / `tasks/get` /
/// `tasks/cancel`. Fields beyond the spec-required `id`/`status` are
/// optional and tolerate absence: a peer that omits `contextId` or
/// `artifacts` still parses. `kind` defaults to the spec's `"task"`
/// discriminator.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub context_id: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub kind: String,
}

/// A2A `MessageSendParams` (spec §3.1.1) — `message/send` params.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct MessageSendParams {
    pub message: Message,
}

/// A2A `Message` — a message payload sent to an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub parts: Vec<TextPart>,
}

/// A2A JSON-RPC 2.0 request envelope (spec §3.1).
#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct JsonRpcRequest {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: serde_json::Value,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A2A JSON-RPC 2.0 response envelope. Peers wrap the `Task` (or
/// card-shaped value) in `result`; errors come back as `error` with the
/// spec's negative codes.
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse<T> {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: serde_json::Value,
    #[serde(default)]
    pub result: Option<T>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

/// A2A JSON-RPC error object.
#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// Unwrap a JSON-RPC response into its result, or surface the error object.
pub fn rpc_result<T>(resp: JsonRpcResponse<T>) -> anyhow::Result<T> {
    if let Some(err) = resp.error {
        anyhow::bail!("A2A JSON-RPC error code {}: {}", err.code, err.message);
    }
    resp.result
        .ok_or_else(|| anyhow::Error::msg("A2A JSON-RPC response has neither result nor error"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_deserializes_minimal_peer_payload() {
        let payload = serde_json::json!({
            "id": "task-1",
            "contextId": "ctx-1",
            "status": { "state": "completed" },
            "artifacts": [
                { "artifactId": "art-1", "parts": [ { "kind": "text", "text": "done" } ] }
            ],
            "kind": "task"
        });
        let task: Task = serde_json::from_value(payload).unwrap();
        assert_eq!(task.id, "task-1");
        assert_eq!(task.context_id, "ctx-1");
        assert_eq!(task.status.state, "completed");
        assert_eq!(task.artifacts[0].parts[0].text, "done");
    }

    #[test]
    fn task_tolerates_missing_optional_fields() {
        let payload = serde_json::json!({ "id": "x", "status": { "state": "working" } });
        let task: Task = serde_json::from_value(payload).unwrap();
        assert_eq!(task.id, "x");
        assert_eq!(task.status.state, "working");
        assert!(task.context_id.is_empty());
        assert!(task.artifacts.is_empty());
    }

    #[test]
    fn agent_card_round_trips() {
        let card = AgentCard {
            name: "alpha".into(),
            description: "test agent".into(),
            supported_interfaces: vec![],
            version: "1.0".into(),
            capabilities: AgentCapabilities::default(),
            default_input_modes: vec!["text".into()],
            default_output_modes: vec!["text".into()],
            skills: vec![AgentSkill {
                id: "s1".into(),
                name: "deploy".into(),
                description: "deploys".into(),
                tags: vec!["prod".into()],
            }],
        };
        let json = serde_json::to_string(&card).unwrap();
        let back: AgentCard = serde_json::from_str(&json).unwrap();
        assert_eq!(card, back);
        // camelCase wire shape
        assert!(json.contains("\"supportedInterfaces\""));
        assert!(json.contains("\"defaultInputModes\""));
    }

    #[test]
    fn jsonrpc_response_surfaces_error() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32601, "message": "Method not found" }
        });
        let resp: JsonRpcResponse<Task> = serde_json::from_value(payload).unwrap();
        assert!(rpc_result(resp).is_err());
    }

    #[test]
    fn jsonrpc_response_unwraps_result() {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "id": "task-9", "status": { "state": "completed" } }
        });
        let resp: JsonRpcResponse<Task> = serde_json::from_value(payload).unwrap();
        let task = rpc_result(resp).unwrap();
        assert_eq!(task.id, "task-9");
    }

    #[test]
    fn message_send_params_serializes_camel_case() {
        let params = MessageSendParams {
            message: Message {
                role: "user".into(),
                parts: vec![TextPart {
                    kind: "text".into(),
                    text: "hello".into(),
                }],
            },
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"message\""));
        assert!(json.contains("\"role\":\"user\""));
    }
}
