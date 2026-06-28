//! The durable task/run registry contract — EPIC A's stable seam.
//!
//! One trait, backed once by SQLite (`task_store_sqlite.rs`), that every
//! spawned/delegated/peer-driven unit of work registers into. This supersedes the
//! flat-file `BackgroundDelegateResult`/`BackgroundTaskStatus`
//! (`tools/delegate.rs`) by adding the terminal-loss states it cannot represent
//! and an out-of-band reconcile seam the reaper drives.
//!
//! Downstream epics EXTEND this surface — EPIC E adds a `RemoteTurn` kind, EPIC C
//! consumes `delivered`/`idem_key`, EPIC D stamps a principal — by adding a field
//! or a trait impl, never by re-opening the supervision logic.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Discriminates which producer registered a unit of work. EXTEND, don't fork:
/// EPIC E adds `RemoteTurn`; EPIC B treats a paused task as a supervised *status*,
/// not a new kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Delegate,
    Subagent,
    Goal,
    PeerInbox,
    // EPIC E: RemoteTurn
}

/// The task state machine. Supersedes `BackgroundTaskStatus` (delegate.rs) by
/// ADDING the terminal-loss states a fire-and-forget task cannot write for itself.
/// `snake_case` repr keeps on-disk JSON stable: the legacy
/// `running|completed|failed|cancelled` values still parse unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    /// Written by the reaper/recovery sweep from OUTSIDE the task body — the state
    /// today's enum literally cannot represent (task-lifecycle-supervision gap).
    Lost,
    /// Heartbeat exceeded its grace window / the task passed `max_runtime`.
    TimedOut,
}

/// Goal-specific extension record keyed by the canonical task id.
///
/// Lifecycle, ownership, route, principal, timestamps, and terminal state stay on
/// [`TaskRecord`]. This record owns only goal-specific state that has no meaning
/// for delegates/subagents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalTaskRecord {
    pub task_id: String,
    pub objective: String,
    #[serde(default)]
    pub effective_token_limit: Option<u64>,
    #[serde(default)]
    pub effective_cost_limit_usd: Option<f64>,
    #[serde(default)]
    pub pause_reason: Option<GoalPauseReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GoalBlocker>,
}

/// Typed policy input for why a goal is paused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPauseReason {
    NeedsUserInput,
    HumanEscalation,
    ExternalDependency,
    ProviderUnavailable,
    VerifierBlocked,
    DaemonRestart,
}

/// Structured blocker packet attached to a paused goal.
///
/// Free-form text is only explanatory. Policy branches on `kind` and payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalBlocker {
    pub kind: GoalBlockerKind,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalBlockerKind {
    NeedsUserInput,
    HumanEscalation,
    ExternalDependency,
    Provider,
    Verifier,
    RestartRecovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalPauseState {
    pub reason: GoalPauseReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<GoalBlocker>,
}

impl TaskStatus {
    /// A task is terminal once it can no longer transition. The reaper only
    /// reconciles non-terminal records.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Lost
                | TaskStatus::TimedOut
        )
    }
}

/// The durable record. New fields are `#[serde(default)]` so pre-existing on-disk
/// payloads load unchanged; downstream epics ADD fields here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// UUID — validated at the producer boundary (reuse `validate_task_id`).
    pub id: String,
    pub kind: TaskKind,
    pub agent: String,
    pub status: TaskStatus,
    /// OS pid of the daemon that created the task; paired with `owner_boot_id` so a
    /// recycled pid on a later boot is not mistaken for the live owner.
    #[serde(default)]
    pub owner_pid: u32,
    /// Daemon run-id; survives PID reuse and distinguishes a prior-boot orphan from
    /// a live same-boot task.
    #[serde(default)]
    pub owner_boot_id: String,
    #[serde(default)]
    pub heartbeat_at: Option<String>,
    /// GOVERNOR: monotonic, persisted recursion depth.
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub parent_id: Option<String>,
    /// EPIC B threads the originator's HITL reply-target across the spawn boundary.
    #[serde(default)]
    pub originator_route: Option<String>,
    /// EPIC C delivery evidence.
    #[serde(default)]
    pub delivered: bool,
    /// EPIC C idempotency key.
    #[serde(default)]
    pub idem_key: Option<String>,
    /// EPIC D attribution (Principal co-design `COORD-principal-contract.md` §7/R3): the
    /// authenticated `Principal.id` that originated this run. Additive and unstamped today:
    /// stored as `Option<String>` (a serialization-friendly primitive) and left `None` until
    /// EPIC D wires population from the now-landed `zeroclaw_api::principal::PrincipalId`; the
    /// type swaps to `Option<PrincipalId>` as part of that wiring.
    /// It resolves to the carried `Principal.id` (never a bare principal-`None`).
    #[serde(default)]
    pub principal_id: Option<String>,
    pub started_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
}

/// THE stable seam. One trait, backed once by SQLite. The ACP session store and the
/// delegate/subagent/peer producers all converge here (CROSS-CUTTING epic-A D1).
#[async_trait::async_trait]
pub trait TaskRegistry: Send + Sync {
    /// Register a new unit of work. Idempotent on `rec.id`.
    async fn create(&self, rec: TaskRecord) -> anyhow::Result<()>;
    /// Register a goal task and its goal-specific extension atomically.
    ///
    /// `TaskRecord` remains the canonical lifecycle/route/principal record;
    /// `GoalTaskRecord` remains the goal-only extension. This method only owns
    /// the transaction boundary between those two source-of-truth rows.
    async fn create_goal(&self, task: TaskRecord, goal: GoalTaskRecord) -> anyhow::Result<()>;
    /// Stamp a liveness beat for `id` from the heart-beating owner.
    async fn heartbeat(&self, id: &str, owner_boot_id: &str) -> anyhow::Result<()>;
    /// Transition `id` to `status`, optionally recording terminal output/error.
    async fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> anyhow::Result<()>;
    async fn get(&self, id: &str) -> anyhow::Result<Option<TaskRecord>>;
    async fn list_running(&self) -> anyhow::Result<Vec<TaskRecord>>;
    async fn list_by_agent(&self, agent: &str) -> anyhow::Result<Vec<TaskRecord>>;
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
    async fn create_goal_task(&self, rec: GoalTaskRecord) -> anyhow::Result<()>;
    async fn get_goal_task(&self, task_id: &str) -> anyhow::Result<Option<GoalTaskRecord>>;
    async fn update_goal_pause(
        &self,
        task_id: &str,
        pause: Option<GoalPauseState>,
    ) -> anyhow::Result<()>;
    /// Reaper/recovery seam: mark a record terminal-loss ONLY when this process is
    /// authoritative for it. Returns `false` (no write) when another live daemon
    /// owns it. See [`crate::control_plane::authority::is_authoritative`].
    async fn reconcile_lost(&self, id: &str, now_boot_id: &str) -> anyhow::Result<bool>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_status_values_still_parse() {
        // Backward-compat: pre-EPIC-A on-disk values must deserialize unchanged.
        for (json, want) in [
            ("\"running\"", TaskStatus::Running),
            ("\"paused\"", TaskStatus::Paused),
            ("\"completed\"", TaskStatus::Completed),
            ("\"failed\"", TaskStatus::Failed),
            ("\"cancelled\"", TaskStatus::Cancelled),
        ] {
            let got: TaskStatus = serde_json::from_str(json).unwrap();
            assert_eq!(got, want, "legacy status {json} must parse");
        }
    }

    #[test]
    fn goal_kind_roundtrips_snake_case() {
        let s = serde_json::to_string(&TaskKind::Goal).unwrap();
        assert_eq!(s, "\"goal\"");
        let back: TaskKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, TaskKind::Goal);
    }

    #[test]
    fn new_loss_states_roundtrip_snake_case() {
        for st in [TaskStatus::Lost, TaskStatus::TimedOut] {
            let s = serde_json::to_string(&st).unwrap();
            assert!(s == "\"lost\"" || s == "\"timed_out\"", "got {s}");
            let back: TaskStatus = serde_json::from_str(&s).unwrap();
            assert_eq!(back, st);
            assert!(st.is_terminal());
        }
    }

    #[test]
    fn paused_status_is_non_terminal() {
        assert!(!TaskStatus::Paused.is_terminal());
    }

    #[test]
    fn record_loads_without_new_fields() {
        // An old payload carrying only the original columns must deserialize, with
        // the EPIC-A/B/C/D fields defaulting.
        let legacy = r#"{
            "id": "11111111-1111-1111-1111-111111111111",
            "kind": "delegate",
            "agent": "main",
            "status": "running",
            "started_at": "2026-06-18T00:00:00Z"
        }"#;
        let rec: TaskRecord = serde_json::from_str(legacy).unwrap();
        assert_eq!(rec.depth, 0);
        assert_eq!(rec.owner_pid, 0);
        assert!(!rec.delivered);
        assert!(rec.parent_id.is_none());
        assert!(rec.originator_route.is_none());
        assert!(rec.principal_id.is_none()); // EPIC-D attribution not yet stamped; absent
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
