//! The durable task/run registry contract — EPIC A's stable seam.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Background delegation task.
    Delegate,
    /// Subagent task spawned under the runtime.
    Subagent,
    /// Goal-mode task. Goal-specific state lives in the goal extension table;
    /// lifecycle and route ownership still live on [`TaskRecord`].
    Goal,
    /// Peer inbox task.
    PeerInbox,
    // EPIC E: RemoteTurn
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task is currently eligible to execute or already executing.
    Running,
    /// Task is intentionally stopped but resumable.
    Paused,
    /// Task finished successfully.
    Completed,
    /// Task ended with an error.
    Failed,
    /// Task was intentionally cancelled.
    Cancelled,
    /// Written by the reaper/recovery sweep from OUTSIDE the task body — the state
    /// today's enum literally cannot represent (task-lifecycle-supervision gap).
    Lost,
    /// Heartbeat exceeded its grace window / the task passed `max_runtime`.
    TimedOut,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    /// Stable task id. Producers validate it at registration boundaries.
    pub id: String,
    /// Durable task domain type.
    pub kind: TaskKind,
    /// Agent alias that owns and executes this task.
    pub agent: String,
    /// Canonical lifecycle state for the task.
    pub status: TaskStatus,
    /// OS pid of the daemon that created the task; paired with `owner_boot_id` so a
    /// recycled pid on a later boot is not mistaken for the live owner.
    #[serde(default)]
    pub owner_pid: u32,
    /// Daemon run-id; survives PID reuse and distinguishes a prior-boot orphan from
    /// a live same-boot task.
    #[serde(default)]
    pub owner_boot_id: String,
    /// Optional owner heartbeat timestamp in RFC3339 form.
    /// Only tasks that actively heartbeat may be timed out by heartbeat age; an
    /// absent heartbeat is not a derived runtime duration.
    #[serde(default)]
    pub heartbeat_at: Option<String>,
    /// Monotonic persisted recursion depth for delegation/subagent governors.
    #[serde(default)]
    pub depth: u32,
    /// Parent task id for synchronous child work, when one exists.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Trusted route/reply target that originated the task.
    /// Goal admission and visibility checks use this canonical route instead of
    /// trusting model-supplied task selectors.
    #[serde(default)]
    pub originator_route: Option<String>,
    /// Whether user-visible completion delivery has been confirmed.
    #[serde(default)]
    pub delivered: bool,
    /// Optional idempotency key for completion/delivery operations.
    #[serde(default)]
    pub idem_key: Option<String>,
    #[serde(default)]
    pub principal_id: Option<String>,
    /// Task registration/start timestamp in RFC3339 form.
    pub started_at: String,
    /// Terminal transition timestamp in RFC3339 form.
    #[serde(default)]
    pub finished_at: Option<String>,
}

/// One atomic read of a task's lifecycle and terminal payload columns.
///
/// `TaskRecord` remains the shared lifecycle record used by generic task
/// producers. Consumers that need terminal output use this view so status and
/// payload cannot come from different reads or stores.
#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub task: TaskRecord,
    pub output: Option<String>,
    pub error: Option<String>,
}

/// A durably recorded terminal transition that has not yet been applied to the
/// canonical [`TaskRecord`]. The intent is recovery metadata, not a second
/// lifecycle authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSettlementIntent {
    pub task_id: String,
    pub owner_pid: u32,
    pub owner_boot_id: String,
    pub desired_status: TaskStatus,
    pub artifact_path: String,
    pub artifact_ref: Option<String>,
    pub artifact_sha256: String,
    pub terminal_error: Option<String>,
}

/// THE stable seam. One trait, backed once by SQLite. The ACP session store and the
/// delegate/subagent/peer producers all converge here (CROSS-CUTTING epic-A D1).
#[async_trait::async_trait]
pub trait TaskRegistry: Send + Sync {
    /// Register a new unit of work. Idempotent on `rec.id`.
    async fn create(&self, rec: TaskRecord) -> anyhow::Result<()>;
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
    /// Atomically settle a non-terminal task and record its terminal payload.
    /// Returns `true` only for the caller that won the terminal transition.
    async fn transition_terminal(
        &self,
        id: &str,
        status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> anyhow::Result<bool> {
        let _ = (id, status, output, error);
        anyhow::bail!("task registry does not support atomic terminal transitions")
    }
    /// Record an unapplied terminal transition before its artifact is published.
    /// Returns `false` when the task is no longer non-terminal or owner-matched.
    async fn persist_terminal_settlement_intent(
        &self,
        intent: TerminalSettlementIntent,
    ) -> anyhow::Result<bool> {
        let _ = intent;
        anyhow::bail!("task registry does not support terminal settlement intents")
    }
    /// List terminal transitions that still need recovery.
    async fn list_terminal_settlement_intents(
        &self,
    ) -> anyhow::Result<Vec<TerminalSettlementIntent>> {
        anyhow::bail!("task registry does not support terminal settlement intents")
    }
    /// Apply a settlement intent and delete that unchanged intent in one SQLite
    /// transaction. `resolved_status` is normally the intent's desired status;
    /// recovery may conservatively resolve a corrupt artifact as `Failed`.
    async fn promote_terminal_settlement(
        &self,
        intent: &TerminalSettlementIntent,
        resolved_status: TaskStatus,
        output: Option<String>,
        error: Option<String>,
    ) -> anyhow::Result<bool> {
        let _ = (intent, resolved_status, output, error);
        anyhow::bail!("task registry does not support terminal settlement intents")
    }
    /// Remove an unchanged stale intent without changing the canonical task row.
    async fn discard_terminal_settlement_intent(
        &self,
        intent: &TerminalSettlementIntent,
    ) -> anyhow::Result<bool> {
        let _ = intent;
        anyhow::bail!("task registry does not support terminal settlement intents")
    }
    async fn claim_owner(
        &self,
        id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> anyhow::Result<()>;
    async fn get(&self, id: &str) -> anyhow::Result<Option<TaskRecord>>;
    async fn get_snapshot(&self, id: &str) -> anyhow::Result<Option<TaskSnapshot>> {
        Ok(self.get(id).await?.map(|task| TaskSnapshot {
            task,
            output: None,
            error: None,
        }))
    }
    async fn list_running(&self) -> anyhow::Result<Vec<TaskRecord>>;
    async fn list_by_agent(&self, agent: &str) -> anyhow::Result<Vec<TaskRecord>>;
    /// Reaper/recovery seam: mark a record terminal-loss ONLY when this process is
    /// authoritative for it. Returns `false` (no write) when another live process
    /// owns it. `now_boot_id` remains part of the recovery seam for reaper filtering.
    /// See [`crate::control_plane::authority::is_authoritative`].
    async fn reconcile_lost(&self, id: &str, now_boot_id: &str) -> anyhow::Result<bool>;
    /// Mark a stale-heartbeat task timed out only if the observed owner and
    /// heartbeat still match. Returns `false` when liveness changed first.
    async fn reconcile_timed_out(
        &self,
        id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
        heartbeat_at: &str,
    ) -> anyhow::Result<bool> {
        let _ = (id, owner_pid, owner_boot_id, heartbeat_at);
        anyhow::bail!("task registry does not support atomic timeout reconciliation")
    }
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
}
