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
    pub fn as_str(&self) -> &'static str {
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
