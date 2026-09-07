//! Task-local side-channel for safeguard (refusal-triggered) model switches.
//!
//! Mirrors the task-local contract of [`crate::reliable::ProviderFallbackInfo`]:
//! the accepted-response owner commits at most one notice per turn via
//! [`commit_safeguard_fallback`], and the runtime turn boundary reads it via
//! [`take_last_safeguard_fallback`]. Both must run inside a
//! [`scope_safeguard_fallback`] scope for the data to be visible; outside a
//! scope, commit/peek/take are silent no-ops.

use std::cell::RefCell;
use std::future::Future;

use crate::reliable::ProviderFallbackInfo;

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

impl SafeguardFallbackNotice {
    /// Whether this notice already presents the client-side recovery leg
    /// that Reliable records as a generic provider fallback.
    ///
    /// Client-owned kinds are composed from the originally requested model,
    /// so a generic record for the same turn would only repeat them. A
    /// server-side notice describes nothing but the accepted attempt's own
    /// request: an ordinary provider fallback that preceded it is a separate
    /// leg with a different cause, and its record is the only place the
    /// original request still appears.
    pub fn covers_client_fallback(&self) -> bool {
        matches!(
            self.kind,
            SafeguardFallbackKind::ClientSide | SafeguardFallbackKind::ClientAndServer
        )
    }
}

/// Select the generic provider-fallback record that still needs its own
/// presentation next to `safeguard`.
///
/// Every delivery surface that renders both records (direct agent, streamed
/// CLI/ACP, RPC, web, channels) must make this decision the same way, so it is
/// owned here rather than repeated per renderer.
pub fn visible_provider_fallback<'a>(
    fallback: Option<&'a ProviderFallbackInfo>,
    safeguard: Option<&SafeguardFallbackNotice>,
) -> Option<&'a ProviderFallbackInfo> {
    fallback.filter(|_| !safeguard.is_some_and(SafeguardFallbackNotice::covers_client_fallback))
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
/// Both `commit_safeguard_fallback` (inside the accepted-response owner) and
/// `take_last_safeguard_fallback` (the runtime turn boundary) must execute
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

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(kind: SafeguardFallbackKind) -> SafeguardFallbackNotice {
        SafeguardFallbackNotice {
            kind,
            requested_model: "model-b".into(),
            served_model: "model-c".into(),
            category: Some("private-category".into()),
        }
    }

    fn ordinary_fallback() -> ProviderFallbackInfo {
        ProviderFallbackInfo {
            requested_provider: "anthropic.primary".into(),
            requested_model: "model-a".into(),
            actual_provider: "anthropic.fallback".into(),
            actual_model: "model-b".into(),
        }
    }

    #[test]
    fn client_owned_notices_cover_the_generic_client_leg() {
        assert!(notice(SafeguardFallbackKind::ClientSide).covers_client_fallback());
        assert!(notice(SafeguardFallbackKind::ClientAndServer).covers_client_fallback());
        assert!(!notice(SafeguardFallbackKind::ServerSide).covers_client_fallback());
    }

    #[test]
    fn ordinary_fallback_stays_visible_beside_a_server_side_notice() {
        let fallback = ordinary_fallback();
        let server = notice(SafeguardFallbackKind::ServerSide);

        let visible = visible_provider_fallback(Some(&fallback), Some(&server))
            .expect("an ordinary A to B leg must survive a server-side B to C notice");
        assert_eq!(visible.requested_model, "model-a");
        assert_eq!(visible.actual_model, "model-b");
    }

    #[test]
    fn refusal_composed_notices_replace_the_generic_record() {
        let fallback = ordinary_fallback();

        for kind in [
            SafeguardFallbackKind::ClientSide,
            SafeguardFallbackKind::ClientAndServer,
        ] {
            assert!(
                visible_provider_fallback(Some(&fallback), Some(&notice(kind))).is_none(),
                "{kind:?} already names the original request"
            );
        }
        assert!(
            visible_provider_fallback(None, Some(&notice(SafeguardFallbackKind::ServerSide)))
                .is_none()
        );
        assert_eq!(
            visible_provider_fallback(Some(&fallback), None).map(|info| info.actual_model.as_str()),
            Some("model-b")
        );
    }
}
