//! The durable run/task control-plane — supervised lifecycle for delegated and
//! subagent runs.
//!
//! A NEW small-module tree (modular-architecture) behind compiler-enforced traits, so
//! the megafiles (`tools/delegate.rs`, `tools/spawn_subagent.rs`, `daemon/mod.rs`)
//! change only as thin wiring at named seams:
//!   * [`task_registry`] — the `TaskRegistry` trait + `TaskRecord`/`TaskKind`/`TaskStatus`.
//!   * [`goal_task`] — goal-mode extension records and repository operations.
//!   * [`task_store_sqlite`] — the single SQLite impl, modelled on
//!     `zeroclaw_infra::acp_session_store`.
//!   * [`authority`] — `is_authoritative`: the runtime-authority reclaim guard.
//!   * [`reaper`] — the periodic sweep + one-shot startup crash-recovery pass.
//!   * [`boot`] — the per-run [`ControlPlaneHandle`].
//!   * [`global`] — the process-global accessor producers reach the handle through.
//!
//! Producers (background `delegate`, `spawn_subagent`) register a task before the run
//! and resolve it on completion; the reaper reconciles an abandoned task to a terminal
//! state (`Lost`/`TimedOut`) from OUTSIDE the task body.

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
    GoalAdmission, GoalAdmissionContext, GoalCommand, GoalCommandAction, GoalTurnEvaluation,
    admit_goal_autonomous_turn, admit_goal_command, current_goal_admission_context,
    current_goal_start_tool_batch_requested, current_goal_turn_evaluation_marker,
    current_goal_turn_evaluation_requested, evaluate_goal_turn, evaluate_goal_turn_with_verifier,
    goal_recovery_status_message, mark_current_goal_turn_for_evaluation,
    pause_current_goal_for_human_gate, scope_goal_admission_context, scope_goal_start_tool_batch,
    scope_goal_state_updates, scope_goal_turn_evaluation_marker,
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
