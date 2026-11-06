//! Canonical shared types for durable runtime tasks.

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
    /// Stable, globally unique, never-reused task id. Producers validate it
    /// at registration boundaries; hard disposal must not make an id eligible
    /// for reuse because Goal epoch fences are scoped to this identity.
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
    /// Ordered caller aliases from the root delegating agent to the creating
    /// caller, inclusive; the last element equals `originator_route` when both
    /// are set. Delegate rows record it so any ancestor in a bounded delegation
    /// chain can read, await, list or cancel a task its subtree started. Empty
    /// for non-delegate producers and for rows written before it existed.
    #[serde(default)]
    pub originator_chain: Vec<String>,
    /// Whether user-visible completion delivery has been confirmed.
    #[serde(default)]
    pub delivered: bool,
    /// Optional idempotency key for completion/delivery operations.
    #[serde(default)]
    pub idem_key: Option<String>,
    /// Principal recorded by delegate producers as the tool-loop session key
    /// that launched the task. Forensic only: the delegate owner check consults
    /// `originator_route` and `originator_chain`, never this field.
    #[serde(default)]
    pub principal_id: Option<String>,
    /// Canonical session binding for Goal tasks. Legacy and unrelated tasks may omit it.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Durable executor fence for session-bound Goal tasks.
    #[serde(default)]
    pub execution_epoch: i64,
    /// Task registration/start timestamp in RFC3339 form.
    pub started_at: String,
    /// Terminal transition timestamp in RFC3339 form.
    #[serde(default)]
    pub finished_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_kind_serialization_names_are_stable() {
        for (kind, json) in [
            (TaskKind::Delegate, "\"delegate\""),
            (TaskKind::Subagent, "\"subagent\""),
            (TaskKind::Goal, "\"goal\""),
            (TaskKind::PeerInbox, "\"peer_inbox\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), json);
            assert_eq!(serde_json::from_str::<TaskKind>(json).unwrap(), kind);
        }
    }

    #[test]
    fn task_status_serialization_and_terminal_classification_are_stable() {
        for (status, json, terminal) in [
            (TaskStatus::Running, "\"running\"", false),
            (TaskStatus::Paused, "\"paused\"", false),
            (TaskStatus::Completed, "\"completed\"", true),
            (TaskStatus::Failed, "\"failed\"", true),
            (TaskStatus::Cancelled, "\"cancelled\"", true),
            (TaskStatus::Lost, "\"lost\"", true),
            (TaskStatus::TimedOut, "\"timed_out\"", true),
        ] {
            assert_eq!(serde_json::to_string(&status).unwrap(), json);
            assert_eq!(serde_json::from_str::<TaskStatus>(json).unwrap(), status);
            assert_eq!(status.is_terminal(), terminal);
        }
    }

    #[test]
    fn legacy_task_record_deserializes_with_new_fields_defaulted() {
        let legacy = r#"{
            "id": "11111111-1111-1111-1111-111111111111",
            "kind": "delegate",
            "agent": "main",
            "status": "running",
            "started_at": "2026-06-18T00:00:00Z"
        }"#;

        let record: TaskRecord = serde_json::from_str(legacy).unwrap();

        assert_eq!(record.owner_pid, 0);
        assert_eq!(record.owner_boot_id, "");
        assert_eq!(record.heartbeat_at, None);
        assert_eq!(record.depth, 0);
        assert_eq!(record.parent_id, None);
        assert_eq!(record.originator_route, None);
        assert!(record.originator_chain.is_empty());
        assert!(!record.delivered);
        assert_eq!(record.idem_key, None);
        assert_eq!(record.principal_id, None);
        assert_eq!(record.session_id, None);
        assert_eq!(record.execution_epoch, 0);
        assert_eq!(record.finished_at, None);
    }

    #[test]
    fn task_record_serialization_preserves_the_persisted_shape() {
        let record = TaskRecord {
            id: "task-1".to_owned(),
            kind: TaskKind::Goal,
            agent: "main".to_owned(),
            status: TaskStatus::Paused,
            owner_pid: 42,
            owner_boot_id: "boot-a".to_owned(),
            heartbeat_at: Some("2026-06-18T00:01:00Z".to_owned()),
            depth: 2,
            parent_id: Some("parent-1".to_owned()),
            originator_route: Some("worker".to_owned()),
            originator_chain: vec!["root".to_owned(), "worker".to_owned()],
            delivered: true,
            idem_key: Some("delivery-1".to_owned()),
            principal_id: Some("principal-1".to_owned()),
            session_id: Some("session-1".to_owned()),
            execution_epoch: 3,
            started_at: "2026-06-18T00:00:00Z".to_owned(),
            finished_at: Some("2026-06-18T00:02:00Z".to_owned()),
        };

        assert_eq!(
            serde_json::to_value(record).unwrap(),
            serde_json::json!({
                "id": "task-1",
                "kind": "goal",
                "agent": "main",
                "status": "paused",
                "owner_pid": 42,
                "owner_boot_id": "boot-a",
                "heartbeat_at": "2026-06-18T00:01:00Z",
                "depth": 2,
                "parent_id": "parent-1",
                "originator_route": "worker",
                "originator_chain": ["root", "worker"],
                "delivered": true,
                "idem_key": "delivery-1",
                "principal_id": "principal-1",
                "session_id": "session-1",
                "execution_epoch": 3,
                "started_at": "2026-06-18T00:00:00Z",
                "finished_at": "2026-06-18T00:02:00Z"
            })
        );
    }
}
