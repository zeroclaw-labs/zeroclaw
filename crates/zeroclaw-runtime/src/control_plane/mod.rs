//! The durable run/task control-plane — supervised lifecycle for delegated and
//! subagent runs.

pub mod authority;
pub mod boot;
pub mod global;
pub mod goal;
pub mod goal_task;
pub mod reaper;
pub mod task_registry;
pub mod task_store_sqlite;
pub mod verifier;

pub use authority::is_authoritative;
pub use boot::ControlPlaneHandle;
pub use global::{control_plane, init_control_plane};
pub use goal::{
    GoalAdmission, GoalAdmissionContext, GoalCommand, GoalCommandAction, GoalRuntimeScope,
    GoalTurnEvaluation, admit_goal_autonomous_turn, admit_goal_command, bind_current_goal_task,
    current_goal_admission_context, current_goal_start_tool_batch_requested,
    current_goal_turn_evaluation_marker, current_goal_turn_evaluation_requested,
    evaluate_goal_turn, evaluate_goal_turn_with_verifier, goal_recovery_status_message,
    mark_current_goal_turn_for_evaluation, pause_current_goal_for_human_gate,
    pause_goal_for_accounting_failure, scope_goal_admission_context, scope_goal_runtime,
    scope_goal_start_tool_batch, scope_goal_state_updates, scope_goal_turn_evaluation_marker,
};
pub use goal::{GoalStateUpdateEvent, GoalStateUpdateSink};
pub use goal_task::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord,
    GoalTaskRegistry, TaskContinuationContext, TaskContinuationConversationScope, TaskGoal,
};
pub use task_registry::{TaskKind, TaskRecord, TaskRegistry, TaskStatus};
pub use task_store_sqlite::SqliteTaskStore;
pub use verifier::{
    GoalVerificationRequest, GoalVerifier, GoalVerifierDecision, LlmGoalVerifier,
    ensure_verifier_allows_completion, verifier_outage_pause, verify_goal_completion,
};
