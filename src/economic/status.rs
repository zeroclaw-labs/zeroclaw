//! Survival status tracking for economic agents.
//!
//! Defines the health states an agent can be in based on remaining balance
//! as a percentage of initial capital.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Survival status based on balance percentage relative to initial capital.
///
/// Mirrors the ClawWork LiveBench agent survival states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurvivalStatus {
    /// Balance > 80% of initial - Agent is profitable and healthy
    Thriving,
    /// Balance 40-80% of initial - Agent is maintaining stability
    Stable,
    /// Balance 10-40% of initial - Agent is losing money, needs attention
    Struggling,
    /// Balance 1-10% of initial - Agent is near death, urgent intervention needed
    Critical,
    /// Balance <= 0 - Agent has exhausted resources and cannot operate
    Bankrupt,
}

impl SurvivalStatus {
    /// Calculate survival status from current and initial balance.
    ///
    /// # Arguments
    /// * `current_balance` - Current remaining balance
    /// * `initial_balance` - Starting balance
    ///
    /// # Returns
    /// The appropriate `SurvivalStatus` based on the percentage remaining.
    pub fn from_balance(current_balance: f64, initial_balance: f64) -> Self {
        if initial_balance <= 0.0 {
            // Edge case: if initial was zero or negative, can't calculate percentage
            return if current_balance <= 0.0 {
                Self::Bankrupt
            } else {
                Self::Thriving
            };
        }

        let percentage = (current_balance / initial_balance) * 100.0;

        match percentage {
            p if p <= 0.0 => Self::Bankrupt,
            p if p < 10.0 => Self::Critical,
            p if p < 40.0 => Self::Struggling,
            p if p < 80.0 => Self::Stable,
            _ => Self::Thriving,
        }
    }

    /// Check if the agent can still operate (not bankrupt).
    pub fn is_operational(&self) -> bool {
        !matches!(self, Self::Bankrupt)
    }

    /// Check if the agent needs urgent attention.
    pub fn needs_intervention(&self) -> bool {
        matches!(self, Self::Critical | Self::Bankrupt)
    }

    /// Get a human-readable emoji indicator.
    pub fn emoji(&self) -> &'static str {
        match self {
            Self::Thriving => "🌟",
            Self::Stable => "✅",
            Self::Struggling => "⚠️",
            Self::Critical => "🚨",
            Self::Bankrupt => "💀",
        }
    }

    /// Get a color code for terminal output (ANSI).
    pub fn ansi_color(&self) -> &'static str {
        match self {
            Self::Thriving => "\x1b[32m",   // Green
            Self::Stable => "\x1b[34m",     // Blue
            Self::Struggling => "\x1b[33m", // Yellow
            Self::Critical => "\x1b[31m",   // Red
            Self::Bankrupt => "\x1b[35m",   // Magenta
        }
    }
}

impl fmt::Display for SurvivalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match self {
            Self::Thriving => "Thriving",
            Self::Stable => "Stable",
            Self::Struggling => "Struggling",
            Self::Critical => "Critical",
            Self::Bankrupt => "Bankrupt",
        };
        write!(f, "{}", status)
    }
}

impl Default for SurvivalStatus {
    fn default() -> Self {
        Self::Stable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thriving_above_80_percent() {
        assert_eq!(
            SurvivalStatus::from_balance(900.0, 1000.0),
            SurvivalStatus::Thriving
        );
        assert_eq!(
            SurvivalStatus::from_balance(1500.0, 1000.0), // Profit!
            SurvivalStatus::Thriving
        );
        assert_eq!(
            SurvivalStatus::from_balance(800.01, 1000.0),
            SurvivalStatus::Thriving
        );
    }

    #[test]
    fn stable_between_40_and_80_percent() {
        assert_eq!(
            SurvivalStatus::from_balance(799.99, 1000.0),
            SurvivalStatus::Stable
        );
        assert_eq!(
            SurvivalStatus::from_balance(500.0, 1000.0),
            SurvivalStatus::Stable
        );
        assert_eq!(
            SurvivalStatus::from_balance(400.01, 1000.0),
            SurvivalStatus::Stable
        );
    }

    #[test]
    fn struggling_between_10_and_40_percent() {
        assert_eq!(
            SurvivalStatus::from_balance(399.99, 1000.0),
            SurvivalStatus::Struggling
        );
        assert_eq!(
            SurvivalStatus::from_balance(200.0, 1000.0),
            SurvivalStatus::Struggling
        );
        assert_eq!(
            SurvivalStatus::from_balance(100.01, 1000.0),
            SurvivalStatus::Struggling
        );
    }

    #[test]
    fn critical_between_0_and_10_percent() {
        assert_eq!(
            SurvivalStatus::from_balance(99.99, 1000.0),
            SurvivalStatus::Critical
        );
        assert_eq!(
            SurvivalStatus::from_balance(50.0, 1000.0),
            SurvivalStatus::Critical
        );
        assert_eq!(
            SurvivalStatus::from_balance(0.01, 1000.0),
            SurvivalStatus::Critical
        );
    }

    #[test]
    fn bankrupt_at_zero_or_negative() {
        assert_eq!(
            SurvivalStatus::from_balance(0.0, 1000.0),
            SurvivalStatus::Bankrupt
        );
        assert_eq!(
            SurvivalStatus::from_balance(-100.0, 1000.0),
            SurvivalStatus::Bankrupt
        );
    }

    #[test]
    fn is_operational() {
        assert!(SurvivalStatus::Thriving.is_operational());
        assert!(SurvivalStatus::Stable.is_operational());
        assert!(SurvivalStatus::Struggling.is_operational());
        assert!(SurvivalStatus::Critical.is_operational());
        assert!(!SurvivalStatus::Bankrupt.is_operational());
    }

    #[test]
    fn needs_intervention() {
        assert!(!SurvivalStatus::Thriving.needs_intervention());
        assert!(!SurvivalStatus::Stable.needs_intervention());
        assert!(!SurvivalStatus::Struggling.needs_intervention());
        assert!(SurvivalStatus::Critical.needs_intervention());
        assert!(SurvivalStatus::Bankrupt.needs_intervention());
    }

    #[test]
    fn display_format() {
        assert_eq!(format!("{}", SurvivalStatus::Thriving), "Thriving");
        assert_eq!(format!("{}", SurvivalStatus::Bankrupt), "Bankrupt");
    }
}
