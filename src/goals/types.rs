use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    #[default]
    Proposed,
    Approved,
    InProgress,
    Completed,
    Rejected,
}

impl GoalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "approved" => Self::Approved,
            "in_progress" => Self::InProgress,
            "completed" => Self::Completed,
            "rejected" => Self::Rejected,
            _ => Self::Proposed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalSource {
    #[default]
    Telos,
    StalePrd,
    RecurringFailure,
    UserRequest,
}

impl GoalSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Telos => "telos",
            Self::StalePrd => "stale_prd",
            Self::RecurringFailure => "recurring_failure",
            Self::UserRequest => "user_request",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "stale_prd" => Self::StalePrd,
            "recurring_failure" => Self::RecurringFailure,
            "user_request" => Self::UserRequest,
            _ => Self::Telos,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VerificationMethod {
    /// Trust the agent's response. Used when no concrete check is feasible.
    /// Backward-compatible default for goals without explicit verification.
    #[default]
    AgentSelfReport,
    /// Run a shell command. Success = expected exit status (zero by default).
    Command {
        cmd: String,
        #[serde(default = "default_expect_exit_zero")]
        expect_exit_zero: bool,
    },
    /// Health subsystem component must be in `ok` state after the agent finishes.
    HealthOk { component: String },
    /// Never auto-complete. Always revert to `approved` so a human verifies manually.
    Manual,
}

fn default_expect_exit_zero() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub description: String,
    pub source: GoalSource,
    pub status: GoalStatus,
    pub priority: u8,
    pub proposed_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub evidence: Option<String>,
    /// Atomic Ideal-State Criteria. All must be satisfied for completion.
    /// Empty vec is allowed and falls back to `verification_method`.
    #[serde(default)]
    pub success_criteria: Vec<String>,
    /// How the autonomy loop verifies that this goal is satisfied.
    #[serde(default)]
    pub verification_method: VerificationMethod,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoalPatch {
    pub title: Option<String>,
    pub description: Option<String>,
    pub source: Option<GoalSource>,
    pub status: Option<GoalStatus>,
    pub priority: Option<u8>,
    pub evidence: Option<String>,
}
