use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskPriority(u8);

impl TaskPriority {
    pub fn new(value: u8) -> Self {
        Self(value.clamp(1, 10))
    }

    pub fn value(self) -> u8 {
        self.0
    }
}

impl Default for TaskPriority {
    fn default() -> Self {
        Self(5)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    #[default]
    Pending,
    Processing,
    Completed,
    Failed,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "processing" => Self::Processing,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskSource {
    #[default]
    User,
    GoalProposer,
    HealthCheck,
    Consolidation,
}

impl TaskSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::GoalProposer => "goal_proposer",
            Self::HealthCheck => "health_check",
            Self::Consolidation => "consolidation",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.to_ascii_lowercase().as_str() {
            "goal_proposer" => Self::GoalProposer,
            "health_check" => Self::HealthCheck,
            "consolidation" => Self::Consolidation,
            _ => Self::User,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub source: TaskSource,
    pub task: String,
    pub dependencies: Vec<String>,
    pub result: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskPatch {
    pub priority: Option<TaskPriority>,
    pub status: Option<TaskStatus>,
    pub source: Option<TaskSource>,
    pub task: Option<String>,
    pub dependencies: Option<Vec<String>>,
    pub result: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}
