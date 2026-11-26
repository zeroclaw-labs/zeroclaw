//! Shared Goal Mode budget values.

/// Normalized limits. `None` means unlimited in that dimension.
///
/// Admission determines whether an entirely unlimited value was explicitly
/// selected; this value alone does not distinguish defaults from commands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalBudgetLimits {
    pub token_limit: Option<u64>,
    pub cost_limit_usd: Option<f64>,
}

/// Whether a finite token limit is positive and fits the task plane.
pub fn is_valid_finite_goal_token_limit(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}

/// Whether a finite cost limit is usable for Goal admission.
pub fn is_valid_finite_goal_cost_limit(value: f64) -> bool {
    value.is_finite() && value > 0.0
}
