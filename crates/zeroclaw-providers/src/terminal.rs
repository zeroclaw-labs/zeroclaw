//! Provider-owned policy for incomplete terminal responses.
//!
//! `zeroclaw_api::TerminalCompletionFailure` deliberately remains a small,
//! stable protocol error. This module carries delivery and accounting policy
//! through internal error chains without changing that public shape.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use zeroclaw_api::model_provider::{
    SemanticEmptyTerminalFailure, StreamError, TerminalCompletionError, TerminalCompletionFailure,
    terminal_completion_failure,
};

#[derive(Debug, Clone)]
struct PublishedTerminalPolicy {
    reason: TerminalCompletionError,
    policy: TerminalCompletionPolicy,
}

#[derive(Debug, Default)]
pub(crate) struct TerminalPolicySlot(Mutex<Option<PublishedTerminalPolicy>>);

thread_local! {
    static ACTIVE_TERMINAL_POLICY_SLOT: RefCell<Option<Arc<TerminalPolicySlot>>> = const { RefCell::new(None) };
}

pub(crate) struct TerminalPolicyScope(Option<Arc<TerminalPolicySlot>>);

impl Drop for TerminalPolicyScope {
    fn drop(&mut self) {
        ACTIVE_TERMINAL_POLICY_SLOT.with(|active| *active.borrow_mut() = self.0.take());
    }
}

pub(crate) fn enter_terminal_policy_scope() -> (Arc<TerminalPolicySlot>, TerminalPolicyScope) {
    let slot = Arc::new(TerminalPolicySlot::default());
    let previous =
        ACTIVE_TERMINAL_POLICY_SLOT.with(|active| active.borrow_mut().replace(slot.clone()));
    (slot, TerminalPolicyScope(previous))
}

pub(crate) fn capture_terminal_policy_slot() -> Option<Arc<TerminalPolicySlot>> {
    ACTIVE_TERMINAL_POLICY_SLOT.with(|active| active.borrow().clone())
}

pub(crate) fn publish_terminal_policy(
    slot: &Option<Arc<TerminalPolicySlot>>,
    reason: TerminalCompletionError,
    policy: TerminalCompletionPolicy,
) {
    if let Some(slot) = slot {
        let mut published = slot
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if published.is_none() {
            *published = Some(PublishedTerminalPolicy { reason, policy });
        }
    }
}

pub(crate) fn contextualize_terminal_stream_error(
    slot: &Arc<TerminalPolicySlot>,
    error: StreamError,
) -> anyhow::Error {
    let failure = error.terminal_completion_failure().cloned();
    let published = slot
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match (failure, published) {
        (Some(failure), Some(published)) if failure.reason == published.reason => {
            terminal_completion_context_error(failure, published.policy)
        }
        _ => anyhow::Error::from(error),
    }
}

/// Whether the failed request can safely advance to the next configured
/// provider candidate. This never permits replaying the failed candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalRecoveryDisposition {
    NoReplay,
    NextCandidate,
}

/// Whether provider-reported rejected usage contributes to cost accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalUsageChargeability {
    Billable,
    Informational,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCompletionPolicy {
    recovery: TerminalRecoveryDisposition,
    usage_chargeability: TerminalUsageChargeability,
}

impl TerminalCompletionPolicy {
    #[must_use]
    pub const fn new(
        recovery: TerminalRecoveryDisposition,
        usage_chargeability: TerminalUsageChargeability,
    ) -> Self {
        Self {
            recovery,
            usage_chargeability,
        }
    }

    #[must_use]
    pub const fn recovery(self) -> TerminalRecoveryDisposition {
        self.recovery
    }

    #[must_use]
    pub const fn usage_chargeability(self) -> TerminalUsageChargeability {
        self.usage_chargeability
    }
}

/// Default policy for legacy terminal errors that did not carry provider
/// delivery context. It is deliberately conservative for paused turns.
#[must_use]
pub const fn default_terminal_policy(reason: TerminalCompletionError) -> TerminalCompletionPolicy {
    let recovery = match reason {
        TerminalCompletionError::PausedTurn | TerminalCompletionError::InvalidTerminalReason => {
            TerminalRecoveryDisposition::NoReplay
        }
        TerminalCompletionError::OutputTokenLimit
        | TerminalCompletionError::ContextWindow
        | TerminalCompletionError::Refusal => TerminalRecoveryDisposition::NextCandidate,
    };
    TerminalCompletionPolicy::new(recovery, TerminalUsageChargeability::Billable)
}

/// Private-layout contextual error used within provider/runtime composition.
#[derive(Debug)]
pub struct TerminalCompletionContext {
    failure: TerminalCompletionFailure,
    policy: TerminalCompletionPolicy,
}

impl TerminalCompletionContext {
    #[must_use]
    pub fn failure(&self) -> &TerminalCompletionFailure {
        &self.failure
    }

    #[must_use]
    pub const fn policy(&self) -> TerminalCompletionPolicy {
        self.policy
    }
}

impl std::fmt::Display for TerminalCompletionContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.failure.fmt(f)
    }
}

impl std::error::Error for TerminalCompletionContext {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.failure)
    }
}

#[must_use]
pub(crate) fn terminal_completion_context_error(
    failure: TerminalCompletionFailure,
    policy: TerminalCompletionPolicy,
) -> anyhow::Error {
    anyhow::Error::new(TerminalCompletionContext { failure, policy })
}

#[must_use]
pub fn terminal_completion_context(error: &anyhow::Error) -> Option<&TerminalCompletionContext> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<TerminalCompletionContext>())
}

/// Return terminal usage only when the provider policy marks it billable.
///
/// A contextual error is authoritative: an informational terminal outcome must
/// not fall through to its nested failure and become chargeable. Reliable's
/// rejected-attempt sidecar is deliberately outside this projection.
#[must_use]
pub fn billable_terminal_usage(
    error: &anyhow::Error,
) -> Option<&zeroclaw_api::model_provider::TokenUsage> {
    if let Some(context) = terminal_completion_context(error) {
        return (context.policy().usage_chargeability() == TerminalUsageChargeability::Billable)
            .then(|| context.failure().usage.as_ref())
            .flatten();
    }

    terminal_completion_failure(error)
        .and_then(|failure| failure.usage.as_ref())
        .or_else(|| {
            error.chain().find_map(|cause| {
                cause
                    .downcast_ref::<SemanticEmptyTerminalFailure>()
                    .and_then(|failure| failure.usage.as_ref())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::{
        TerminalCompletionPolicy, TerminalRecoveryDisposition, TerminalUsageChargeability,
        billable_terminal_usage, terminal_completion_context_error,
    };
    use zeroclaw_api::model_provider::{
        TerminalCompletionError, TerminalCompletionFailure, TokenUsage,
    };

    #[test]
    fn informational_context_does_not_fall_through_to_nested_usage() {
        let error = terminal_completion_context_error(
            TerminalCompletionFailure::new(
                TerminalCompletionError::Refusal,
                Some(TokenUsage {
                    input_tokens: Some(10),
                    output_tokens: Some(0),
                    cached_input_tokens: None,
                }),
            ),
            TerminalCompletionPolicy::new(
                TerminalRecoveryDisposition::NextCandidate,
                TerminalUsageChargeability::Informational,
            ),
        );

        assert!(billable_terminal_usage(&error).is_none());
    }
}
