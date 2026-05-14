//! Slot data model and storage trait.
//!
//! A `Slot` is the dashboard's primary entity: one row in the sidebar, one
//! independent conversation, with its own per-slot agent configuration
//! (provider, model, mode, personality). Slots layer on top of memory
//! sessions — each slot carries a `session_id` that points at the existing
//! session backend for history persistence.
//!
//! Persistence lives alongside `SessionBackend` in this crate so that the
//! SQLite backend can implement both traits against the same database file
//! without a circular dependency with `zeroclaw-gateway`.
//!
//! The request/response wrappers with `JsonSchema` derives live in the
//! gateway crate (`zeroclaw-gateway::slot`) since `zeroclaw-infra` does
//! not pull in `schemars`.

use serde::{Deserialize, Serialize};

#[cfg(feature = "schema-export")]
use schemars::JsonSchema;

/// Per-slot agent configuration overrides.
///
/// `None` fields inherit the global agent config. When a field is `Some`,
/// the slot's agent uses the override value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotAgentConfig {
    /// Provider id (e.g. `"anthropic"`, `"openai_codex"`). `None` = inherit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id (e.g. `"claude-3-5-sonnet"`). `None` = inherit provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Turn-by-turn approval mode.
    #[serde(default)]
    pub mode: SlotMode,
    /// Personality filename from `EDITABLE_PERSONALITY_FILES` (e.g. `"SOUL.md"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Named persona preset that stamped provider/model/personality/mode at
    /// creation time. Retained for display; not re-resolved on every message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_preset: Option<String>,
}

/// Tool-approval mode for a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SlotMode {
    /// Prompt for approval on every tool call. Default.
    #[default]
    Normal,
    /// Auto-approve tools pre-authorized by the user for this slot.
    Trust,
    /// Auto-approve every tool call. Dangerous; user opts in explicitly.
    Yolo,
}

/// Runtime state of a slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SlotState {
    /// No in-flight turn.
    #[default]
    Idle,
    /// An agent turn is in progress.
    Running,
    /// A tool call is waiting for user approval.
    WaitingApproval,
    /// The last turn errored and has not yet been cleared.
    Error,
}

/// A slot — one parallel conversation surface in the dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct Slot {
    /// Unique slot id (UUID). Distinct from `session_id`.
    pub id: String,
    /// Memory/history session id backing this slot. Shared sessions across
    /// slots are allowed (slots become aliased views of the same history).
    pub session_id: String,
    /// Human-readable title shown in the sidebar.
    pub title: String,
    /// Per-slot agent config overrides.
    #[serde(default)]
    pub agent_config: SlotAgentConfig,
    /// Current runtime state.
    #[serde(default)]
    pub state: SlotState,
    /// Creation timestamp (unix seconds).
    pub created_at: i64,
    /// Last update timestamp (unix seconds).
    pub updated_at: i64,
    /// Number of messages in the slot's session history.
    #[serde(default)]
    pub message_count: usize,
    /// Unread activity since the user last focused this slot.
    #[serde(default)]
    pub dirty: bool,
    /// Optional workspace label for grouping (`"home-lab"`, `"client-x"`).
    /// Labels only — does not scope tools, memory, or personality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

impl Slot {
    /// Build a new slot with defaults populated from `now` and a fresh id.
    ///
    /// The caller is responsible for supplying the `id` and `session_id` —
    /// constructors don't pull in `uuid` here to keep this crate's dep tree
    /// minimal.
    pub fn new(id: String, session_id: String, title: String, now: i64) -> Self {
        Self {
            id,
            session_id,
            title,
            agent_config: SlotAgentConfig::default(),
            state: SlotState::Idle,
            created_at: now,
            updated_at: now,
            message_count: 0,
            dirty: false,
            workspace: None,
        }
    }
}

/// Partial update for a slot applied via PATCH.
///
/// `Option<String>` can't distinguish "field absent" from "explicit
/// null" in JSON, so workspace clearing is signaled out-of-band via
/// `clear_workspace: true`. When set, `workspace` is nulled and any
/// value passed in `workspace` is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema-export", derive(JsonSchema))]
pub struct SlotUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_config: Option<SlotAgentConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<SlotState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Explicit signal to clear the workspace label. Takes precedence
    /// over `workspace` when `true`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub clear_workspace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_count: Option<usize>,
}

/// Storage trait for slot persistence.
///
/// Implementations must be `Send + Sync` for sharing across async tasks.
/// Default impls return the equivalent of "not supported" so that optional
/// backends (e.g. an in-memory stub) can implement just the operations they
/// need.
pub trait SlotStore: Send + Sync {
    /// Persist a new slot. Returns an error if `slot.id` already exists.
    fn create_slot(&self, slot: &Slot) -> std::io::Result<()>;

    /// Load a slot by id. Returns `Ok(None)` when the slot does not exist.
    fn get_slot(&self, slot_id: &str) -> std::io::Result<Option<Slot>>;

    /// List every slot, newest-updated first.
    fn list_slots(&self) -> std::io::Result<Vec<Slot>>;

    /// Apply a partial update. Returns `Ok(None)` when the slot does not exist.
    fn update_slot(&self, slot_id: &str, update: &SlotUpdate) -> std::io::Result<Option<Slot>>;

    /// Delete a slot. Returns `true` when the slot existed and was removed.
    /// Does not touch the backing memory session — callers decide whether
    /// to delete the shared history separately.
    fn delete_slot(&self, slot_id: &str) -> std::io::Result<bool>;

    /// Count total slots. Used for soft/hard-limit enforcement.
    fn count_slots(&self) -> std::io::Result<usize>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_serde_roundtrip() {
        let slot = Slot {
            id: "slot-1".into(),
            session_id: "gw_abc".into(),
            title: "Research".into(),
            agent_config: SlotAgentConfig {
                provider: Some("anthropic".into()),
                model: Some("claude-3-5-sonnet".into()),
                mode: SlotMode::Trust,
                personality: Some("SOUL.md".into()),
                persona_preset: Some("codex-researcher".into()),
            },
            state: SlotState::Running,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_050,
            message_count: 12,
            dirty: true,
            workspace: Some("home-lab".into()),
        };

        let json = serde_json::to_string(&slot).expect("serialize");
        let back: Slot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(slot, back);
    }

    #[test]
    fn slot_agent_config_defaults() {
        let cfg = SlotAgentConfig::default();
        assert!(cfg.provider.is_none());
        assert!(cfg.model.is_none());
        assert_eq!(cfg.mode, SlotMode::Normal);
        assert!(cfg.personality.is_none());
        assert!(cfg.persona_preset.is_none());
    }

    #[test]
    fn slot_new_populates_defaults() {
        let slot = Slot::new("s1".into(), "gw_s1".into(), "T".into(), 42);
        assert_eq!(slot.id, "s1");
        assert_eq!(slot.session_id, "gw_s1");
        assert_eq!(slot.state, SlotState::Idle);
        assert_eq!(slot.created_at, 42);
        assert_eq!(slot.updated_at, 42);
        assert!(!slot.dirty);
        assert_eq!(slot.message_count, 0);
    }

    #[test]
    fn slot_state_is_snake_case_in_json() {
        // Serialized form must stay stable for the dashboard protocol.
        let s = serde_json::to_string(&SlotState::WaitingApproval).unwrap();
        assert_eq!(s, "\"waiting_approval\"");
    }

    #[test]
    fn slot_mode_serializes_lowercase() {
        let s = serde_json::to_string(&SlotMode::Yolo).unwrap();
        assert_eq!(s, "\"yolo\"");
    }

    #[test]
    fn slot_update_is_all_optional() {
        let empty: SlotUpdate = serde_json::from_str("{}").unwrap();
        assert!(empty.title.is_none());
        assert!(empty.agent_config.is_none());
        assert!(empty.state.is_none());
    }
}
