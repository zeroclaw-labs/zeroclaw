//! Core data types for the MoA ontology layer.
//!
//! Models the Palantir-style Object/Link/Action triple that represents
//! the user's real world as a digital twin.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Meta-types (schema definitions)
// ---------------------------------------------------------------------------

/// An object type definition (e.g. "User", "Contact", "Task", "Document").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectType {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
}

/// A link type definition constraining which object types can be connected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkType {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub from_type_id: i64,
    pub to_type_id: i64,
}

/// An action type definition (e.g. "SendMessage", "CreateTask").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionType {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    /// Optional JSON Schema that validates action parameters.
    pub params_schema: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Instance types (actual data)
// ---------------------------------------------------------------------------

/// A concrete object in the ontology graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyObject {
    pub id: i64,
    pub type_id: i64,
    /// Human-readable display name (Palantir's "Title Key").
    pub title: Option<String>,
    /// Flexible properties stored as JSON.
    pub properties: serde_json::Value,
    /// Owner scope for multi-user isolation.
    pub owner_user_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A directed relationship between two objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyLink {
    pub id: i64,
    pub link_type_id: i64,
    pub from_object_id: i64,
    pub to_object_id: i64,
    /// Optional metadata on the relationship (e.g. strength, tags).
    pub properties: Option<serde_json::Value>,
    pub created_at: i64,
}

/// Status of an action execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Pending,
    Success,
    Error,
    RolledBack,
}

impl std::fmt::Display for ActionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Success => write!(f, "success"),
            Self::Error => write!(f, "error"),
            Self::RolledBack => write!(f, "rolled_back"),
        }
    }
}

impl ActionStatus {
    /// Parse a status string, logging a warning for unrecognized values.
    ///
    /// Falls back to `Pending` for unknown values (DB may contain legacy data),
    /// but emits a warning so the issue is observable.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "success" => Self::Success,
            "error" => Self::Error,
            "rolled_back" => Self::RolledBack,
            other => {
                tracing::warn!(value = other, "unrecognized ActionStatus, defaulting to Pending");
                Self::Pending
            }
        }
    }
}

/// The actor kind that performed an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    User,
    Agent,
    System,
}

impl std::fmt::Display for ActorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Agent => write!(f, "agent"),
            Self::System => write!(f, "system"),
        }
    }
}

impl ActorKind {
    /// Parse an actor kind string, logging a warning for unrecognized values.
    pub fn from_str_lossy(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "agent" => Self::Agent,
            "system" => Self::System,
            other => {
                tracing::warn!(value = other, "unrecognized ActorKind, defaulting to Agent");
                Self::Agent
            }
        }
    }
}

/// A recorded action (the "verb" in the ontology).
///
/// Every action answers the 5W1H question: **Who** (actor) did **What**
/// (action_type) to **Whom** (primary_object), **When** (occurred_at),
/// **Where** (location), and **How** (params/result).
///
/// `occurred_at` and `location` are first-class fields because:
/// - Time-ordered action history enables timeline reconstruction.
/// - Location-based grouping reveals implicit relationships (e.g. two
///   people at the same place → likely met, events at same venue → pattern).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OntologyAction {
    pub id: i64,
    pub action_type_id: i64,
    pub actor_user_id: String,
    pub actor_kind: ActorKind,
    /// The primary target object of this action.
    pub primary_object_id: Option<i64>,
    /// Additional related objects (JSON array of IDs).
    pub related_object_ids: Vec<i64>,
    /// Input parameters supplied when the action was invoked.
    pub params: serde_json::Value,
    /// Execution result (populated after completion).
    pub result: Option<serde_json::Value>,
    /// Channel through which the action was triggered.
    pub channel: Option<String>,
    /// Context object ID for situational awareness.
    pub context_id: Option<i64>,
    /// **When (UTC)** — the absolute UTC time this action occurred.
    /// ISO-8601 format, always ending in `Z` (e.g. "2026-03-18T05:30:00.000Z").
    /// This is the **primary sort key** for cross-device timeline ordering.
    /// Distinct from `created_at` which is the DB insertion timestamp.
    pub occurred_at_utc: Option<String>,
    /// **When (device-local)** — the same instant in the device's local
    /// timezone at the time of recording (e.g. "2026-03-18T01:30:00.000-04:00"
    /// if the device was in US Eastern).
    pub occurred_at_local: Option<String>,
    /// IANA timezone of the device when the action was recorded
    /// (e.g. "America/New_York", "Asia/Seoul").
    pub timezone: Option<String>,
    /// **When (home timezone)** — the same instant converted to the user's
    /// home timezone (e.g. "2026-03-18T14:30:00.000+09:00" for Asia/Seoul).
    /// This is the **display time** so events from different devices appear
    /// on a single consistent timeline from the user's home perspective.
    pub occurred_at_home: Option<String>,
    /// The user's home timezone IANA name (e.g. "Asia/Seoul").
    pub home_timezone: Option<String>,
    /// **Where** — the real-world location of this action (free-form text,
    /// e.g. "서울 서초구 법원로 서울중앙지방법원", "골프장 레이크사이드CC",
    /// "서울 강남구 테헤란로 사무실").
    pub location: Option<String>,
    pub status: ActionStatus,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

// ---------------------------------------------------------------------------
// Request / Response types for the dispatcher
// ---------------------------------------------------------------------------

/// Parameters for executing an ontology action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteActionRequest {
    pub action_type_name: String,
    pub owner_user_id: String,
    pub actor_kind: Option<ActorKind>,
    pub primary_object_id: Option<i64>,
    pub related_object_ids: Vec<i64>,
    pub params: serde_json::Value,
    pub channel: Option<String>,
    pub context_id: Option<i64>,
    /// When the action occurred (ISO-8601 UTC, local with offset, or descriptive).
    /// The system will normalize this into UTC + device-local + home-timezone.
    pub occurred_at: Option<String>,
    /// Where the action occurred (free-form location string).
    pub location: Option<String>,
}

impl ExecuteActionRequest {
    /// Convenience: set occurred_at to None (system will auto-fill with now).
    pub fn without_time(mut self) -> Self {
        self.occurred_at = None;
        self
    }
}

/// Parameters for querying the context snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshotRequest {
    pub owner_user_id: String,
    pub channel: Option<String>,
    pub device_id: Option<String>,
    pub limit_recent_actions: usize,
}

/// A compact snapshot of the user's ontology state for LLM context injection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextSnapshot {
    pub user: Option<OntologyObject>,
    pub current_context: Option<OntologyObject>,
    pub recent_contacts: Vec<OntologyObject>,
    pub recent_tasks: Vec<OntologyObject>,
    pub recent_projects: Vec<OntologyObject>,
    pub recent_actions: Vec<ActionSummary>,
}

/// Lightweight summary of a past action for context injection.
///
/// Includes **when** and **where** so the LLM can reason about
/// the user's timeline and location patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSummary {
    pub action_type: String,
    pub primary_object_title: Option<String>,
    pub params_summary: serde_json::Value,
    pub result_summary: Option<serde_json::Value>,
    pub channel: Option<String>,
    /// When — UTC absolute time (sort key).
    pub occurred_at_utc: Option<String>,
    /// When — converted to user's home timezone (display).
    pub occurred_at_home: Option<String>,
    /// Device timezone when recorded.
    pub timezone: Option<String>,
    /// Where the action happened (free-form location string).
    pub location: Option<String>,
    pub status: String,
    pub created_at: i64,
}

/// Specification for creating a link alongside an object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkSpec {
    pub link_type: String,
    pub to_object_id: i64,
}
