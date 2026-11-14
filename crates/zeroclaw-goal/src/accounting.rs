//! Goal-owned contracts for fencing and settling provider operations.

use std::sync::Arc;

use async_trait::async_trait;
use zeroclaw_api::model_provider::TokenUsage;

use crate::control::GoalExecutionScope;
use crate::task::GoalDispatchStart;

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

/// Goal accounting made incomplete by an observed condition, closing admission
/// to every further Goal-owned model call. Delivered errors retain their original
/// detail; failures retained by an accounting owner are scrubbed before reuse.
/// There is no source, so both displays show the complete diagnostic once.
#[derive(Clone, Debug)]
pub struct GoalAccountingFailure {
    message: String,
}

/// Whether an unsettled operation had entered provider dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalUnsettledOperation {
    NeverDispatched,
    Dispatched,
}

impl GoalAccountingFailure {
    /// Report accounting errors after known usage and its classification are durable.
    #[must_use]
    pub fn settled(state: crate::task::GoalAccountingState, causes: &[String]) -> Self {
        use crate::task::GoalAccountingState;

        let state = match state {
            GoalAccountingState::Complete => "complete",
            GoalAccountingState::Missing => "missing",
            GoalAccountingState::Invalid => "invalid",
            GoalAccountingState::OutcomeUnknown => "outcome-unknown",
        };
        Self {
            message: format!(
                "Goal accounting is {state} after a durable usage settlement; no further Goal model call will be admitted: {}",
                causes.join("; ")
            ),
        }
    }

    /// Name the durable conditions that prevent another Goal-owned model call.
    #[must_use]
    pub fn incomplete(state: crate::task::GoalAccountingState, pending: bool) -> Self {
        use crate::task::GoalAccountingState;

        let mut conditions = Vec::new();
        match state {
            GoalAccountingState::Complete => {}
            GoalAccountingState::Missing => {
                conditions.push("usage was not reported for an earlier Goal model call");
            }
            GoalAccountingState::Invalid => {
                conditions.push(
                    "usage for an earlier Goal model call was invalid or could not be priced",
                );
            }
            GoalAccountingState::OutcomeUnknown => {
                conditions.push("the outcome of an earlier Goal model call is unknown");
            }
        }
        if pending {
            conditions.push("an earlier Goal model call has not settled");
        }
        let message = if conditions.is_empty() {
            "Goal accounting is incomplete; no further Goal model call will be admitted".to_owned()
        } else {
            format!(
                "Goal accounting is incomplete: {}; no further Goal model call will be admitted",
                conditions.join("; ")
            )
        };
        Self { message }
    }

    /// Report a failed durable operation without conflating an unstarted call
    /// with an unknown provider outcome.
    #[must_use]
    pub fn unsettled(operation: GoalUnsettledOperation, detail: &str) -> Self {
        let condition = match operation {
            GoalUnsettledOperation::NeverDispatched => {
                "an admitted Goal model call that was never dispatched could not be released, so it remains pending"
            }
            GoalUnsettledOperation::Dispatched => {
                "usage settlement for an earlier Goal model call did not become durable, so the call may remain pending"
            }
        };
        Self {
            message: format!(
                "Goal accounting is incomplete: {condition}; no further Goal model call will be admitted: {detail}"
            ),
        }
    }

    /// Report an admission whose durable result was not delivered to its caller.
    #[must_use]
    pub fn admission_unconfirmed(detail: &str) -> Self {
        Self {
            message: format!(
                "Goal accounting is incomplete: the admission of a Goal model call that was never dispatched did not finish, so the call may remain pending; no further Goal model call will be admitted: {detail}"
            ),
        }
    }

    /// Return a copy whose diagnostic has passed through the caller's scrubber.
    #[must_use]
    pub fn redacted(&self, scrub: impl FnOnce(&str) -> String) -> Self {
        Self {
            message: scrub(&self.message),
        }
    }

    pub(crate) fn for_operation(mut self, operation_error: &anyhow::Error) -> Self {
        self.message
            .push_str(&format!("; original operation error: {operation_error:#}"));
        self
    }

    pub(crate) fn incomplete_for_operation(
        state: crate::task::GoalAccountingState,
        pending: bool,
        operation_error: &anyhow::Error,
    ) -> Self {
        let mut failure = Self::incomplete(state, pending);
        failure
            .message
            .push_str(&format!("; original operation error: {operation_error:#}"));
        failure
    }
}

impl std::fmt::Display for GoalAccountingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GoalAccountingFailure {}

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

/// Keeps the admission's liveness witness for the caller's operation scope.
/// Dropping it before settlement lets the accounting owner settle abandoned work.
#[must_use = "the admission guard must live until settlement or release"]
pub struct GoalOperationGuard {
    _witness: Box<dyn Send>,
}

impl GoalOperationGuard {
    /// Wrap an accounting owner's operation-lifetime witness.
    pub fn new(witness: impl Send + 'static) -> Self {
        Self {
            _witness: Box::new(witness),
        }
    }
}

/// A one-shot source of the best available settlement when a caller is dropped.
pub type GoalAbandonmentSettlement = Box<dyn FnOnce() -> GoalOperationSettlement + Send>;

/// A single admitted Goal-owned model operation settles after its physical
/// attempts have reported or been classified as outcome-unknown.
#[async_trait]
pub trait GoalOperationAccounting: Send + Sync {
    /// Admit work only with complete durable accounting. Incomplete accounting
    /// returns a [`GoalAccountingFailure`], independently of the Goal budget.
    /// Once the durable admission starts, dropping the future does not cancel it.
    /// An undelivered applied admission is released, a definite refusal is left
    /// unchanged, and an uncertain failure is retained with scrubbed detail.
    async fn admit(&self, request: GoalOperationRequest) -> anyhow::Result<()>;
    /// Obtain the witness immediately after admission, before yielding. Hold it
    /// through settlement or release, including if the caller is cancelled.
    fn admission_guard(&self) -> anyhow::Result<GoalOperationGuard>;
    /// Mark the first poll of the admitted provider dispatch.
    fn mark_dispatch_entered(&self) {}
    /// Replace the current admission's source for abandonment settlement.
    fn set_abandonment_settlement(&self, _source: GoalAbandonmentSettlement) {}
    /// Wait for outstanding local accounting work, including any retained
    /// failure, to become observable.
    async fn quiesce(&self) {}
    /// Return an undelivered owner-side failure, with sensitive detail scrubbed.
    fn unsettled_failure(&self) -> Option<GoalAccountingFailure> {
        None
    }
    /// After awaited pre-dispatch work, check the admitted operation while
    /// acquiring the registry's session gate. This read-only check takes no
    /// additional operation permit. On error the caller must not dispatch and
    /// must release the unstarted admission. The guard orders dispatch entry,
    /// not physical transmission, against fences using the same registry
    /// instance. Retries and fallback within an entered operation are not gated.
    async fn begin_dispatch(&self) -> anyhow::Result<GoalDispatchStart>;
    /// Persist known usage and its accounting classification before reopening
    /// admission. `Ok` means settlement is durable and no accounting error was
    /// observed by the accountant; the reported settlement may itself still be
    /// incomplete. An error carrying [`GoalAccountingFailure`] means settlement
    /// is durable, known usage is recorded, and admission is closed. Any other
    /// error means settlement may not be durable and a pending marker may remain.
    /// Once its durable section starts, dropping this future does not cancel it;
    /// an undelivered failure is retained with scrubbed detail.
    async fn settle(&self, settlement: GoalOperationSettlement) -> anyhow::Result<()>;
    /// Release an admitted operation only when its provider-dispatch future
    /// has never been polled. No physical attempt or usage can exist in that
    /// case. A failed release must leave durable pending evidence intact. Once
    /// its durable section starts, dropping this future does not cancel it; an
    /// undelivered failure is retained with scrubbed detail.
    async fn release_unstarted(&self) -> anyhow::Result<()>;
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
    fn accounting_failure_display_is_its_complete_diagnostic() {
        // A bare durable accounting failure renders each original cause exactly once.
        let failure = GoalAccountingFailure::settled(
            crate::task::GoalAccountingState::OutcomeUnknown,
            &["cause one".to_owned(), "cause two".to_owned()],
        );
        let expected = "Goal accounting is outcome-unknown after a durable usage settlement; no further Goal model call will be admitted: cause one; cause two";
        assert_eq!(failure.to_string(), expected);
        assert!(std::error::Error::source(&failure).is_none());
        let error = anyhow::Error::new(failure);
        assert_eq!(error.to_string(), expected);
        assert_eq!(format!("{error:#}"), expected);
    }

    #[test]
    fn unsettled_failures_distinguish_dispatch_and_retain_scrubbed_detail() {
        let unstarted = GoalAccountingFailure::unsettled(
            GoalUnsettledOperation::NeverDispatched,
            "registry secret",
        );
        assert!(unstarted.to_string().contains("never dispatched"));
        assert!(!unstarted.to_string().contains("outcome of an earlier"));
        let redacted = unstarted.redacted(|message| message.replace("secret", "[redacted]"));
        assert!(redacted.to_string().contains("registry [redacted]"));

        let dispatched = GoalAccountingFailure::unsettled(
            GoalUnsettledOperation::Dispatched,
            "ledger unavailable",
        )
        .for_operation(&anyhow::Error::msg("provider failed"));
        assert!(dispatched.to_string().contains("usage settlement"));
        assert!(dispatched.to_string().contains("ledger unavailable"));
        assert!(
            dispatched
                .to_string()
                .contains("original operation error: provider failed")
        );
    }

    #[test]
    fn unconfirmed_admission_does_not_claim_a_marker_exists() {
        let message = GoalAccountingFailure::admission_unconfirmed("fixture error").to_string();
        assert!(message.starts_with("Goal accounting is incomplete:"));
        assert!(message.ends_with("no further Goal model call will be admitted: fixture error"));
        assert!(message.contains("never dispatched"));
        assert!(message.contains("may remain pending"));
        assert!(!message.contains("could not be released"));
        assert!(!message.contains("outcome of an earlier"));
        assert!(!message.contains("has not settled"));
        assert_eq!(message.matches("fixture error").count(), 1);
    }

    #[test]
    fn incomplete_accounting_failure_names_each_durable_condition() {
        // Every durable condition is named in order, independently of any budget.
        use crate::task::GoalAccountingState::{Complete, Invalid, Missing, OutcomeUnknown};

        for (state, pending, expected) in [
            (
                Missing,
                false,
                "Goal accounting is incomplete: usage was not reported for an earlier Goal model call; no further Goal model call will be admitted",
            ),
            (
                Invalid,
                false,
                "Goal accounting is incomplete: usage for an earlier Goal model call was invalid or could not be priced; no further Goal model call will be admitted",
            ),
            (
                OutcomeUnknown,
                false,
                "Goal accounting is incomplete: the outcome of an earlier Goal model call is unknown; no further Goal model call will be admitted",
            ),
            (
                Invalid,
                true,
                "Goal accounting is incomplete: usage for an earlier Goal model call was invalid or could not be priced; an earlier Goal model call has not settled; no further Goal model call will be admitted",
            ),
            (
                Complete,
                false,
                "Goal accounting is incomplete; no further Goal model call will be admitted",
            ),
        ] {
            assert_eq!(
                GoalAccountingFailure::incomplete(state, pending).to_string(),
                expected
            );
        }
    }

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
