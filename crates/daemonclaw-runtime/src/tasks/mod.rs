pub mod store;

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone)]
pub struct TaskBinding {
    pub task_id: String,
    pub actor_id: String,
}

tokio::task_local! {
    pub static CURRENT_TASK_BINDING: Option<TaskBinding>;
}

pub fn with_task_binding<F: std::future::Future>(
    binding: Option<TaskBinding>,
    f: F,
) -> impl std::future::Future<Output = F::Output> {
    CURRENT_TASK_BINDING.scope(binding, f)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    Active,
    Blocked,
    Paused,
    Review,
    Closed,
    Abandoned,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Active => "active",
            Self::Blocked => "blocked",
            Self::Paused => "paused",
            Self::Review => "review",
            Self::Closed => "closed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "active" => Some(Self::Active),
            "blocked" => Some(Self::Blocked),
            "paused" => Some(Self::Paused),
            "review" => Some(Self::Review),
            "closed" => Some(Self::Closed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed | Self::Abandoned)
    }
}

impl fmt::Display for TaskStatus {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    Auto,
    Assisted,
    Gated,
}

impl Autonomy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Assisted => "assisted",
            Self::Gated => "gated",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "assisted" => Some(Self::Assisted),
            "gated" => Some(Self::Gated),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    Agentic,
    Deterministic,
}

impl Execution {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agentic => "agentic",
            Self::Deterministic => "deterministic",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "agentic" => Some(Self::Agentic),
            "deterministic" => Some(Self::Deterministic),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceItem {
    pub kind: String, // "machine" | "human"
    pub check: String,
    pub satisfied: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub intent: Option<String>,
    pub acceptance: Vec<AcceptanceItem>,
    pub status: TaskStatus,
    pub priority: u8,
    pub assigned_to: Option<String>,
    pub autonomy: Autonomy,
    pub execution: Execution,
    pub tools: Vec<String>,
    pub blockers: serde_json::Value,
    pub template_id: Option<String>,
    pub source: String,
    pub abandon_reason: Option<String>,
    pub outcome: Option<TaskOutcome>,
    pub turn_count: u32,
    pub created_at: String,
    pub updated_at: String,
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
    Pause,
    Resume,
    Submit,
    Close,
    Abandon,
}

impl Transition {
    pub fn valid_from(&self) -> &[TaskStatus] {
        match self {
            Self::Claim => &[TaskStatus::Open],
            Self::Block => &[TaskStatus::Active],
            Self::Unblock => &[TaskStatus::Blocked],
            Self::Pause => &[TaskStatus::Active],
            Self::Resume => &[TaskStatus::Paused],
            Self::Submit => &[TaskStatus::Active],
            Self::Close => &[TaskStatus::Review],
            Self::Abandon => &[
                TaskStatus::Open,
                TaskStatus::Active,
                TaskStatus::Blocked,
                TaskStatus::Paused,
                TaskStatus::Review,
            ],
        }
    }

    pub fn target_status(&self) -> TaskStatus {
        match self {
            Self::Claim | Self::Unblock | Self::Resume => TaskStatus::Active,
            Self::Block => TaskStatus::Blocked,
            Self::Pause => TaskStatus::Paused,
            Self::Submit => TaskStatus::Review,
            Self::Close => TaskStatus::Closed,
            Self::Abandon => TaskStatus::Abandoned,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Claim => "claim",
            Self::Block => "block",
            Self::Unblock => "unblock",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Submit => "submit",
            Self::Close => "close",
            Self::Abandon => "abandon",
        }
    }
}

/// Trait seam for machine acceptance verification.
/// Track E swaps in git_operations/test-runner; Track A provides only the
/// default implementation that shells out.
pub trait AcceptanceVerifier: Send + Sync {
    fn verify(&self, check: &str) -> std::result::Result<bool, String>;
}

pub struct ShellVerifier;

impl AcceptanceVerifier for ShellVerifier {
    fn verify(&self, check: &str) -> std::result::Result<bool, String> {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(check)
            .output()
            .map_err(|e| format!("failed to run check: {e}"))?;
        Ok(output.status.success())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("invalid transition: cannot {transition} from {current_status}")]
    InvalidTransition {
        current_status: TaskStatus,
        transition: String,
    },
    #[error("claim conflict: task is already {actual_status} (not open)")]
    ClaimConflict { actual_status: TaskStatus },
    #[error("abandon requires a reason")]
    AbandonRequiresReason,
    #[error("close refused: {reason}")]
    CloseRefused { reason: String },
    #[error("priority must be 0-4, got {0}")]
    InvalidPriority(u8),
    #[error("acceptance item not found: {0}")]
    AcceptanceItemNotFound(String),
    #[error("audit error: {0}")]
    Audit(String),
    #[error("database error: {0}")]
    Db(#[from] anyhow::Error),
}
