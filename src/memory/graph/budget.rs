//! Token budget controller for graph synthesis operations.
//!
//! Synchronizes with the existing CostTracker to enforce daily token
//! and cost limits for LLM-powered graph operations (REM synthesis, research).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Budget controller for graph synthesis operations.
pub struct BudgetController {
    /// Daily token budget for synthesis operations.
    daily_budget_tokens: u64,
    /// Daily cost cap in USD (0.0 = unlimited).
    daily_cost_cap_usd: f64,
    /// Tokens consumed today by synthesis operations.
    tokens_consumed: Arc<AtomicU64>,
}

impl BudgetController {
    /// Create a new budget controller.
    pub fn new(daily_budget_tokens: u64, daily_cost_cap_usd: f64) -> Self {
        Self {
            daily_budget_tokens,
            daily_cost_cap_usd,
            tokens_consumed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Check if a synthesis operation with estimated token cost is allowed.
    ///
    /// `current_daily_cost_usd` should come from `CostTracker::get_summary().daily_cost_usd`.
    pub fn can_spend(&self, estimated_tokens: u64, current_daily_cost_usd: f64) -> bool {
        // Check cost cap
        if self.daily_cost_cap_usd > 0.0 && current_daily_cost_usd >= self.daily_cost_cap_usd {
            return false;
        }

        // Check token budget
        let consumed = self.tokens_consumed.load(Ordering::Relaxed);
        consumed + estimated_tokens <= self.daily_budget_tokens
    }

    /// Record tokens consumed by a synthesis operation.
    pub fn record_spend(&self, tokens: u64) {
        self.tokens_consumed.fetch_add(tokens, Ordering::Relaxed);
    }

    /// Get remaining token budget for today.
    pub fn remaining_tokens(&self) -> u64 {
        let consumed = self.tokens_consumed.load(Ordering::Relaxed);
        self.daily_budget_tokens.saturating_sub(consumed)
    }

    /// Get total tokens consumed today.
    pub fn consumed_tokens(&self) -> u64 {
        self.tokens_consumed.load(Ordering::Relaxed)
    }

    /// Reset daily counters (called at day boundary).
    pub fn reset_daily(&self) {
        self.tokens_consumed.store(0, Ordering::Relaxed);
    }

    /// Get the configured daily budget.
    pub fn daily_budget(&self) -> u64 {
        self.daily_budget_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_budget_allows_spending() {
        let bc = BudgetController::new(50_000, 0.0);
        assert!(bc.can_spend(1000, 0.0));
        assert_eq!(bc.remaining_tokens(), 50_000);
    }

    #[test]
    fn budget_exhaustion_blocks_spending() {
        let bc = BudgetController::new(1000, 0.0);
        bc.record_spend(900);
        assert!(bc.can_spend(100, 0.0));
        assert!(!bc.can_spend(101, 0.0));
    }

    #[test]
    fn cost_cap_blocks_spending() {
        let bc = BudgetController::new(50_000, 1.0);
        assert!(!bc.can_spend(100, 1.0));
        assert!(!bc.can_spend(100, 1.5));
        assert!(bc.can_spend(100, 0.5));
    }

    #[test]
    fn reset_restores_budget() {
        let bc = BudgetController::new(1000, 0.0);
        bc.record_spend(800);
        assert_eq!(bc.remaining_tokens(), 200);
        bc.reset_daily();
        assert_eq!(bc.remaining_tokens(), 1000);
    }

    #[test]
    fn unlimited_cost_cap() {
        let bc = BudgetController::new(50_000, 0.0);
        // Even with huge daily cost, 0.0 cap means unlimited
        assert!(bc.can_spend(100, 999.0));
    }
}
