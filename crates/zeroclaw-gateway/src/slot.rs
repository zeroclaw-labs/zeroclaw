//! Dashboard slot REST types.
//!
//! The core data model (`Slot`, `SlotAgentConfig`, `SlotState`, `SlotMode`,
//! `SlotStore`) lives in `zeroclaw-infra` so both the gateway and the
//! storage backend can use it without a dependency cycle. This module
//! re-exports those types and adds request/response wrappers with
//! `#[derive(schemars::JsonSchema)]` so the OpenAPI generator can
//! describe `/api/slots` endpoints.
//!
//! Behind the `schema-export` feature, JSON Schema derives are enabled;
//! without it the wrappers remain plain serde types. This mirrors the
//! `api_config` pattern.

use serde::{Deserialize, Serialize};

#[cfg(feature = "schema-export")]
use schemars::JsonSchema;

pub use zeroclaw_infra::slot::{Slot, SlotAgentConfig, SlotMode, SlotState, SlotStore, SlotUpdate};

/// `POST /api/slots` request body.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotCreateRequest {
    /// Display title for the sidebar. Defaults to `"Untitled"` when empty.
    #[serde(default)]
    pub title: Option<String>,
    /// Existing memory session id to back this slot with. When `None`, a
    /// new session id is minted server-side.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Initial agent config overrides. When omitted, the slot inherits the
    /// gateway's global agent config.
    #[serde(default)]
    pub agent_config: Option<SlotAgentConfig>,
    /// Optional workspace label for grouping. Does not scope tools/memory.
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `PATCH /api/slots/:id` request body.
///
/// Identical in shape to the storage-layer `SlotUpdate` type but re-declared
/// here so OpenAPI describes the API-visible fields without leaking the
/// infra enum names.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotPatchRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_config: Option<SlotAgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SlotState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl From<SlotPatchRequest> for SlotUpdate {
    fn from(req: SlotPatchRequest) -> Self {
        Self {
            title: req.title,
            agent_config: req.agent_config,
            state: req.state,
            workspace: req.workspace,
            dirty: None,
            message_count: None,
        }
    }
}

/// `POST /api/slots/:id/duplicate` request body.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotDuplicateRequest {
    /// Override the title for the duplicated slot. When `None`, the
    /// original title gets a `" (copy)"` suffix.
    #[serde(default)]
    pub title: Option<String>,
    /// When `true` (default `false`), the duplicate shares the source
    /// slot's `session_id` and thus its message history. When `false`, a
    /// fresh session id is minted.
    #[serde(default)]
    pub include_history: bool,
}

/// Response body for `GET /api/slots/:id`, `POST /api/slots`,
/// `PATCH /api/slots/:id`, and `POST /api/slots/:id/duplicate`.
///
/// The gateway serializes [`Slot`] directly. This type alias + schema
/// export wire the OpenAPI definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotResponse {
    pub id: String,
    pub session_id: String,
    pub title: String,
    pub agent_config: SlotAgentConfig,
    pub state: SlotState,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: usize,
    pub dirty: bool,
    pub workspace: Option<String>,
}

impl From<Slot> for SlotResponse {
    fn from(s: Slot) -> Self {
        Self {
            id: s.id,
            session_id: s.session_id,
            title: s.title,
            agent_config: s.agent_config,
            state: s.state,
            created_at: s.created_at,
            updated_at: s.updated_at,
            message_count: s.message_count,
            dirty: s.dirty,
            workspace: s.workspace,
        }
    }
}

/// Response body for `GET /api/slots`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotListResponse {
    pub slots: Vec<SlotResponse>,
}

/// Error body for slot endpoints. The `code` is a stable string the frontend
/// can switch on; `message` is human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotError {
    pub code: String,
    pub message: String,
}

impl SlotError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
