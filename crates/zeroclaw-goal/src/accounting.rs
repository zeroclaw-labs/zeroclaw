//! Goal-owned contracts for fencing and settling provider operations.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::model_provider::TokenUsage;

use crate::control::GoalExecutionScope;

/// A Goal operation was refused because its exact execution scope is no longer
/// running. The committed lifecycle transition, not this refusal, decides the
/// execution outcome. This includes a budget-exhaustion pause committed by the
/// accountant itself. Failed pause transitions and other admission failures
/// must not use this marker.
#[derive(Clone, Copy, Debug)]
pub struct GoalOperationFenced {
    message: &'static str,
}

impl GoalOperationFenced {
    /// The canonical session no longer has a current Goal task.
    pub const NO_CURRENT_TASK: Self = Self {
        message: "Goal session no longer has a current task",
    };

    /// The current task differs from the task bound to the execution scope.
    pub const STALE_TASK: Self = Self {
        message: "Goal task identity is stale",
    };

    /// The bound Goal has left its running state.
    pub const NOT_RUNNING: Self = Self {
        message: "Goal task is not running",
    };

    /// The bound Goal has moved to a different execution epoch.
    pub const STALE_EPOCH: Self = Self {
        message: "Goal execution epoch is stale",
    };

    /// Durable pending-operation admission rejected the exact execution fence.
    pub const ADMISSION_FENCE_LOST: Self = Self {
        message: "Goal operation admission lost its execution fence",
    };

    /// The accountant committed a budget-exhaustion pause before refusing work.
    pub const BUDGET_EXHAUSTED: Self = Self {
        message: "Goal budget is exhausted",
    };
}

impl std::fmt::Display for GoalOperationFenced {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for GoalOperationFenced {}

/// Configured route at the start of one logical Goal-owned provider operation.
#[derive(Clone, Debug)]
pub struct GoalOperationRequest {
    pub model_provider: String,
    pub model: String,
}

impl GoalOperationRequest {
    pub fn new(model_provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model_provider: model_provider.into(),
            model: model.into(),
        }
    }
}

/// One observed usage event attributed to the actual provider route.
#[derive(Clone, Debug)]
pub struct GoalUsageEvent {
    pub provider_ref: String,
    pub model: String,
    pub usage: TokenUsage,
    /// False when token dimensions are retained but malformed cache metadata
    /// prevents a trustworthy dollar projection.
    pub priceable: bool,
}

/// Full accounting projection for one admitted logical Goal operation.
#[derive(Clone, Debug)]
pub struct GoalOperationSettlement {
    pub accounting_state: crate::task::GoalAccountingState,
    /// Known usage is retained even when the overall operation is incomplete.
    pub events: Vec<GoalUsageEvent>,
}

impl GoalOperationSettlement {
    /// Reject an apparently complete operation when any event cannot support
    /// a structurally valid durable usage record or safely priced cache split.
    pub fn normalize(&mut self) {
        use crate::task::GoalAccountingState;

        let invalid_event = self.events.iter().any(|event| {
            if !event.priceable
                || event.provider_ref.trim().is_empty()
                || event.model.trim().is_empty()
            {
                return true;
            }
            let Some(input) = event.usage.input_tokens else {
                return true;
            };
            let Some(output) = event.usage.output_tokens else {
                return true;
            };
            let Some(total) = input.checked_add(output) else {
                return true;
            };
            total == 0
                || event.usage.cached_input_tokens.unwrap_or(0) > input
                || event.usage.cache_creation_input_tokens.unwrap_or(0)
                    > input.saturating_sub(event.usage.cached_input_tokens.unwrap_or(0))
        });

        if invalid_event {
            self.accounting_state =
                combine_accounting_state(self.accounting_state, GoalAccountingState::Invalid);
        }
    }
}

pub fn combine_accounting_state(
    current: crate::task::GoalAccountingState,
    next: crate::task::GoalAccountingState,
) -> crate::task::GoalAccountingState {
    use crate::task::GoalAccountingState::{Complete, Invalid, Missing, OutcomeUnknown};

    match (current, next) {
        (OutcomeUnknown, _) | (_, OutcomeUnknown) => OutcomeUnknown,
        (Invalid, _) | (_, Invalid) => Invalid,
        (Missing, _) | (_, Missing) => Missing,
        (Complete, Complete) => Complete,
    }
}

/// Runtime-provided pricing adapter for the canonical cost representation.
/// The Goal component does not embed pricing tables or duplicate the runtime
/// pricing resolver.
pub trait GoalUsagePricer: Send + Sync {
    fn price(
        &self,
        provider_ref: &str,
        model: &str,
        usage: &TokenUsage,
    ) -> anyhow::Result<GoalUsagePrice>;
}

/// Pricing-only result returned by the runtime's existing provider-cost
/// resolver. Token dimensions and model attribution remain sourced from the
/// reported provider usage; runtime combines both into its canonical ledger
/// record.
#[derive(Clone, Debug, PartialEq)]
pub struct GoalUsagePrice {
    pub cost_usd: f64,
    pub unpriced_tokens: u64,
    pub pricing_available: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// A single admitted Goal-owned model operation can settle only after all
/// physical attempts in the ordinary provider path have reported.
#[async_trait]
pub trait GoalOperationAccounting: Send + Sync {
    async fn admit(&self, request: GoalOperationRequest) -> anyhow::Result<()>;
    async fn settle(&self, settlement: GoalOperationSettlement) -> anyhow::Result<()>;
    async fn pricing_complete(&self) -> anyhow::Result<bool>;
}

/// Produces an operation accountant bound to the same durable Goal scope.
/// Runtime owns the concrete ledger adapter and injects its shared pricing
/// implementation when constructing this factory.
pub trait GoalOperationAccountingFactory: Send + Sync {
    fn for_scope(&self, scope: GoalExecutionScope) -> Arc<dyn GoalOperationAccounting>;
}

tokio::task_local! {
    /// Provider-call admission and settlement scope for one Goal epoch.
    pub static GOAL_OPERATION_ACCOUNTING: Option<Arc<dyn GoalOperationAccounting>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_fence_errors_preserve_the_five_refusal_texts_without_a_source() {
        let cases = [
            (
                GoalOperationFenced::NO_CURRENT_TASK,
                "Goal session no longer has a current task",
            ),
            (
                GoalOperationFenced::STALE_TASK,
                "Goal task identity is stale",
            ),
            (GoalOperationFenced::NOT_RUNNING, "Goal task is not running"),
            (
                GoalOperationFenced::STALE_EPOCH,
                "Goal execution epoch is stale",
            ),
            (
                GoalOperationFenced::ADMISSION_FENCE_LOST,
                "Goal operation admission lost its execution fence",
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            assert!(std::error::Error::source(&error).is_none());
            let erased = anyhow::Error::new(error);
            assert!(erased.downcast_ref::<GoalOperationFenced>().is_some());
        }
    }

    #[test]
    fn a_complete_operation_cannot_contain_an_unpriceable_cache_partition() {
        let mut settlement = GoalOperationSettlement {
            accounting_state: crate::task::GoalAccountingState::Complete,
            events: vec![GoalUsageEvent {
                provider_ref: "provider.primary".to_owned(),
                model: "model-a".to_owned(),
                usage: TokenUsage {
                    input_tokens: Some(41),
                    output_tokens: Some(2),
                    cached_input_tokens: Some(0),
                    cache_creation_input_tokens: Some(0),
                },
                priceable: false,
            }],
        };

        settlement.normalize();

        assert_eq!(
            settlement.accounting_state,
            crate::task::GoalAccountingState::Invalid
        );
    }
}
