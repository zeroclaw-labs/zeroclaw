//! Compatibility re-exports for the Goal contracts now owned by
//! `zeroclaw-goal`.

pub use zeroclaw_goal::task::{
    GoalAccountingState, GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState,
    GoalTaskRecord, GoalTaskRegistry, GoalTransitionResult, TaskContinuationContext,
    TaskContinuationConversationScope, TaskGoal,
};
