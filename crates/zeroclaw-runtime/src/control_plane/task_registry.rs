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
    async fn claim_owner(
        &self,
        id: &str,
        owner_pid: u32,
        owner_boot_id: &str,
    ) -> anyhow::Result<()>;
    async fn get(&self, id: &str) -> anyhow::Result<Option<TaskRecord>>;
    async fn list_running(&self) -> anyhow::Result<Vec<TaskRecord>>;
    async fn list_by_agent(&self, agent: &str) -> anyhow::Result<Vec<TaskRecord>>;
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
}
