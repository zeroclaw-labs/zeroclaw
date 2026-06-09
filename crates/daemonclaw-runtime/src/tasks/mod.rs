pub mod store;

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Open,
    Claimed,
    Blocked,
    Done,
    Abandoned,
}

impl TaskState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Claimed => "claimed",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "claimed" => Some(Self::Claimed),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Abandoned)
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOrigin {
    Heartbeat,
    Cron,
    Channel,
    Manual,
    Sop,
}

impl TaskOrigin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::Cron => "cron",
            Self::Channel => "channel",
            Self::Manual => "manual",
            Self::Sop => "sop",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "heartbeat" => Some(Self::Heartbeat),
            "cron" => Some(Self::Cron),
            "channel" => Some(Self::Channel),
            "manual" => Some(Self::Manual),
            "sop" => Some(Self::Sop),
            _ => None,
        }
    }
}

impl fmt::Display for TaskOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub origin: TaskOrigin,
    pub state: TaskState,
    pub priority: u8,
    pub created_at: String,
    pub updated_at: String,
    pub claimed_by_channel: Option<String>,
    pub claimed_by_id: Option<String>,
    pub blocked_reason: Option<String>,
    pub outcome: Option<TaskOutcome>,
    pub parent_task_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskActivity {
    pub id: i64,
    pub task_id: String,
    pub old_state: Option<TaskState>,
    pub new_state: TaskState,
    pub actor_channel: String,
    pub actor_id: Option<String>,
    pub reason: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct TaskActor {
    pub channel: String,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Claim,
    Block,
    Unblock,
    Complete,
    Abandon,
}

impl Transition {
    pub fn valid_from(&self) -> &[TaskState] {
        match self {
            Self::Claim => &[TaskState::Open, TaskState::Blocked],
            Self::Block => &[TaskState::Claimed],
            Self::Unblock => &[TaskState::Blocked],
            Self::Complete => &[TaskState::Claimed],
            Self::Abandon => &[TaskState::Open, TaskState::Claimed, TaskState::Blocked],
        }
    }

    pub fn target_state(&self) -> TaskState {
        match self {
            Self::Claim | Self::Unblock => TaskState::Claimed,
            Self::Block => TaskState::Blocked,
            Self::Complete => TaskState::Done,
            Self::Abandon => TaskState::Abandoned,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("invalid transition: cannot {transition} from {current_state}")]
    InvalidTransition {
        current_state: TaskState,
        transition: String,
    },
    #[error("claim conflict: task already claimed (expected state version {expected}, found {found})")]
    ClaimConflict { expected: String, found: String },
    #[error("abandon requires a reason")]
    AbandonRequiresReason,
    #[error("complete requires an outcome")]
    CompleteRequiresOutcome,
    #[error("priority must be 0-4, got {0}")]
    InvalidPriority(u8),
    #[error("database error: {0}")]
    Db(#[from] anyhow::Error),
}
