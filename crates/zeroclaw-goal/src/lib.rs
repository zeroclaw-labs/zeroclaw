//! Shared, transport-neutral Goal Mode behavior and contracts.
//!
//! This crate owns Goal policy and session-bound interfaces. Concrete storage,
//! provider routing, agent execution, and transport presentation remain with
//! their existing owners.

pub mod control;
pub mod task;

pub use control::{
    GoalController, GoalExecutionHost, GoalExecutionScope, GoalHostSettings, GoalIngressContext,
    GoalIngressPrincipal, GoalParentTurn, GoalResponse, GoalSessionBinding, GoalSessionDriver,
    GoalSessionExecutionLease, GoalSessionKey, GoalSessionLease, GoalStatusProjection,
    GoalSubmission, GoalSurface, GoalVerifierTurn,
};
pub use task::{
    GoalAccountingState, GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState,
    GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult, TaskContinuationContext,
    TaskContinuationConversationScope, TaskGoal,
};
