//! Task-local side-channel for safeguard (refusal-triggered) model switches.
//!
//! Mirrors the task-local contract of [`crate::reliable::ProviderFallbackInfo`]:
//! the reliability loop records at most one notice per turn via
//! [`record_safeguard_fallback`], and the post-loop channel orchestrator reads
//! it via [`take_last_safeguard_fallback`]. Both must run inside a
//! [`scope_safeguard_fallback`] scope for the data to be visible; outside a
//! scope, `record`/`take` are silent no-ops.

use std::cell::RefCell;
use std::future::Future;

/// Which layer performed the safeguard-triggered model switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeguardFallbackKind {
    ServerSide,
    ClientSide,
}

/// One safeguard (refusal-triggered) fallback event for the current turn.
/// Read post-loop by the channel orchestrator (PR 5); mirrors
/// `ProviderFallbackInfo`'s task-local contract.
#[derive(Debug, Clone)]
pub struct SafeguardFallbackNotice {
    pub kind: SafeguardFallbackKind,
    pub requested_model: String,
    pub served_model: String,
    /// Category token for logs only — never rendered to users.
    pub category: Option<String>,
}

tokio::task_local! {
    static SAFEGUARD_FALLBACK: RefCell<Option<SafeguardFallbackNotice>>;
}

/// Take (consume) the last safeguard fallback notice, if any.
/// Must be called within a `scope_safeguard_fallback` scope.
pub fn take_last_safeguard_fallback() -> Option<SafeguardFallbackNotice> {
    SAFEGUARD_FALLBACK
        .try_with(|cell| cell.borrow_mut().take())
        .ok()
        .flatten()
}

/// Run the given future within a safeguard-fallback scope.
/// Both `record_safeguard_fallback` (inside ReliableModelProvider) and
/// `take_last_safeguard_fallback` (post-loop channel code) must execute
/// within this scope for the data to be visible.
pub async fn scope_safeguard_fallback<F: Future>(future: F) -> F::Output {
    SAFEGUARD_FALLBACK.scope(RefCell::new(None), future).await
}

/// Record a safeguard (refusal-triggered) fallback event. Last-write-wins;
/// silent when called outside a `scope_safeguard_fallback` scope.
pub fn record_safeguard_fallback(notice: SafeguardFallbackNotice) {
    let _ = SAFEGUARD_FALLBACK.try_with(|cell| {
        *cell.borrow_mut() = Some(notice);
    });
}
