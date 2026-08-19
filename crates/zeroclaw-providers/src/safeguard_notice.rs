//! Task-local side-channel for safeguard (refusal-triggered) model switches.
//!
//! Mirrors the task-local contract of [`crate::reliable::ProviderFallbackInfo`]:
//! the accepted-response owner commits at most one notice per turn via
//! [`commit_safeguard_fallback`], and the post-loop delivery boundary reads
//! it via [`take_last_safeguard_fallback`]. Both must run inside a
//! [`scope_safeguard_fallback`] scope for the data to be visible; outside a
//! scope, commit/peek/take are silent no-ops.

use std::cell::RefCell;
use std::future::Future;

/// Which layer performed the safeguard-triggered model switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeguardFallbackKind {
    ServerSide,
    ClientSide,
    /// Reliable advanced after a refusal and the accepted provider attempt
    /// was itself served by a server-side fallback.
    ClientAndServer,
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

/// Read the accepted safeguard notice without consuming it.
///
/// The agent uses this to suppress its generic fallback text when the outer
/// delivery surface will publish the richer safeguard event.
pub fn peek_last_safeguard_fallback() -> Option<SafeguardFallbackNotice> {
    SAFEGUARD_FALLBACK
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten()
}

/// Run the given future within a safeguard-fallback scope.
/// Both `commit_safeguard_fallback` (inside the accepted-response owner) and
/// `take_last_safeguard_fallback` (post-loop channel code) must execute
/// within this scope for the data to be visible.
pub async fn scope_safeguard_fallback<F: Future>(future: F) -> F::Output {
    SAFEGUARD_FALLBACK.scope(RefCell::new(None), future).await
}

/// Commit the safeguard attribution for the latest accepted response.
///
/// Passing `None` clears attribution from an earlier rejected attempt or
/// tool-loop round. This mirrors Reliable's generic accepted-response record
/// and prevents stale notices from escaping at the final delivery boundary.
pub fn commit_safeguard_fallback(notice: Option<SafeguardFallbackNotice>) {
    let _ = SAFEGUARD_FALLBACK.try_with(|cell| {
        *cell.borrow_mut() = notice;
    });
}
