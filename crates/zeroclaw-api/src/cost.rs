//! Read-only projections of the canonical cost ledger.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConversationCostStatus {
    Estimated,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConversationCostReason {
    MissingUsage,
    MissingPrices,
    UnverifiedHistory,
    TrackingDisabled,
    TrackingUnavailable,
    NoRecordedUsage,
    LedgerIntegrity,
    PersistenceUncertain,
}

/// An estimate of recorded, attributed usage, never an invoice or a live balance.
/// `None` distinguishes an unavailable estimate from explicitly free usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ConversationCostSummary {
    pub estimated_cost_usd: Option<f64>,
    pub status: ConversationCostStatus,
    pub reasons: Vec<ConversationCostReason>,
}

impl ConversationCostSummary {
    pub fn unavailable(reason: ConversationCostReason) -> Self {
        Self {
            estimated_cost_usd: None,
            status: ConversationCostStatus::Unavailable,
            reasons: vec![reason],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_is_not_serialized_as_zero() {
        let value = serde_json::to_value(ConversationCostSummary::unavailable(
            ConversationCostReason::MissingUsage,
        ))
        .unwrap();
        assert!(value["estimated_cost_usd"].is_null());
        assert_eq!(value["status"], "unavailable");
        assert_eq!(value["reasons"][0], "missing_usage");
    }
}
