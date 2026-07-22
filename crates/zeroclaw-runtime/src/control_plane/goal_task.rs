//! Goal-specific task extensions for the durable control plane.
//!
//! Goal mode is represented as [`TaskKind::Goal`](super::TaskKind::Goal) in
//! the canonical task table. This module owns only the goal extension record,
//! continuation context, and goal-oriented repository operations layered on top
//! of that generic task record.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::task_registry::{TaskRecord, TaskStatus};

/// Goal-specific extension record keyed by the canonical task id.
///
/// Lifecycle, ownership, route, principal, timestamps, and terminal state stay on
/// [`TaskRecord`]. This record owns only goal-specific state that has no meaning
/// for delegates/subagents. In source-of-truth terms: this is the authoritative
/// row for the objective, effective limits, and goal pause details; it is not a
/// copy of the generic task row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalTaskRecord {
    /// Foreign key to the canonical [`TaskRecord`].
    pub task_id: String,
    /// Operator/model-supplied objective text. Treated as prompt input, not as
    /// trusted policy data.
    pub objective: String,
    /// Effective token limit for this goal after config defaults and command
    /// overrides have been resolved.
    ///
    /// This is persisted because later config edits must not rewrite a goal's
    /// already-admitted policy. Consumed and remaining tokens stay derived from
    /// the cost ledger.
    #[serde(default)]
    pub effective_token_limit: Option<u64>,
    /// Effective USD limit for this goal after config defaults and command
    /// overrides have been resolved.
    ///
    /// This is the admitted limit, not a usage counter. Actual spend is derived
    /// from canonical cost records.
    #[serde(default)]
    pub effective_cost_limit_usd: Option<f64>,
    /// Controller-readable reason the goal is paused.
    ///
    /// This explains a canonical [`TaskStatus::Paused`] state, but does not
    /// replace it. Terminal lifecycle state remains on the canonical task row.
    #[serde(default)]
    pub pause_reason: Option<GoalPauseReason>,
    /// Human-facing pause summary. Policy must branch on `pause_reason` and
    /// `blockers`, not by parsing this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_description: Option<String>,
    /// Structured blockers that explain what must change before continuation.
    ///
    /// The blocker list is the durable machine-readable resume surface for
    /// goal-specific pauses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GoalBlocker>,
}

/// Per-call view of a canonical task plus its goal extension.
///
/// This is deliberately not stored or cached. Lifecycle, routing, ownership,
/// timestamps, and terminal state remain canonical on [`TaskRecord`];
/// goal-only state remains canonical on [`GoalTaskRecord`]. The view exists so
/// controller code can pass one coherent domain object instead of repeatedly
/// pairing unrelated-looking values.
#[derive(Debug, Clone)]
pub struct TaskGoal {
    /// Canonical task row: lifecycle, ownership, route, principal, timestamps.
    task: TaskRecord,
    /// Goal extension row: objective, effective limits, pause/blocker detail.
    goal: GoalTaskRecord,
}

impl TaskGoal {
    /// Build an on-demand goal view from its canonical task row and goal
    /// extension row.
    pub fn new(task: TaskRecord, goal: GoalTaskRecord) -> Self {
        Self { task, goal }
    }

    /// Borrow the canonical task row.
    pub fn task(&self) -> &TaskRecord {
        &self.task
    }

    /// Borrow the goal extension row.
    pub fn goal(&self) -> &GoalTaskRecord {
        &self.goal
    }

    /// Canonical task id for this goal.
    pub fn task_id(&self) -> &str {
        &self.task.id
    }

    /// Agent alias that owns this goal.
    pub fn agent(&self) -> &str {
        &self.task.agent
    }

    /// Canonical lifecycle state for this goal task.
    pub fn status(&self) -> TaskStatus {
        self.task.status
    }

    /// True when the canonical task status is `Running`.
    pub fn is_running(&self) -> bool {
        self.status() == TaskStatus::Running
    }

    /// True when the canonical task status is terminal.
    pub fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }

    /// Untrusted objective text from the goal extension.
    pub fn objective(&self) -> &str {
        &self.goal.objective
    }

    /// Return a per-call view with updated effective budget limits.
    ///
    /// This does not persist anything. The caller must first commit the limit
    /// update through [`GoalTaskRegistry::update_goal_limits`], then use this
    /// helper to keep the controller's in-memory task/goal pair coherent for
    /// the rest of the admission decision.
    pub fn with_effective_limits(
        mut self,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
    ) -> Self {
        self.goal.effective_token_limit = token_limit;
        self.goal.effective_cost_limit_usd = cost_limit_usd;
        self
    }

    /// Consume the view and return only the canonical task row.
    ///
    /// Use this after goal-extension facts have already been folded into the
    /// controller decision. Mutations that move both lifecycle and pause state
    /// must still go through [`GoalTaskRegistry`] transaction methods.
    pub fn into_task(self) -> TaskRecord {
        self.task
    }

    /// Consume the view and return both canonical rows.
    pub fn into_parts(self) -> (TaskRecord, GoalTaskRecord) {
        (self.task, self.goal)
    }
}

/// Typed policy input for why a goal is paused.
///
/// A pause reason is goal-specific explanation layered on top of
/// [`TaskStatus::Paused`]. It must not be used as a second lifecycle enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPauseReason {
    /// An operator explicitly paused the goal through the control plane.
    OperatorPaused,
    /// The agent needs an operator answer before it can continue.
    NeedsUserInput,
    /// The agent escalated work to a human.
    HumanEscalation,
    /// A non-human dependency outside ZeroClaw is blocking progress.
    ExternalDependency,
    /// The selected provider or provider configuration is unavailable.
    ProviderUnavailable,
    /// The verifier could not produce a usable decision.
    VerifierBlocked,
    /// Goal-attributed usage reached an effective limit.
    BudgetExhausted,
    /// An effective limit exists but canonical usage records are unavailable.
    BudgetUnavailable,
    /// The daemon stopped before the goal could finish and restart recovery
    /// chose not to auto-continue it.
    #[serde(rename = "daemon_restarted", alias = "daemon_restart")]
    DaemonRestart,
}

/// Structured blocker packet attached to a paused goal.
///
/// Free-form text is only explanatory. Policy branches on `kind` and payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalBlocker {
    /// Machine-readable blocker class used for policy and resume routing.
    pub kind: GoalBlockerKind,
    /// Human-readable explanation of the blocker.
    pub message: String,
    /// Optional structured detail supplied by the tool/controller that created
    /// the blocker.
    ///
    /// This is durable goal-control metadata, not a generic event log. Consumers
    /// must treat embedded user/model text as untrusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

/// Coarse class of a blocker attached to a paused goal.
///
/// This is intentionally separate from [`GoalPauseReason`]: a pause has one
/// primary reason, while the blocker list can contain several actionable items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalBlockerKind {
    /// Waiting for an operator to resume an explicitly paused goal.
    OperatorPause,
    /// Waiting for an operator answer.
    NeedsUserInput,
    /// Waiting for human escalation handling.
    HumanEscalation,
    /// Waiting on an external system or dependency.
    ExternalDependency,
    /// Provider configuration or availability problem.
    Provider,
    /// Verifier outage or refusal to decide.
    Verifier,
    /// Effective budget limit or usage-ledger availability problem.
    Budget,
    /// Restart recovery state that needs continuation or operator action.
    RestartRecovery,
}

/// Complete pause extension to persist on a goal task.
///
/// The canonical task row says only that the task is paused. This structure
/// carries the goal-specific reason and actionable blockers needed for status,
/// resume, and recovery behavior.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalPauseState {
    /// Primary pause reason for controller policy.
    pub reason: GoalPauseReason,
    /// Optional human-visible summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Actionable blockers associated with the pause.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GoalBlocker>,
}

/// Persisted channel scope needed to synthesize trusted goal continuation turns.
///
/// This is intentionally narrower than `ChannelMessage`: it owns only delivery
/// and history-routing facts that cannot be reconstructed from
/// [`TaskRecord::originator_route`]. User text, attachments, and other
/// transient message data stay out of the durable control plane. The same
/// scope is reused for `/goal resume`, budget-triggered continuation, and
/// daemon-restart recovery so those paths do not invent separate delivery
/// state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskContinuationContext {
    /// Channel family that should receive the continuation turn.
    pub channel: String,
    /// Configured channel alias when multiple bots share the same channel
    /// family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_alias: Option<String>,
    /// Channel-native target used for replies or room/channel sends.
    pub reply_target: String,
    /// Original sender identity for history scope and user-visible routing.
    pub sender: String,
    /// Channel-native thread/topic id when the transport supports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
    /// Optional debouncer/interruption scope id to keep continuation ordering
    /// consistent with live channel turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interruption_scope_id: Option<String>,
    /// Conversation history scope to hydrate before injecting the continuation
    /// prompt.
    pub conversation_scope: TaskContinuationConversationScope,
}

/// Durable representation of the channel history scope for a continuation
/// prompt. Kept local to the control plane so the store does not depend on
/// channel transport structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskContinuationConversationScope {
    /// Continue in the same sender-scoped history used by direct chats.
    Sender,
    /// Continue in the same reply-target scoped history used by shared rooms or
    /// channels.
    ReplyTarget,
}

/// Repository operations for the goal extension table.
///
/// Implementations should store goal-specific rows beside, not inside, the
/// canonical [`TaskRecord`]. `create_goal` exists only to preserve the atomic
/// transaction boundary between the generic task row and its goal extension.
#[async_trait::async_trait]
pub trait GoalTaskRegistry: Send + Sync {
    /// Atomically insert the canonical task row and its goal extension row.
    ///
    /// The caller supplies both records because they are different sources of
    /// truth: [`TaskRecord`] owns lifecycle/routing/ownership, while
    /// [`GoalTaskRecord`] owns objective/limits/pause details. Implementations
    /// must reject mismatched ids or non-goal task kinds rather than repairing
    /// them silently.
    async fn create_goal(
        &self,
        task: TaskRecord,
        goal: GoalTaskRecord,
        continuation_context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()>;

    /// Resolve the latest non-terminal goal task for `agent` directly from the
    /// canonical task table. This is a read-only resolver, not cached state.
    async fn latest_active_goal_for_agent(&self, agent: &str)
    -> anyhow::Result<Option<TaskRecord>>;

    /// Resolve the latest non-terminal goal for the trusted runtime context.
    ///
    /// Route and principal filters are matched against canonical `TaskRecord`
    /// fields when present. Callers that have no route/principal context pass
    /// `None` and intentionally fall back to agent-scoped resolution.
    async fn latest_active_goal_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> anyhow::Result<Option<TaskRecord>>;

    /// Resolve only the id of the latest non-terminal goal for the trusted
    /// runtime context. This is a read-only projection from `tasks.id`, used
    /// by hot attribution paths that do not need the full task record.
    async fn latest_active_goal_id_for_context(
        &self,
        agent: &str,
        originator_route: Option<&str>,
        principal_id: Option<&str>,
    ) -> anyhow::Result<Option<String>>;

    /// Load the goal extension row for a canonical task id.
    ///
    /// Absence means either the task is not a goal or the extension row is
    /// missing. Callers that require lifecycle state must pair this with a
    /// fresh canonical [`TaskRecord`] read; the extension row alone is not a
    /// complete lifecycle view.
    async fn get_goal_task(&self, task_id: &str) -> anyhow::Result<Option<GoalTaskRecord>>;

    /// Replace the canonical objective text for a goal.
    ///
    /// Objective text is goal-specific untrusted prompt input, so it belongs on
    /// the goal extension row rather than the generic task row. Callers must
    /// resolve lifecycle and visibility from the canonical [`TaskRecord`]
    /// before invoking this mutation; the store only owns the extension write.
    async fn update_goal_objective(&self, task_id: &str, objective: &str) -> anyhow::Result<()>;

    /// Replace the persisted effective budget limits for a goal.
    ///
    /// These are creation/update-time policy limits only. Consumed and
    /// remaining usage stay derived from canonical usage ledger rows.
    async fn update_goal_limits(
        &self,
        task_id: &str,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
    ) -> anyhow::Result<()>;

    /// Replace only the goal-extension pause payload.
    ///
    /// This is for extension-only repairs or tests that already control the
    /// canonical lifecycle row. Runtime pause/resume paths must use
    /// [`GoalTaskRegistry::pause_goal_task_if_status`] or
    /// [`GoalTaskRegistry::resume_paused_goal_task`] so `tasks.status` and
    /// `goal_tasks` cannot diverge.
    async fn update_goal_pause(
        &self,
        task_id: &str,
        pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()>;

    /// Atomically persist a pause only if the canonical task is still in the
    /// expected lifecycle state.
    ///
    /// A controller command resolves a task before it mutates it. The expected
    /// state prevents that stale read from overwriting an operator or terminal
    /// transition that won the race in the meantime.
    async fn pause_goal_task_if_status(
        &self,
        task_id: &str,
        expected_status: TaskStatus,
        pause: GoalPauseState,
    ) -> anyhow::Result<bool>;

    /// Atomically cancel a goal only if its lifecycle state still matches the
    /// controller's resolved target. A false result means a concurrent state
    /// transition won and no terminal output or error was written.
    async fn cancel_goal_task_if_status(
        &self,
        task_id: &str,
        expected_status: TaskStatus,
        error: String,
    ) -> anyhow::Result<bool>;

    /// Atomically complete exactly a running goal. A false result means a
    /// concurrent pause/cancel/terminal transition won the lifecycle race.
    async fn complete_running_goal_task(
        &self,
        task_id: &str,
        output: String,
    ) -> anyhow::Result<bool>;

    /// Complete a running goal only when its canonical effective limits still
    /// match the policy that was verified. This prevents a verifier result from
    /// bypassing an operator budget update that landed while it was running.
    async fn complete_running_goal_task_if_limits(
        &self,
        task_id: &str,
        token_limit: Option<u64>,
        cost_limit_usd: Option<f64>,
        output: String,
    ) -> anyhow::Result<bool>;
    /// Atomically clear goal pause state, claim ownership, and mark the task running.
    ///
    /// `continuation_context` is written only when supplied by trusted ingress.
    /// This keeps manual resume, budget-triggered resume, and future recovery
    /// paths from splitting lifecycle and goal-extension updates across
    /// independent writes.
    async fn resume_paused_goal_task(
        &self,
        task_id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
        continuation_context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<bool>;

    /// Replace or clear the continuation delivery context for a task.
    ///
    /// The context is control-plane-owned delivery state, not lifecycle state.
    /// It is consumed when resume, budget, or restart-recovery paths need to
    /// enqueue a trusted continuation prompt through the channel runtime.
    async fn set_continuation_context(
        &self,
        task_id: &str,
        context: Option<TaskContinuationContext>,
    ) -> anyhow::Result<()>;

    /// Load the durable channel continuation context for a task, when present.
    ///
    /// The returned context is delivery metadata only. It does not authorize a
    /// resume by itself; callers still need trusted admission/recovery context
    /// before injecting a synthetic continuation into a channel loop.
    async fn get_continuation_context(
        &self,
        task_id: &str,
    ) -> anyhow::Result<Option<TaskContinuationContext>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_restart_pause_reason_uses_rfc_name_with_legacy_alias() {
        // Restart recovery is an RFC-visible persisted reason; old local rows
        // that used the draft spelling must continue to deserialize.
        let serialized = serde_json::to_string(&GoalPauseReason::DaemonRestart).unwrap();
        assert_eq!(serialized, "\"daemon_restarted\"");

        let legacy: GoalPauseReason = serde_json::from_str("\"daemon_restart\"").unwrap();
        assert_eq!(legacy, GoalPauseReason::DaemonRestart);
    }

    #[test]
    fn operator_pause_reason_roundtrips_with_control_plane_name() {
        // `/goal pause` is a controller request, not a human escalation. Its
        // persisted reason must stay distinguishable for status and resume
        // policy.
        let serialized = serde_json::to_string(&GoalPauseReason::OperatorPaused).unwrap();
        assert_eq!(serialized, "\"operator_paused\"");

        let parsed: GoalPauseReason = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, GoalPauseReason::OperatorPaused);
    }

    #[test]
    fn goal_task_loads_without_effective_limits() {
        let legacy = r#"{
            "task_id": "goal-1",
            "objective": "ship goal mode"
        }"#;
        let rec: GoalTaskRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(rec.task_id, "goal-1");
        assert!(rec.effective_token_limit.is_none());
        assert!(rec.effective_cost_limit_usd.is_none());
        assert!(rec.pause_reason.is_none());
        assert!(rec.pause_description.is_none());
        assert!(rec.blockers.is_empty());
    }
}
