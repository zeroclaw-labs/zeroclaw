//! Provider-owned policy for incomplete terminal responses.
//!
//! `zeroclaw_api::TerminalCompletionFailure` deliberately remains a small,
//! stable protocol error. This module carries delivery and accounting policy
//! through internal error chains without changing that public shape.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use zeroclaw_api::model_provider::{
    StreamError, StreamProviderAttempt, TerminalCompletionError, TerminalCompletionFailure,
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
    let attempt = error.failed_candidate().cloned();
    let published = slot
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    match (failure, published) {
        (Some(failure), Some(published)) if failure.reason == published.reason => match attempt {
            Some(attempt) => {
                terminal_completion_context_error_with_attempt(failure, published.policy, attempt)
            }
            None => terminal_completion_context_error(failure, published.policy),
        },
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
    failed_candidate: Option<StreamProviderAttempt>,
}

impl TerminalCompletionContext {
    #[must_use]
    pub(crate) fn new(
        failure: TerminalCompletionFailure,
        policy: TerminalCompletionPolicy,
        failed_candidate: Option<StreamProviderAttempt>,
    ) -> Self {
        Self {
            failure,
            policy,
            failed_candidate,
        }
    }

    #[must_use]
    pub fn failure(&self) -> &TerminalCompletionFailure {
        &self.failure
    }

    #[must_use]
    pub const fn policy(&self) -> TerminalCompletionPolicy {
        self.policy
    }

    #[must_use]
    pub fn failed_candidate(&self) -> Option<&StreamProviderAttempt> {
        self.failed_candidate.as_ref()
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
    anyhow::Error::new(TerminalCompletionContext {
        failure,
        policy,
        failed_candidate: None,
    })
}

#[must_use]
pub(crate) fn terminal_completion_context_error_with_attempt(
    failure: TerminalCompletionFailure,
    policy: TerminalCompletionPolicy,
    failed_candidate: StreamProviderAttempt,
) -> anyhow::Error {
    anyhow::Error::new(TerminalCompletionContext {
        failure,
        policy,
        failed_candidate: Some(failed_candidate),
    })
}

#[must_use]
pub fn terminal_completion_context(error: &anyhow::Error) -> Option<&TerminalCompletionContext> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<TerminalCompletionContext>())
}
