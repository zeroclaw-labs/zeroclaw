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

/// An ordinary spend-admission refusal carrying the producing runtime
/// boundary's checked, operation-local tool-phase evidence.
///
/// The original native error and its contexts remain available through
/// [`std::error::Error::source`]. This wrapper does not classify that source,
/// assert accounting admission or settlement, or authorize a resumable pause.
/// Consumers must check the decisive native stop, accounting and execution
/// ownership independently. A non-clean wrapper anywhere in a nested source
/// chain remains disqualifying even if an outer boundary reports a clean phase.
#[derive(Debug)]
pub struct GoalOrdinarySpendRefusal {
    source: anyhow::Error,
    clean_tool_phase: bool,
}

impl GoalOrdinarySpendRefusal {
    /// Retain the exact refusal source and the producing runtime boundary's
    /// explicitly checked phase result. `true` requires positive evidence that
    /// no local tool batch began or every begun batch closed correctly; missing,
    /// open, invalid or failed cleanup evidence must be supplied as `false`.
    #[must_use]
    pub fn new(source: anyhow::Error, clean_tool_phase: bool) -> Self {
        Self {
            source,
            clean_tool_phase,
        }
    }

    /// Whether this producing boundary positively established a clean phase.
    /// This does not override negative evidence from an inner boundary.
    #[must_use]
    pub fn clean_tool_phase(&self) -> bool {
        self.clean_tool_phase
    }
}

impl std::fmt::Display for GoalOrdinarySpendRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(if self.clean_tool_phase {
            "Goal ordinary spend admission refused with a checked clean tool phase"
        } else {
            "Goal ordinary spend admission refused without a clean tool phase"
        })
    }
}

impl std::error::Error for GoalOrdinarySpendRefusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Goal accounting made incomplete by an observed condition, closing admission
/// to every further Goal-owned model call. The message embeds each original
/// error chain unchanged. Bare failures have no source, so their plain and
/// alternate displays both show the complete diagnostic exactly once. The
/// operation-source bridge retains the original error as its source.
#[derive(Debug)]
pub struct GoalAccountingFailure {
    message: String,
    state: crate::task::GoalAccountingState,
    pending: bool,
    source: Option<anyhow::Error>,
}

impl GoalAccountingFailure {
    /// Report accounting errors after known usage and its classification are durable.
    #[must_use]
    pub fn settled(state: crate::task::GoalAccountingState, causes: &[String]) -> Self {
        use crate::task::GoalAccountingState;

        let label = match state {
            GoalAccountingState::Complete => "complete",
            GoalAccountingState::Missing => "missing",
            GoalAccountingState::Invalid => "invalid",
            GoalAccountingState::OutcomeUnknown => "outcome-unknown",
        };
        Self {
            message: format!(
                "Goal accounting is {label} after a durable usage settlement; no further Goal model call will be admitted: {}",
                causes.join("; ")
            ),
            state,
            pending: false,
            source: None,
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
        Self {
            message,
            state,
            pending,
            source: None,
        }
    }

    /// Accounting classification carried by this failure.
    #[must_use]
    pub fn accounting_state(&self) -> crate::task::GoalAccountingState {
        self.state
    }

    /// Whether an earlier operation remains pending.
    #[must_use]
    pub fn has_pending_operation(&self) -> bool {
        self.pending
    }

    /// Preserve known metadata across a foreign error wrapper, retaining the operation error.
    #[must_use]
    pub fn with_operation_source(
        state: crate::task::GoalAccountingState,
        pending: bool,
        source: anyhow::Error,
    ) -> Self {
        let mut failure = Self::incomplete(state, pending);
        failure.source = Some(source);
        failure
    }

    /// Add settlement diagnostics without replacing the original operation error.
    #[must_use]
    pub fn preserving_primary_error(
        primary: anyhow::Error,
        settlement_error: anyhow::Error,
    ) -> anyhow::Error {
        let marker = settlement_error.downcast_ref::<Self>().or_else(|| {
            settlement_error
                .chain()
                .find_map(|cause| cause.downcast_ref::<Self>())
        });
        let diagnostic = format!("{settlement_error:#}");
        if let Some(marker) = marker {
            let mut context = Self::incomplete(marker.state, marker.pending);
            context.message = diagnostic;
            primary.context(context)
        } else {
            primary.context(diagnostic)
        }
    }
}

impl std::fmt::Display for GoalAccountingFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GoalAccountingFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_ref().map(|source| source.as_ref())
    }
}

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

/// Diagnostic for an incomplete physical attempt, independent of any usage row.
#[derive(Clone, Debug)]
pub struct GoalAccountingIssue {
    pub provider_ref: String,
    pub model: String,
    pub accounting_state: crate::task::GoalAccountingState,
    /// Diagnostic only; callers must not parse this to determine policy.
    pub reason: String,
}

/// Full accounting projection for one admitted logical Goal operation.
#[derive(Clone, Debug)]
pub struct GoalOperationSettlement {
    pub accounting_state: crate::task::GoalAccountingState,
    /// Known usage is retained even when the overall operation is incomplete.
    pub events: Vec<GoalUsageEvent>,
    /// Ordered incomplete-attempt diagnostics; empty for a clean operation.
    pub issues: Vec<GoalAccountingIssue>,
}

impl GoalOperationSettlement {
    /// Combine issue states and reject malformed routes or usage events while
    /// retaining the original diagnostics and rows for the accountant.
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

        for issue in &self.issues {
            self.accounting_state = combine_accounting_state(
                self.accounting_state,
                if issue.accounting_state == GoalAccountingState::Complete {
                    GoalAccountingState::Invalid
                } else {
                    issue.accounting_state
                },
            );
            if issue.provider_ref.trim().is_empty() || issue.model.trim().is_empty() {
                self.accounting_state =
                    combine_accounting_state(self.accounting_state, GoalAccountingState::Invalid);
            }
        }
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
    /// Admit work only with complete durable accounting. Incomplete accounting
    /// returns a [`GoalAccountingFailure`], independently of the Goal budget.
    async fn admit(&self, request: GoalOperationRequest) -> anyhow::Result<()>;
    /// Persist known usage and its accounting classification before reopening
    /// admission. `Ok` means durable, Complete settlement without an accounting
    /// error. A runtime-settled [`GoalAccountingFailure`] acknowledges durable
    /// settlement and closed admission; admission refusals and diagnostic wrappers
    /// do not themselves assert settlement. Other errors may leave pending evidence.
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

    #[derive(Debug)]
    struct ForeignSpendWrapper(anyhow::Error);

    impl std::fmt::Display for ForeignSpendWrapper {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("foreign spend wrapper")
        }
    }

    impl std::error::Error for ForeignSpendWrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(self.0.as_ref())
        }
    }

    #[test]
    fn ordinary_spend_refusal_keeps_checked_phase_and_each_diagnostic_once() {
        for clean in [false, true] {
            let source = anyhow::Error::new(Primary).context("ordinary admission detail");
            let refusal = GoalOrdinarySpendRefusal::new(source, clean);
            assert_eq!(refusal.clean_tool_phase(), clean);
            let expected = if clean {
                "Goal ordinary spend admission refused with a checked clean tool phase"
            } else {
                "Goal ordinary spend admission refused without a clean tool phase"
            };
            assert_eq!(refusal.to_string(), expected);
            assert_eq!(
                std::error::Error::source(&refusal).unwrap().to_string(),
                "ordinary admission detail"
            );
            let error = anyhow::Error::new(refusal);
            assert_eq!(
                format!("{error:#}"),
                format!("{expected}: ordinary admission detail: primary provider failure")
            );
            assert!(error.chain().any(|cause| cause.is::<Primary>()));
            assert!(
                !error
                    .chain()
                    .any(|cause| cause.is::<GoalAccountingFailure>())
            );
        }
    }

    #[test]
    fn ordinary_spend_refusal_preserves_native_stop_through_foreign_sources() {
        use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode, tag, turn_stop};

        let original = tag(
            anyhow::Error::new(Primary).context("original admission context"),
            TurnStop::fatal(TurnStopCode::BudgetExhausted, "ordinary budget exhausted"),
        );
        let original_diagnostic = format!("{original:#}");
        let refusal =
            GoalOrdinarySpendRefusal::new(anyhow::Error::new(ForeignSpendWrapper(original)), true);
        let error = anyhow::Error::new(ForeignSpendWrapper(anyhow::Error::new(refusal)))
            .context("outer admission context");

        assert_eq!(
            turn_stop(&error).unwrap().code,
            TurnStopCode::BudgetExhausted
        );
        assert_eq!(
            turn_stop(&error).unwrap().detail,
            "ordinary budget exhausted"
        );
        assert!(error.chain().any(|cause| cause.is::<Primary>()));
        let markers = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<GoalOrdinarySpendRefusal>())
            .collect::<Vec<_>>();
        assert_eq!(markers.len(), 1);
        assert!(markers[0].clean_tool_phase());
        let text = format!("{error:#}");
        assert!(text.ends_with(&original_diagnostic), "{text}");
        assert_eq!(text.matches("outer admission context").count(), 1, "{text}");
        // The existing native tag displays its cause and exposes that same
        // cause as its source. Preserve that diagnostic without copying it.
        for detail in ["original admission context", "primary provider failure"] {
            assert_eq!(
                text.matches(detail).count(),
                original_diagnostic.matches(detail).count(),
                "{text}"
            );
        }
    }

    #[test]
    fn ordinary_spend_refusal_outer_clean_phase_preserves_inner_negative_proof() {
        use zeroclaw_api::turn_stop::{TurnStop, TurnStopCode, turn_stop};

        let original = anyhow::Error::new(TurnStop::fatal(
            TurnStopCode::BudgetExhausted,
            "ordinary budget exhausted",
        ));
        let inner = GoalOrdinarySpendRefusal::new(original, false);
        let outer = GoalOrdinarySpendRefusal::new(
            anyhow::Error::new(ForeignSpendWrapper(anyhow::Error::new(inner)))
                .context("enclosing operation"),
            true,
        );
        let error = anyhow::Error::new(outer);

        assert!(
            error
                .downcast_ref::<GoalOrdinarySpendRefusal>()
                .unwrap()
                .clean_tool_phase()
        );
        let phases = error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<GoalOrdinarySpendRefusal>())
            .map(GoalOrdinarySpendRefusal::clean_tool_phase)
            .collect::<Vec<_>>();
        assert_eq!(phases, [true, false]);
        assert_eq!(
            turn_stop(&error).unwrap().code,
            TurnStopCode::BudgetExhausted
        );
    }

    #[test]
    fn accounting_failure_display_is_its_complete_diagnostic() {
        // A bare durable accounting failure renders each original cause exactly once.
        let failure = GoalAccountingFailure::settled(
            crate::task::GoalAccountingState::OutcomeUnknown,
            &["cause one".to_owned(), "cause two".to_owned()],
        );
        let expected = "Goal accounting is outcome-unknown after a durable usage settlement; no further Goal model call will be admitted: cause one; cause two";
        assert_eq!(failure.to_string(), expected);
        assert_eq!(
            failure.accounting_state(),
            crate::task::GoalAccountingState::OutcomeUnknown
        );
        assert!(!failure.has_pending_operation());
        assert!(std::error::Error::source(&failure).is_none());
        let error = anyhow::Error::new(failure);
        assert_eq!(error.to_string(), expected);
        assert_eq!(format!("{error:#}"), expected);
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
            let failure = GoalAccountingFailure::incomplete(state, pending);
            assert_eq!(failure.to_string(), expected);
            assert_eq!(failure.accounting_state(), state);
            assert_eq!(failure.has_pending_operation(), pending);
            assert!(std::error::Error::source(&failure).is_none());
        }
    }

    #[derive(Debug)]
    struct Primary;
    impl std::fmt::Display for Primary {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("primary provider failure")
        }
    }
    impl std::error::Error for Primary {}

    #[derive(Debug)]
    struct ForeignWrapper(GoalAccountingFailure);
    impl std::fmt::Display for ForeignWrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("foreign settlement wrapper")
        }
    }
    impl std::error::Error for ForeignWrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn primary_error_and_full_secondary_chain_survive_combination() {
        use crate::task::GoalAccountingState::OutcomeUnknown;
        let secondary = anyhow::Error::new(ForeignWrapper(GoalAccountingFailure::incomplete(
            OutcomeUnknown,
            true,
        )))
        .context("outer settlement detail");
        let combined =
            GoalAccountingFailure::preserving_primary_error(anyhow::Error::new(Primary), secondary);
        assert!(combined.downcast_ref::<Primary>().is_some());
        let marker = combined.downcast_ref::<GoalAccountingFailure>().unwrap();
        assert_eq!(marker.accounting_state(), OutcomeUnknown);
        assert!(marker.has_pending_operation());
        let text = format!("{combined:#}");
        for detail in [
            "outer settlement detail",
            "foreign settlement wrapper",
            "outcome of an earlier Goal model call is unknown",
            "primary provider failure",
        ] {
            assert_eq!(text.matches(detail).count(), 1, "{text}");
        }

        let plain = GoalAccountingFailure::preserving_primary_error(
            anyhow::Error::new(Primary),
            anyhow::Error::msg("ledger failure").context("storage wrapper"),
        );
        assert!(plain.downcast_ref::<GoalAccountingFailure>().is_none());
        assert!(plain.downcast_ref::<Primary>().is_some());
        assert_eq!(
            format!("{plain:#}"),
            "storage wrapper: ledger failure: primary provider failure"
        );

        let wrapper = GoalAccountingFailure::with_operation_source(OutcomeUnknown, true, combined);
        assert_eq!(wrapper.accounting_state(), OutcomeUnknown);
        assert!(wrapper.has_pending_operation());
        let mut source = std::error::Error::source(&wrapper);
        let mut saw_primary = false;
        while let Some(cause) = source {
            saw_primary |= cause.downcast_ref::<Primary>().is_some();
            source = cause.source();
        }
        assert!(saw_primary);
    }

    #[test]
    fn normalization_combines_ordered_issues_and_rejects_inconsistent_inputs() {
        use crate::task::GoalAccountingState::{Complete, Invalid, Missing, OutcomeUnknown};
        let issue = |state, provider: &str, model: &str, reason: &str| GoalAccountingIssue {
            provider_ref: provider.to_owned(),
            model: model.to_owned(),
            accounting_state: state,
            reason: reason.to_owned(),
        };
        let mut settlement = GoalOperationSettlement {
            accounting_state: Complete,
            events: vec![GoalUsageEvent {
                provider_ref: "b".into(),
                model: "model-b".into(),
                usage: TokenUsage {
                    input_tokens: Some(7),
                    output_tokens: Some(3),
                    cached_input_tokens: None,
                    cache_creation_input_tokens: None,
                },
                priceable: true,
            }],
            issues: vec![
                issue(Missing, "a", "model-a", "missing"),
                issue(Invalid, "c", "model-c", "MissingOutput"),
                issue(OutcomeUnknown, "d", "model-d", "unknown"),
            ],
        };
        settlement.normalize();
        assert_eq!(settlement.accounting_state, OutcomeUnknown);
        assert_eq!(settlement.events.len(), 1);
        assert_eq!(
            settlement
                .issues
                .iter()
                .map(|i| i.reason.as_str())
                .collect::<Vec<_>>(),
            ["missing", "MissingOutput", "unknown"]
        );
        for invalid in [
            issue(Complete, "a", "m", "contradiction"),
            issue(Missing, " ", "m", "blank route"),
            issue(Missing, "a", " ", "blank model"),
        ] {
            let mut direct = GoalOperationSettlement {
                accounting_state: Complete,
                events: vec![],
                issues: vec![invalid],
            };
            direct.normalize();
            assert_eq!(direct.accounting_state, Invalid);
            assert_eq!(direct.issues.len(), 1);
        }
        let mut direct = GoalOperationSettlement {
            accounting_state: Complete,
            events: vec![],
            issues: vec![issue(Missing, "a", "m", "missing")],
        };
        direct.normalize();
        assert_eq!(direct.accounting_state, Missing);
        settlement.events[0].usage.input_tokens = None;
        settlement.accounting_state = Complete;
        settlement.issues.clear();
        settlement.normalize();
        assert_eq!(settlement.accounting_state, Invalid);
        assert_eq!(settlement.events.len(), 1);
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
            issues: vec![],
        };

        settlement.normalize();

        assert_eq!(
            settlement.accounting_state,
            crate::task::GoalAccountingState::Invalid
        );
    }
}
