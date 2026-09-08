//! The durable run/task control-plane — supervised lifecycle for delegated and
//! subagent runs.

pub mod authority;
pub mod boot;
pub mod global;
pub mod goal_task;
pub mod reaper;
pub mod task_registry;
pub mod task_store_sqlite;

pub use authority::is_authoritative;
pub use boot::ControlPlaneHandle;
pub(crate) use boot::ControlPlaneRecoveryOwner;
pub use global::control_plane;
pub(crate) use global::{init_control_plane, spawn_control_plane_reaper};
pub use goal_task::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord,
    GoalTaskRegistry, TaskContinuationContext, TaskContinuationConversationScope, TaskGoal,
};
pub use task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskSnapshot, TaskStatus};
pub use task_store_sqlite::SqliteTaskStore;
