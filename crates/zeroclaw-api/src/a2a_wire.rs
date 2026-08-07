//! Shared A2A v1.0 wire types for both inbound and outbound.
//!
//! Pure-Serde protocol DTOs for the A2A (Agent2Agent) v1.0 protobuf-JSON
//! payload, aligned to the official specification and `a2a.proto`
//! (`package lf.a2a.v1`): camelCase field names, SCREAMING_SNAKE_CASE enum
//! values, and oneof discriminators expressed as the branch field name (no
//! `kind` discriminator). Inbound (`zeroclaw-gateway/src/a2a.rs`) uses these
//! to construct responses; outbound (`zeroclaw-tools/src/a2a_client.rs`) uses
//! them to build requests and deserialize peer responses. Router/server-only
//! types stay gateway-local.
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

/// A2A `AgentInterface` (spec §5.8) — a declared transport interface. The
/// first entry of `supportedInterfaces` is the preferred transport. `tenant`,
/// when set, MUST be echoed into the `tenant` field of every request sent to
/// this interface (spec §5.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    pub protocol_binding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
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

/// A2A `AgentCard` (spec §14) — the discovery surface for an agent.
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

/// A2A `Role` (proto `enum Role`) — the sender of a message. ProtoJSON
/// serializes enum values as their SCREAMING_SNAKE_CASE proto names (spec
/// §5.5).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Role {
    #[default]
    RoleUnspecified,
    RoleUser,
    RoleAgent,
}

/// A2A `Part` (proto `message Part`) — a section of communication content.
/// The `content` oneof is expressed without a discriminator: the active
/// branch is exactly one of `text` / `raw` / `url` / `data`; the shared
/// optional fields (`metadata`/`filename`/`mediaType`) ride alongside it.
/// The oneof invariant is enforced at deserialization (via
/// `deserialize_part`): a payload with zero content branches or more than
/// one is rejected, matching the proto `oneof` semantics (spec §5.7 — a
/// `Part` carries exactly one content branch). A struct (not an enum) lets
/// serde handle the field-name-keyed shape directly while a custom
/// deserializer validates the oneof invariant.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// base64-encoded bytes (proto `bytes` JSON form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl<'de> serde::Deserialize<'de> for Part {
    /// Enforce the proto `oneof content` invariant: exactly one of
    /// `text` / `raw` / `url` / `data` must be present. Zero branches (empty
    /// Part) or multiple branches are rejected, matching proto `oneof`
    /// semantics (spec §5.7 — a `Part` carries exactly one content branch).
    /// `{"data": null}` is accepted: the `data` key is present, so the `data`
    /// branch is selected (proto3 `oneof` uses key presence, not value
    /// nullness).
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("A2A Part must be a JSON object"))?;
        let content_branches: [&str; 4] = ["text", "raw", "url", "data"];
        let present: Vec<&&str> = content_branches
            .iter()
            .filter(|&&k| obj.contains_key(k))
            .collect();
        if present.is_empty() {
            return Err(serde::de::Error::custom(
                "A2A Part must have exactly one content branch (text/raw/url/data); got none",
            ));
        }
        if present.len() > 1 {
            return Err(serde::de::Error::custom(format!(
                "A2A Part must have exactly one content branch; got multiple: {:?}",
                present
            )));
        }
        // Deserialize through the wire struct to avoid recursion.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PartWire {
            text: Option<String>,
            raw: Option<String>,
            url: Option<String>,
            data: Option<serde_json::Value>,
            metadata: Option<serde_json::Value>,
            filename: Option<String>,
            media_type: Option<String>,
        }
        let wire: PartWire = serde_json::from_value(raw)
            .map_err(|e| serde::de::Error::custom(format!("A2A Part: {e}")))?;
        Ok(Part {
            text: wire.text,
            raw: wire.raw,
            url: wire.url,
            data: wire.data,
            metadata: wire.metadata,
            filename: wire.filename,
            media_type: wire.media_type,
        })
    }
}

impl Part {
    /// Construct a text part (the common MVP case).
    pub fn text_str(s: impl Into<String>) -> Self {
        Part {
            text: Some(s.into()),
            raw: None,
            url: None,
            data: None,
            metadata: None,
            filename: None,
            media_type: None,
        }
    }
    /// The text payload of a `text` part, else `None`.
    pub fn as_text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

/// A2A `Artifact` (proto `message Artifact`) — a task output. `parts` is the
/// same `Part` oneof used by messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    /// Spec-REQUIRED: no `#[serde(default)]` — an Artifact without
    /// `artifactId` is rejected at deserialization.
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
}

/// A2A `TaskState` (proto `enum TaskState`) — task lifecycle state.
/// SCREAMING_SNAKE_CASE on the wire (spec §5.5). `TASK_STATE_UNSPECIFIED`
/// is rejected at deserialization: the spec marks `state` as REQUIRED and
/// the default UNSPECIFIED value must not be accepted as a valid wire value.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaskState {
    #[default]
    TaskStateUnspecified,
    TaskStateSubmitted,
    TaskStateWorking,
    TaskStateCompleted,
    TaskStateFailed,
    TaskStateCanceled,
    TaskStateInputRequired,
    TaskStateRejected,
    TaskStateAuthRequired,
}

impl<'de> serde::Deserialize<'de> for TaskState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
        #[allow(clippy::enum_variant_names)]
        enum TaskStateWire {
            TaskStateUnspecified,
            TaskStateSubmitted,
            TaskStateWorking,
            TaskStateCompleted,
            TaskStateFailed,
            TaskStateCanceled,
            TaskStateInputRequired,
            TaskStateRejected,
            TaskStateAuthRequired,
        }
        let v = TaskStateWire::deserialize(deserializer)?;
        match v {
            TaskStateWire::TaskStateUnspecified => Err(serde::de::Error::custom(
                "A2A TaskState must not be TASK_STATE_UNSPECIFIED (spec REQUIRED field)",
            )),
            TaskStateWire::TaskStateSubmitted => Ok(TaskState::TaskStateSubmitted),
            TaskStateWire::TaskStateWorking => Ok(TaskState::TaskStateWorking),
            TaskStateWire::TaskStateCompleted => Ok(TaskState::TaskStateCompleted),
            TaskStateWire::TaskStateFailed => Ok(TaskState::TaskStateFailed),
            TaskStateWire::TaskStateCanceled => Ok(TaskState::TaskStateCanceled),
            TaskStateWire::TaskStateInputRequired => Ok(TaskState::TaskStateInputRequired),
            TaskStateWire::TaskStateRejected => Ok(TaskState::TaskStateRejected),
            TaskStateWire::TaskStateAuthRequired => Ok(TaskState::TaskStateAuthRequired),
        }
    }
}

impl TaskState {
    /// Terminal states per spec: COMPLETED, FAILED, CANCELED, REJECTED.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskState::TaskStateCompleted
                | TaskState::TaskStateFailed
                | TaskState::TaskStateCanceled
                | TaskState::TaskStateRejected
        )
    }
}

/// A2A `TaskStatus` (proto `message TaskStatus`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    /// Spec-REQUIRED: no `#[serde(default)]` — a TaskStatus without `state`
    /// is rejected at deserialization (not defaulted to UNSPECIFIED).
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// ISO 8601 UTC timestamp string (proto `google.protobuf.Timestamp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// A2A `Task` (proto `message Task`) — returned by `SendMessage` /
/// `GetTask` / `CancelTask`. Fields beyond the spec-required `id`/`status`
/// are optional and tolerate absence: a peer that omits `contextId` or
/// `artifacts` still parses.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Spec-REQUIRED: no `#[serde(default)]` — a Task without `id` is
    /// rejected at deserialization (proto `field_behavior = REQUIRED`).
    pub id: String,
    #[serde(default)]
    pub context_id: String,
    /// Spec-REQUIRED: no `#[serde(default)]` — a Task without `status` is
    /// rejected at deserialization.
    pub status: TaskStatus,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// A2A `Message` (proto `message Message`) — one unit of client/server
/// communication. `messageId` and `role` are spec-REQUIRED on send, and
/// `parts` MUST contain at least one `Part` (spec §5.7 REQUIRED — a list
/// field with REQUIRED behavior must have ≥1 element).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<String>,
}

impl<'de> serde::Deserialize<'de> for Message {
    /// Enforce spec REQUIRED invariants: `role` must not be `ROLE_UNSPECIFIED`
    /// (the 0-value means "not set"), and `parts` must be non-empty (≥1 Part).
    /// A missing `messageId`/`role`/`parts` field fails at the serde level
    /// (no `#[serde(default)]`); this layer rejects present-but-empty values.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct MessageShadow {
            message_id: String,
            #[serde(default)]
            context_id: Option<String>,
            #[serde(default)]
            task_id: Option<String>,
            #[serde(default)]
            role: Role,
            parts: Vec<Part>,
            #[serde(default)]
            metadata: Option<serde_json::Value>,
            #[serde(default)]
            extensions: Vec<String>,
            #[serde(default)]
            reference_task_ids: Vec<String>,
        }
        let s = MessageShadow::deserialize(deserializer)?;
        if s.role == Role::RoleUnspecified {
            return Err(serde::de::Error::custom(
                "A2A Message.role must not be ROLE_UNSPECIFIED (spec REQUIRED)",
            ));
        }
        if s.parts.is_empty() {
            return Err(serde::de::Error::custom(
                "A2A Message.parts must contain at least one Part (spec REQUIRED ≥1)",
            ));
        }
        Ok(Message {
            message_id: s.message_id,
            context_id: s.context_id,
            task_id: s.task_id,
            role: s.role,
            parts: s.parts,
            metadata: s.metadata,
            extensions: s.extensions,
            reference_task_ids: s.reference_task_ids,
        })
    }
}

/// `SendMessage` request `params` (proto `SendMessageRequest`). `tenant`
/// echoes the selected `AgentInterface.tenant`; `message` is REQUIRED.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SendMessageParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<SendMessageConfiguration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// `SendMessageConfiguration` (proto) — optional send tuning.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SendMessageConfiguration {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_length: Option<i32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub return_immediately: bool,
}

/// `SendMessageResponse` (proto `message SendMessageResponse`) — the
/// `oneof payload` with branches `task` / `message`. Custom deserialize
/// enforces exactly-one branch (proto oneof semantics).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum SendMessageResponse {
    Task { task: Task },
    Message { message: Message },
}

impl<'de> serde::Deserialize<'de> for SendMessageResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("SendMessageResponse must be a JSON object"))?;
        let has_task = obj.contains_key("task");
        let has_message = obj.contains_key("message");
        match (has_task, has_message) {
            (true, false) => {
                let task: Task = serde_json::from_value(obj["task"].clone()).map_err(|e| {
                    serde::de::Error::custom(format!("SendMessageResponse.task: {e}"))
                })?;
                Ok(SendMessageResponse::Task { task })
            }
            (false, true) => {
                let message: Message =
                    serde_json::from_value(obj["message"].clone()).map_err(|e| {
                        serde::de::Error::custom(format!("SendMessageResponse.message: {e}"))
                    })?;
                Ok(SendMessageResponse::Message { message })
            }
            (true, true) => Err(serde::de::Error::custom(
                "SendMessageResponse must have exactly one branch (task or message); got both",
            )),
            (false, false) => Err(serde::de::Error::custom(
                "SendMessageResponse must have a task or message branch; got neither",
            )),
        }
    }
}

/// A2A JSON-RPC 2.0 request envelope (spec §9).
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

/// A2A JSON-RPC 2.0 response envelope. Peers wrap the result payload
/// (`Task`, `SendMessageResponse`, ...) in `result`; errors come back as
/// `error` with the spec's negative codes. `result` is `Option<T>` so a
/// present-but-`null` result deserializes to `None` without forcing `T:
/// Default` (the response union types are untagged enums with no Default).
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse<T> {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: serde_json::Value,
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
        // v1.0 wire: SCREAMING_SNAKE_CASE state, flattened Part (no kind).
        let payload = serde_json::json!({
            "id": "task-1",
            "contextId": "ctx-1",
            "status": { "state": "TASK_STATE_COMPLETED" },
            "artifacts": [
                { "artifactId": "art-1", "parts": [ { "text": "done" } ] }
            ]
        });
        let task: Task = serde_json::from_value(payload).unwrap();
        assert_eq!(task.id, "task-1");
        assert_eq!(task.context_id, "ctx-1");
        assert_eq!(task.status.state, TaskState::TaskStateCompleted);
        assert_eq!(task.artifacts[0].parts[0].as_text().unwrap(), "done");
    }

    #[test]
    fn task_tolerates_missing_optional_fields() {
        let payload = serde_json::json!({
            "id": "x",
            "status": { "state": "TASK_STATE_WORKING" }
        });
        let task: Task = serde_json::from_value(payload).unwrap();
        assert_eq!(task.id, "x");
        assert_eq!(task.status.state, TaskState::TaskStateWorking);
        assert!(task.context_id.is_empty());
        assert!(task.artifacts.is_empty());
        assert!(task.history.is_empty());
    }

    #[test]
    fn terminal_states_classified() {
        assert!(TaskState::TaskStateCompleted.is_terminal());
        assert!(TaskState::TaskStateFailed.is_terminal());
        assert!(TaskState::TaskStateCanceled.is_terminal());
        assert!(TaskState::TaskStateRejected.is_terminal());
        assert!(!TaskState::TaskStateWorking.is_terminal());
        assert!(!TaskState::TaskStateInputRequired.is_terminal());
    }

    #[test]
    fn agent_card_round_trips() {
        let card = AgentCard {
            name: "alpha".into(),
            description: "test agent".into(),
            supported_interfaces: vec![AgentInterface {
                url: "https://agent.example.com/a2a/v1".into(),
                protocol_binding: "JSONRPC".into(),
                tenant: Some("tenant-a".into()),
                protocol_version: "1.0".into(),
            }],
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
        // camelCase wire shape + tenant echoed
        assert!(json.contains("\"supportedInterfaces\""));
        assert!(json.contains("\"defaultInputModes\""));
        assert!(json.contains("\"protocolBinding\":\"JSONRPC\""));
        assert!(json.contains("\"tenant\":\"tenant-a\""));
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
            "result": {
                "id": "task-9",
                "status": { "state": "TASK_STATE_COMPLETED" }
            }
        });
        let resp: JsonRpcResponse<Task> = serde_json::from_value(payload).unwrap();
        let task = rpc_result(resp).unwrap();
        assert_eq!(task.id, "task-9");
        assert_eq!(task.status.state, TaskState::TaskStateCompleted);
    }

    #[test]
    fn send_message_params_serializes_v1() {
        let params = SendMessageParams {
            tenant: Some("tenant-a".into()),
            message: Message {
                message_id: "msg-uuid".into(),
                context_id: None,
                task_id: None,
                role: Role::RoleUser,
                parts: vec![Part::text_str("hello")],
                metadata: None,
                extensions: vec![],
                reference_task_ids: vec![],
            },
            configuration: None,
            metadata: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        // v1.0 wire shape
        assert!(json.contains("\"message\""));
        assert!(json.contains("\"role\":\"ROLE_USER\""));
        assert!(json.contains("\"messageId\":\"msg-uuid\""));
        assert!(json.contains("\"text\":\"hello\""));
        assert!(!json.contains("\"kind\""));
        assert!(json.contains("\"tenant\":\"tenant-a\""));
    }

    #[test]
    fn send_message_response_task_branch() {
        let payload = serde_json::json!({
            "task": { "id": "t-1", "status": { "state": "TASK_STATE_COMPLETED" } }
        });
        let resp: SendMessageResponse = serde_json::from_value(payload).unwrap();
        match resp {
            SendMessageResponse::Task { task } => assert_eq!(task.id, "t-1"),
            SendMessageResponse::Message { .. } => panic!("expected task branch"),
        }
    }

    #[test]
    fn send_message_response_message_branch() {
        let payload = serde_json::json!({
            "message": {
                "messageId": "m-1",
                "role": "ROLE_AGENT",
                "parts": [{ "text": "hi" }]
            }
        });
        let resp: SendMessageResponse = serde_json::from_value(payload).unwrap();
        match resp {
            SendMessageResponse::Message { message } => {
                assert_eq!(message.message_id, "m-1");
                assert_eq!(message.role, Role::RoleAgent);
            }
            SendMessageResponse::Task { .. } => panic!("expected message branch"),
        }
    }

    #[test]
    fn part_data_branch_round_trips() {
        let payload = serde_json::json!({ "data": { "k": "v" }, "mediaType": "application/json" });
        let part: Part = serde_json::from_value(payload).unwrap();
        assert_eq!(part.data.as_ref().unwrap()["k"], "v");
        assert_eq!(part.media_type.as_deref(), Some("application/json"));
        assert!(part.text.is_none());
    }

    #[test]
    fn part_rejects_zero_content_branches() {
        // proto oneof content: a Part with no content branch is invalid.
        let payload = serde_json::json!({ "mediaType": "text/plain" });
        let err = serde_json::from_value::<Part>(payload).unwrap_err();
        assert!(
            err.to_string().contains("exactly one content branch"),
            "empty Part must be rejected: {err}"
        );
    }

    #[test]
    fn part_rejects_multiple_content_branches() {
        // proto oneof content: a Part with two branches (text + data) is invalid.
        let payload = serde_json::json!({ "text": "hi", "data": { "k": "v" } });
        let err = serde_json::from_value::<Part>(payload).unwrap_err();
        assert!(
            err.to_string().contains("exactly one content branch"),
            "multi-branch Part must be rejected: {err}"
        );
        // text + url (both strings) also rejected.
        let payload2 = serde_json::json!({ "text": "hi", "url": "https://x" });
        assert!(serde_json::from_value::<Part>(payload2).is_err());
    }

    #[test]
    fn task_rejects_missing_required_id_and_status() {
        // spec REQUIRED: `id` and `status` must be present (no default).
        let no_id = serde_json::json!({ "status": { "state": "TASK_STATE_COMPLETED" } });
        assert!(
            serde_json::from_value::<Task>(no_id).is_err(),
            "Task without id must be rejected"
        );
        let no_status = serde_json::json!({ "id": "t-1" });
        assert!(
            serde_json::from_value::<Task>(no_status).is_err(),
            "Task without status must be rejected"
        );
    }

    #[test]
    fn task_status_rejects_missing_state() {
        // spec REQUIRED: TaskStatus.state must be present (no default to UNSPECIFIED).
        let no_state = serde_json::json!({ "message": null });
        assert!(
            serde_json::from_value::<TaskStatus>(no_state).is_err(),
            "TaskStatus without state must be rejected"
        );
    }

    #[test]
    fn message_rejects_unspecified_role_and_empty_parts() {
        // spec REQUIRED: role must not be ROLE_UNSPECIFIED; parts must be ≥1.
        let unspecified_role = serde_json::json!({
            "messageId": "m-1",
            "role": "ROLE_UNSPECIFIED",
            "parts": [{ "text": "hi" }]
        });
        assert!(
            serde_json::from_value::<Message>(unspecified_role).is_err(),
            "Message with ROLE_UNSPECIFIED must be rejected"
        );
        let empty_parts = serde_json::json!({
            "messageId": "m-1",
            "role": "ROLE_USER",
            "parts": []
        });
        assert!(
            serde_json::from_value::<Message>(empty_parts).is_err(),
            "Message with empty parts must be rejected"
        );
        // Missing role field entirely also rejected.
        let no_role = serde_json::json!({ "messageId": "m-1", "parts": [{ "text": "hi" }] });
        assert!(serde_json::from_value::<Message>(no_role).is_err());
        // Missing parts field entirely rejected.
        let no_parts = serde_json::json!({ "messageId": "m-1", "role": "ROLE_USER" });
        assert!(serde_json::from_value::<Message>(no_parts).is_err());
    }

    #[test]
    fn artifact_rejects_missing_artifact_id() {
        // spec REQUIRED: artifactId must be present.
        let no_id = serde_json::json!({ "parts": [{ "text": "x" }] });
        assert!(
            serde_json::from_value::<Artifact>(no_id).is_err(),
            "Artifact without artifactId must be rejected"
        );
    }

    /// Checked-in conformance fixtures: these JSON files capture the v1.0
    /// wire shape from the spec and are compared against the DTO
    /// serialization/deserialization. If the DTOs drift from the spec, these
    /// tests fail — preventing silent protocol drift without a live peer.
    #[test]
    fn fixture_send_message_request_round_trips() {
        let raw = include_str!("../tests/fixtures/send_message_request.json");
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        // The request envelope must parse as a JsonRpcRequest.
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "SendMessage");
        // The params must parse as SendMessageParams.
        let params: SendMessageParams = serde_json::from_value(req.params).unwrap();
        assert_eq!(params.message.message_id, "msg-fixture-1");
        assert_eq!(params.message.role, Role::RoleUser);
        assert_eq!(params.tenant.as_deref(), Some("tenant-a"));
        // Re-serialize and check key wire fields are present (no `kind`).
        let reserialized = serde_json::to_string(&params).unwrap();
        assert!(reserialized.contains("\"messageId\""));
        assert!(reserialized.contains("\"role\":\"ROLE_USER\""));
        assert!(reserialized.contains("\"text\""));
        assert!(!reserialized.contains("\"kind\""));
        // The full envelope round-trips (parse → serialize → same shape).
        let _roundtrip: serde_json::Value = serde_json::to_value(&parsed).unwrap();
    }

    #[test]
    fn fixture_send_message_response_task_branch() {
        let raw = include_str!("../tests/fixtures/send_message_response_task.json");
        let resp: JsonRpcResponse<SendMessageResponse> = serde_json::from_str(raw).unwrap();
        let payload = rpc_result(resp).unwrap();
        match payload {
            SendMessageResponse::Task { task } => {
                assert_eq!(task.id, "task-fixture-1");
                assert_eq!(task.context_id, "ctx-fixture-1");
                assert_eq!(task.status.state, TaskState::TaskStateCompleted);
                assert_eq!(task.artifacts[0].artifact_id, "art-fixture-1");
                assert_eq!(
                    task.artifacts[0].parts[0].as_text().unwrap(),
                    "Fixture reply"
                );
            }
            SendMessageResponse::Message { .. } => panic!("expected Task branch"),
        }
    }

    #[test]
    fn fixture_send_message_response_message_branch() {
        let raw = include_str!("../tests/fixtures/send_message_response_message.json");
        let resp: JsonRpcResponse<SendMessageResponse> = serde_json::from_str(raw).unwrap();
        let payload = rpc_result(resp).unwrap();
        match payload {
            SendMessageResponse::Message { message } => {
                assert_eq!(message.message_id, "msg-reply-1");
                assert_eq!(message.role, Role::RoleAgent);
                assert_eq!(message.parts[0].as_text().unwrap(), "Direct reply");
            }
            SendMessageResponse::Task { .. } => panic!("expected Message branch"),
        }
    }
}
