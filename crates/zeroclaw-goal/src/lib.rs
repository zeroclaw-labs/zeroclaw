//! Shared, transport-neutral Goal Mode behavior and contracts.
//!
//! This crate owns Goal policy and session-bound interfaces. Concrete storage,
//! provider routing, agent execution, and transport presentation remain with
//! their existing owners.

pub mod accounting;
pub mod control;
pub mod task;

pub use control::{
    GoalController, GoalExecutionEngine, GoalExecutionHost, GoalExecutionOutcome,
    GoalExecutionRequest, GoalExecutionScope, GoalHostSettings, GoalIngressContext,
    GoalIngressPrincipal, GoalParentTurn, GoalResponse, GoalRuntime, GoalRuntimeSubmission,
    GoalSessionBinding, GoalSessionDriver, GoalSessionExecutionLease, GoalSessionKey,
    GoalSessionLease, GoalStatusProjection, GoalSubmission, GoalSurface, GoalVerifierAttribution,
    GoalVerifierResult, GoalVerifierTurn,
};
pub use task::{
    GoalAccountingState, GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState,
    GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult, TaskContinuationContext,
    TaskContinuationConversationScope, TaskGoal,
};
