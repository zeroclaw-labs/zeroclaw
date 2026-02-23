//! Survival system — tier-based resource management for autonomous agents.
//!
//! Maps credit balance to survival tiers that control agent behavior:
//! - Model downgrade on low compute
//! - Funding strategy cooldowns per tier
//! - Tier transitions emitted as observer events

use serde::{Deserialize, Serialize};

/// Survival tiers based on credit balance (in cents).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SurvivalTier {
    /// Agent is non-functional (balance < 0)
    Dead,
    /// Minimal operations only (balance >= 0)
    Critical,
    /// Reduced model quality, conserve resources (balance >= 10)
    LowCompute,
    /// Normal operations (balance >= 50)
    Normal,
    /// Full capabilities, premium models available (balance >= 500)
    High,
}

impl SurvivalTier {
    /// Compute tier from credit balance in cents.
    pub fn from_balance(balance_cents: i64) -> Self {
        match balance_cents {
            b if b < 0 => Self::Dead,
            b if b < 10 => Self::Critical,
            b if b < 50 => Self::LowCompute,
            b if b < 500 => Self::Normal,
            _ => Self::High,
        }
    }

    /// Whether the agent should operate at reduced capacity.
    pub fn is_degraded(self) -> bool {
        matches!(self, Self::Dead | Self::Critical | Self::LowCompute)
    }

    /// Whether the agent can operate at all.
    pub fn is_alive(self) -> bool {
        !matches!(self, Self::Dead)
    }

    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::Dead => "DEAD",
            Self::Critical => "CRITICAL",
            Self::LowCompute => "LOW_COMPUTE",
            Self::Normal => "NORMAL",
            Self::High => "HIGH",
        }
    }
}

impl std::fmt::Display for SurvivalTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Configurable thresholds for survival tiers (in cents).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurvivalThresholds {
    /// Balance threshold for High tier (default: 500 cents)
    pub high: i64,
    /// Balance threshold for Normal tier (default: 50 cents)
    pub normal: i64,
    /// Balance threshold for LowCompute tier (default: 10 cents)
    pub low_compute: i64,
    /// Balance threshold for Critical tier (default: 0 cents)
    pub critical: i64,
}

impl Default for SurvivalThresholds {
    fn default() -> Self {
        Self {
            high: 500,
            normal: 50,
            low_compute: 10,
            critical: 0,
        }
    }
}

impl SurvivalThresholds {
    /// Compute tier from balance using custom thresholds.
    pub fn tier_for_balance(&self, balance_cents: i64) -> SurvivalTier {
        if balance_cents < self.critical {
            SurvivalTier::Dead
        } else if balance_cents < self.low_compute {
            SurvivalTier::Critical
        } else if balance_cents < self.normal {
            SurvivalTier::LowCompute
        } else if balance_cents < self.high {
            SurvivalTier::Normal
        } else {
            SurvivalTier::High
        }
    }
}

/// Monitors agent survival state based on credit balance.
pub struct SurvivalMonitor {
    current_tier: SurvivalTier,
    credit_balance_cents: i64,
    thresholds: SurvivalThresholds,
}

impl SurvivalMonitor {
    /// Create a new survival monitor with initial credit balance.
    pub fn new(initial_balance_cents: i64, thresholds: SurvivalThresholds) -> Self {
        let current_tier = thresholds.tier_for_balance(initial_balance_cents);
        Self {
            current_tier,
            credit_balance_cents: initial_balance_cents,
            thresholds,
        }
    }

    /// Get the current survival tier.
    pub fn tier(&self) -> SurvivalTier {
        self.current_tier
    }

    /// Get the current credit balance in cents.
    pub fn balance_cents(&self) -> i64 {
        self.credit_balance_cents
    }

    /// Deduct credits and return any tier transition that occurred.
    ///
    /// Returns `Some((old_tier, new_tier))` if the tier changed.
    pub fn deduct(&mut self, amount_cents: i64) -> Option<(SurvivalTier, SurvivalTier)> {
        self.credit_balance_cents = self.credit_balance_cents.saturating_sub(amount_cents);
        self.check_tier_transition()
    }

    /// Add credits and return any tier transition that occurred.
    ///
    /// Returns `Some((old_tier, new_tier))` if the tier changed.
    pub fn add_credits(&mut self, amount_cents: i64) -> Option<(SurvivalTier, SurvivalTier)> {
        self.credit_balance_cents = self.credit_balance_cents.saturating_add(amount_cents);
        self.check_tier_transition()
    }

    /// Set the credit balance directly and return any tier transition.
    pub fn set_balance(&mut self, balance_cents: i64) -> Option<(SurvivalTier, SurvivalTier)> {
        self.credit_balance_cents = balance_cents;
        self.check_tier_transition()
    }

    /// Build a status summary for display or tool output.
    pub fn status_summary(&self) -> SurvivalStatus {
        SurvivalStatus {
            tier: self.current_tier,
            balance_cents: self.credit_balance_cents,
            is_alive: self.current_tier.is_alive(),
            is_degraded: self.current_tier.is_degraded(),
        }
    }

    fn check_tier_transition(&mut self) -> Option<(SurvivalTier, SurvivalTier)> {
        let new_tier = self.thresholds.tier_for_balance(self.credit_balance_cents);
        if new_tier == self.current_tier {
            None
        } else {
            let old = self.current_tier;
            self.current_tier = new_tier;
            Some((old, new_tier))
        }
    }
}

/// Snapshot of survival state for reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurvivalStatus {
    pub tier: SurvivalTier,
    pub balance_cents: i64,
    pub is_alive: bool,
    pub is_degraded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_from_default_balance_thresholds() {
        assert_eq!(SurvivalTier::from_balance(-1), SurvivalTier::Dead);
        assert_eq!(SurvivalTier::from_balance(0), SurvivalTier::Critical);
        assert_eq!(SurvivalTier::from_balance(9), SurvivalTier::Critical);
        assert_eq!(SurvivalTier::from_balance(10), SurvivalTier::LowCompute);
        assert_eq!(SurvivalTier::from_balance(49), SurvivalTier::LowCompute);
        assert_eq!(SurvivalTier::from_balance(50), SurvivalTier::Normal);
        assert_eq!(SurvivalTier::from_balance(499), SurvivalTier::Normal);
        assert_eq!(SurvivalTier::from_balance(500), SurvivalTier::High);
        assert_eq!(SurvivalTier::from_balance(10000), SurvivalTier::High);
    }

    #[test]
    fn tier_ordering() {
        assert!(SurvivalTier::Dead < SurvivalTier::Critical);
        assert!(SurvivalTier::Critical < SurvivalTier::LowCompute);
        assert!(SurvivalTier::LowCompute < SurvivalTier::Normal);
        assert!(SurvivalTier::Normal < SurvivalTier::High);
    }

    #[test]
    fn tier_is_degraded() {
        assert!(SurvivalTier::Dead.is_degraded());
        assert!(SurvivalTier::Critical.is_degraded());
        assert!(SurvivalTier::LowCompute.is_degraded());
        assert!(!SurvivalTier::Normal.is_degraded());
        assert!(!SurvivalTier::High.is_degraded());
    }

    #[test]
    fn tier_is_alive() {
        assert!(!SurvivalTier::Dead.is_alive());
        assert!(SurvivalTier::Critical.is_alive());
        assert!(SurvivalTier::LowCompute.is_alive());
        assert!(SurvivalTier::Normal.is_alive());
        assert!(SurvivalTier::High.is_alive());
    }

    #[test]
    fn monitor_initial_tier() {
        let monitor = SurvivalMonitor::new(100, SurvivalThresholds::default());
        assert_eq!(monitor.tier(), SurvivalTier::Normal);
        assert_eq!(monitor.balance_cents(), 100);
    }

    #[test]
    fn monitor_deduct_triggers_tier_change() {
        let mut monitor = SurvivalMonitor::new(55, SurvivalThresholds::default());
        assert_eq!(monitor.tier(), SurvivalTier::Normal);

        // Deduct to LowCompute
        let transition = monitor.deduct(10);
        assert_eq!(
            transition,
            Some((SurvivalTier::Normal, SurvivalTier::LowCompute))
        );
        assert_eq!(monitor.tier(), SurvivalTier::LowCompute);
        assert_eq!(monitor.balance_cents(), 45);
    }

    #[test]
    fn monitor_deduct_no_tier_change() {
        let mut monitor = SurvivalMonitor::new(200, SurvivalThresholds::default());
        assert_eq!(monitor.tier(), SurvivalTier::Normal);

        let transition = monitor.deduct(1);
        assert!(transition.is_none());
        assert_eq!(monitor.balance_cents(), 199);
        assert_eq!(monitor.tier(), SurvivalTier::Normal);
    }

    #[test]
    fn monitor_add_credits_triggers_upgrade() {
        let mut monitor = SurvivalMonitor::new(45, SurvivalThresholds::default());
        assert_eq!(monitor.tier(), SurvivalTier::LowCompute);

        let transition = monitor.add_credits(10);
        assert_eq!(
            transition,
            Some((SurvivalTier::LowCompute, SurvivalTier::Normal))
        );
        assert_eq!(monitor.tier(), SurvivalTier::Normal);
    }

    #[test]
    fn monitor_deduct_to_dead() {
        let mut monitor = SurvivalMonitor::new(5, SurvivalThresholds::default());
        assert_eq!(monitor.tier(), SurvivalTier::Critical);

        let transition = monitor.deduct(10);
        assert_eq!(
            transition,
            Some((SurvivalTier::Critical, SurvivalTier::Dead))
        );
        assert!(!monitor.tier().is_alive());
        assert_eq!(monitor.balance_cents(), -5);
    }

    #[test]
    fn monitor_saturating_operations() {
        let mut monitor = SurvivalMonitor::new(i64::MAX, SurvivalThresholds::default());
        let transition = monitor.add_credits(1);
        assert!(transition.is_none()); // Already at max, still High
        assert_eq!(monitor.balance_cents(), i64::MAX);
    }

    #[test]
    fn custom_thresholds() {
        let thresholds = SurvivalThresholds {
            high: 1000,
            normal: 100,
            low_compute: 25,
            critical: 0,
        };

        assert_eq!(thresholds.tier_for_balance(1000), SurvivalTier::High);
        assert_eq!(thresholds.tier_for_balance(999), SurvivalTier::Normal);
        assert_eq!(thresholds.tier_for_balance(99), SurvivalTier::LowCompute);
        assert_eq!(thresholds.tier_for_balance(24), SurvivalTier::Critical);
        assert_eq!(thresholds.tier_for_balance(-1), SurvivalTier::Dead);
    }

    #[test]
    fn status_summary_reflects_state() {
        let monitor = SurvivalMonitor::new(200, SurvivalThresholds::default());
        let status = monitor.status_summary();

        assert_eq!(status.tier, SurvivalTier::Normal);
        assert_eq!(status.balance_cents, 200);
        assert!(status.is_alive);
        assert!(!status.is_degraded);
    }

    #[test]
    fn tier_display_labels() {
        assert_eq!(SurvivalTier::Dead.to_string(), "DEAD");
        assert_eq!(SurvivalTier::Critical.to_string(), "CRITICAL");
        assert_eq!(SurvivalTier::LowCompute.to_string(), "LOW_COMPUTE");
        assert_eq!(SurvivalTier::Normal.to_string(), "NORMAL");
        assert_eq!(SurvivalTier::High.to_string(), "HIGH");
    }

    #[test]
    fn survival_status_serde_roundtrip() {
        let status = SurvivalStatus {
            tier: SurvivalTier::Normal,
            balance_cents: 250,
            is_alive: true,
            is_degraded: false,
        };

        let json = serde_json::to_string(&status).unwrap();
        let parsed: SurvivalStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tier, SurvivalTier::Normal);
        assert_eq!(parsed.balance_cents, 250);
    }

    #[test]
    fn set_balance_works() {
        let mut monitor = SurvivalMonitor::new(100, SurvivalThresholds::default());
        let transition = monitor.set_balance(5);
        assert_eq!(
            transition,
            Some((SurvivalTier::Normal, SurvivalTier::Critical))
        );
        assert_eq!(monitor.balance_cents(), 5);
    }
}
