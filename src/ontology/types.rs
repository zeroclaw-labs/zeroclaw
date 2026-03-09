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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSummary {
    pub action_type: String,
    pub primary_object_title: Option<String>,
    pub params_summary: serde_json::Value,
    pub result_summary: Option<serde_json::Value>,
    pub channel: Option<String>,
    pub status: String,
    pub created_at: i64,
}

/// Specification for creating a link alongside an object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkSpec {
    pub link_type: String,
    pub to_object_id: i64,
}
