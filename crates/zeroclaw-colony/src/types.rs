use serde::{Deserialize, Serialize};

use zeroclaw_runtime::control_plane::{GoalTaskRecord, TaskRecord};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ClarificationAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAssignment {
    pub agent: String,
    pub instruction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct QueenProposal {
    #[serde(default)]
    pub questions: Vec<String>,
    pub summary: String,
    #[serde(default)]
    pub assignments: Vec<ColonyAssignment>,
    #[serde(default)]
    pub new_agents: Vec<ColonyAgentProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyAgentProposal {
    pub alias: String,
    pub core_command: String,
    pub template: String,
    #[serde(default)]
    pub connections: Vec<zeroclaw_config::colony::ColonyConnection>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum GoalContextMode {
    #[default]
    Fresh,
    Continue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GoalRequest {
    pub objective: String,
    #[serde(default)]
    pub mode: GoalContextMode,
    #[serde(default)]
    pub previous_goal_id: Option<String>,
    pub proposal: QueenProposal,
    /// Native operator approval for this reviewed batch; never a durable policy.
    #[serde(default)]
    pub approve_new_agents: bool,
    #[serde(default)]
    pub token_limit: Option<u64>,
    #[serde(default)]
    pub cost_limit_usd: Option<f64>,
}

/// Durable continuation position, rather than another lifecycle record. Only
/// the referenced TaskRecord determines whether this goal may execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyGoalExecution {
    pub task_id: String,
    pub colony_id: String,
    pub mode: GoalContextMode,
    pub previous_goal_id: Option<String>,
    pub proposal: QueenProposal,
    pub next_assignment: usize,
    /// Persisted before starting a turn. A remaining reference after restart
    /// makes its outcome uncertain and forbids automatic replay.
    pub active_child_id: Option<String>,
    #[serde(default)]
    pub active_inbox_id: Option<String>,
    pub summarizing: bool,
    pub summary_message_id: Option<String>,
    #[serde(default)]
    pub pending_plan: Option<QueenProposal>,
    #[serde(default)]
    pub plan_rounds: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyMessage {
    pub id: String,
    pub colony_id: String,
    pub task_id: Option<String>,
    pub sender: String,
    pub recipient: String,
    pub content: String,
    pub created_at: String,
    #[serde(default)]
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct GoalView {
    pub task: TaskRecord,
    pub goal: GoalTaskRecord,
    pub execution: ColonyGoalExecution,
    pub messages: Vec<ColonyMessage>,
    pub error: Option<String>,
    pub approvals: Vec<ColonyApproval>,
    /// Derived process-local attachment; a durable child reference alone is not a live turn.
    pub turn_attached: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyApproval {
    pub id: String,
    pub goal_id: String,
    pub agent: String,
    pub tool: String,
    pub arguments: serde_json::Value,
    pub decision: Option<bool>,
}
