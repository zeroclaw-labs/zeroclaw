//! Buttoned tool-approval surface for the Discord channel.
//!
//! The agent loop asks a channel to approve a tool call and blocks on the
//! channel's answer (a [`ChannelApprovalResponse`] delivered through a
//! `oneshot`). The historical Discord surface for that was a plaintext prompt
//! ("reply `<token> yes`") parsed back out of the next inbound message. This
//! module is the component upgrade: an action row of Allow-once / Session /
//! Always / Deny buttons whose click resolves the same `oneshot`.
//!
//! ## Security model (the headline)
//! A button click echoes back only the `custom_id` we stamped on it — a
//! client-controlled string. So the *decision* a click carries must never be
//! read from the wire. Instead:
//!
//! * Each of the four buttons is registered server-side in `PendingComponents`
//!   under its own `custom_id`, carrying a fixed [`ApprovalDecision`] (a server
//!   enum, not attacker data) plus the approval token.
//! * On click the type-3 dispatch `take`s that entry (single-use; replay,
//!   forgery, and expiry all resolve to `None`) and resolves the `oneshot` with
//!   the decision *from the registered entry* — the `custom_id` never decides
//!   anything by itself.
//! * Per-click `interaction_gate` (fail-closed) runs BEFORE the `take`, so an
//!   unauthorized click can neither resolve the approval nor drain the entry.
//!
//! The token is the key into `pending_approvals`; it is not a bearer secret and
//! is safe to round-trip through a `custom_id`.

use std::collections::HashMap;

use tokio::sync::oneshot;
use zeroclaw_api::channel::ChannelApprovalResponse;

use super::components::{ButtonStyle, DiscordActionRow, action_row, button};
use super::custom_id::CustomId;

/// `custom_id` kind marker for every approval button. The arg carries the
/// approval token; the *decision* is NOT encoded in the wire id — it lives in
/// the server-registered [`super::pending::ComponentIntent::Approval`] entry.
/// One shared kind keeps all four buttons under the same routing handler; the
/// per-button decision is bound server-side.
pub(crate) const APPROVAL_KIND: &str = "apv";

/// The four operator choices, as a fixed server-side enum. A click resolves to
/// exactly one of these because the *emitter* registered it — never because the
/// wire said so. This is the "no privilege escalation via custom_id" guarantee:
/// "allow" can only come from a button this code constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalDecision {
    /// Execute this one call.
    AllowOnce,
    /// Execute and remember for the rest of the session (maps to
    /// `AlwaysApprove`, the session-scoped allowlist).
    AllowSession,
    /// Execute and always allow this tool (same channel-side response as
    /// session: the runtime owns durable allowlisting; the channel surfaces
    /// the same `AlwaysApprove`). Kept as a distinct button so the operator's
    /// intent reads clearly even though both currently map to the
    /// session-scoped response.
    AllowAlways,
    /// Deny this call.
    Deny,
}

impl ApprovalDecision {
    /// The wire [`ChannelApprovalResponse`] this decision resolves the
    /// `oneshot` with. `AllowSession`/`AllowAlways` both map to
    /// `AlwaysApprove` — the strongest allow the channel protocol exposes;
    /// the runtime owns the durable-vs-session distinction.
    pub(crate) fn response(self) -> ChannelApprovalResponse {
        match self {
            ApprovalDecision::AllowOnce => ChannelApprovalResponse::Approve,
            ApprovalDecision::AllowSession | ApprovalDecision::AllowAlways => {
                ChannelApprovalResponse::AlwaysApprove
            }
            ApprovalDecision::Deny => ChannelApprovalResponse::Deny,
        }
    }

    /// Button face + style for this decision.
    fn label_style(self) -> (&'static str, ButtonStyle) {
        match self {
            ApprovalDecision::AllowOnce => ("Allow once", ButtonStyle::Success),
            ApprovalDecision::AllowSession => ("Allow this session", ButtonStyle::Primary),
            ApprovalDecision::AllowAlways => ("Always allow", ButtonStyle::Secondary),
            ApprovalDecision::Deny => ("Deny", ButtonStyle::Danger),
        }
    }
}

/// The four buttons, in display order. The type-3 arm registers one
/// `PendingComponents` entry per `(custom_id, decision)` pair so the click
/// resolves to the right `ChannelApprovalResponse`.
pub(crate) const APPROVAL_BUTTONS: [ApprovalDecision; 4] = [
    ApprovalDecision::AllowOnce,
    ApprovalDecision::AllowSession,
    ApprovalDecision::AllowAlways,
    ApprovalDecision::Deny,
];

/// Build `(custom_id, decision)` for one approval button bound to `token`.
/// The arg is the approval token (a routing key into `pending_approvals`),
/// not a secret. A button whose decision can't be distinguished on the wire is
/// fine — the decision is the server-side enum, carried separately.
pub(crate) fn approval_button_binding(
    token: &str,
    decision: ApprovalDecision,
) -> (CustomId, ApprovalDecision) {
    // The wire id discriminates buttons only by their position-derived suffix
    // so each gets a UNIQUE custom_id (Discord rejects duplicate ids in one
    // message) and a unique PendingComponents key. The decision itself stays
    // server-side.
    let suffix = match decision {
        ApprovalDecision::AllowOnce => "1",
        ApprovalDecision::AllowSession => "2",
        ApprovalDecision::AllowAlways => "3",
        ApprovalDecision::Deny => "0",
    };
    (
        CustomId::new(APPROVAL_KIND, format!("{token}.{suffix}")),
        decision,
    )
}

/// Resolve a parked approval `oneshot` keyed by `token` with the SERVER-bound
/// `decision`. This is the exact step the type-3 dispatch runs after a click
/// passes the fail-closed `interaction_gate` and the single-use `take`:
///
/// * `decision` is the registered enum, never anything parsed from the wire —
///   so a click cannot escalate a deny button into an allow.
/// * Removing the entry makes resolution single-use at the approval layer too:
///   a token already resolved (raced, or timed-out-and-swept) returns `false`,
///   so a late/duplicate click resolves nothing.
///
/// Returns `true` iff a live `oneshot` was found and sent (the receiver hadn't
/// already been dropped by a timeout).
pub(crate) fn resolve_parked_approval(
    map: &mut HashMap<String, oneshot::Sender<ChannelApprovalResponse>>,
    token: &str,
    decision: ApprovalDecision,
) -> bool {
    map.remove(token)
        .map(|sender| sender.send(decision.response()).is_ok())
        .unwrap_or(false)
}

/// Build the approval action row for `token`: Allow-once / Session / Always /
/// Deny. Returns the row plus the `(custom_id, decision)` bindings the caller
/// must register in `PendingComponents` before sending — registering is what
/// makes a click resolvable (an unregistered id resolves to nothing).
pub(crate) fn build_approval_row(
    token: &str,
) -> (DiscordActionRow, Vec<(CustomId, ApprovalDecision)>) {
    let bindings: Vec<(CustomId, ApprovalDecision)> = APPROVAL_BUTTONS
        .iter()
        .map(|d| approval_button_binding(token, *d))
        .collect();
    let buttons = bindings
        .iter()
        .map(|(cid, decision)| {
            let (label, style) = decision.label_style();
            button(style, label, cid.clone())
        })
        .collect();
    (action_row(buttons), bindings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_decision_maps_to_the_right_wire_response() {
        assert_eq!(
            ApprovalDecision::AllowOnce.response(),
            ChannelApprovalResponse::Approve
        );
        assert_eq!(
            ApprovalDecision::AllowSession.response(),
            ChannelApprovalResponse::AlwaysApprove
        );
        assert_eq!(
            ApprovalDecision::AllowAlways.response(),
            ChannelApprovalResponse::AlwaysApprove
        );
        assert_eq!(
            ApprovalDecision::Deny.response(),
            ChannelApprovalResponse::Deny
        );
    }

    #[test]
    fn row_has_four_buttons_with_unique_ids_carrying_the_token() {
        let (row, bindings) = build_approval_row("abc123");
        assert_eq!(row.components.len(), 4, "four approval buttons");
        assert_eq!(bindings.len(), 4);

        // Every binding routes under the approval kind and carries the token.
        let mut wire_ids = std::collections::HashSet::new();
        for (cid, _) in &bindings {
            assert_eq!(cid.kind, APPROVAL_KIND);
            assert!(cid.arg.starts_with("abc123."), "arg carries the token");
            assert!(
                wire_ids.insert(cid.encode().unwrap()),
                "each button has a unique custom_id"
            );
        }
    }

    #[test]
    fn bindings_cover_every_decision_exactly_once() {
        let (_, bindings) = build_approval_row("tok");
        let decisions: Vec<ApprovalDecision> = bindings.iter().map(|(_, d)| *d).collect();
        assert!(decisions.contains(&ApprovalDecision::AllowOnce));
        assert!(decisions.contains(&ApprovalDecision::AllowSession));
        assert!(decisions.contains(&ApprovalDecision::AllowAlways));
        assert!(decisions.contains(&ApprovalDecision::Deny));
        assert_eq!(decisions.len(), 4);
    }

    // ── oneshot resolution (the payoff) ──────────────────────────────────

    #[tokio::test]
    async fn resolves_the_oneshot_with_the_bound_decision() {
        // Every decision delivers its mapped wire response to the parked rx.
        for (decision, expected) in [
            (
                ApprovalDecision::AllowOnce,
                ChannelApprovalResponse::Approve,
            ),
            (
                ApprovalDecision::AllowSession,
                ChannelApprovalResponse::AlwaysApprove,
            ),
            (
                ApprovalDecision::AllowAlways,
                ChannelApprovalResponse::AlwaysApprove,
            ),
            (ApprovalDecision::Deny, ChannelApprovalResponse::Deny),
        ] {
            let mut map = HashMap::new();
            let (tx, rx) = oneshot::channel();
            map.insert("tok".to_string(), tx);

            assert!(
                resolve_parked_approval(&mut map, "tok", decision),
                "live oneshot resolves"
            );
            assert_eq!(rx.await.unwrap(), expected, "decision: {decision:?}");
            assert!(map.is_empty(), "entry removed (single-use)");
        }
    }

    #[tokio::test]
    async fn replay_resolves_nothing_after_first_click() {
        let mut map = HashMap::new();
        let (tx, _rx) = oneshot::channel();
        map.insert("tok".to_string(), tx);

        assert!(resolve_parked_approval(
            &mut map,
            "tok",
            ApprovalDecision::Deny
        ));
        // A second click on the same (now-drained) token resolves nothing — the
        // approval layer is single-use even if a stale button is clicked.
        assert!(
            !resolve_parked_approval(&mut map, "tok", ApprovalDecision::AllowOnce),
            "replay refused"
        );
    }

    #[tokio::test]
    async fn forged_or_unknown_token_resolves_nothing() {
        let mut map = HashMap::new();
        let (tx, _rx) = oneshot::channel();
        map.insert("real".to_string(), tx);
        // A token we never parked (forged, or for another approval) resolves
        // nothing and leaves the real entry untouched.
        assert!(!resolve_parked_approval(
            &mut map,
            "forged",
            ApprovalDecision::AllowOnce
        ));
        assert!(map.contains_key("real"), "the real entry is not drained");
    }

    #[tokio::test]
    async fn timed_out_receiver_reports_not_resolved() {
        // request_approval drops the rx on timeout and removes the entry. If a
        // late click somehow still held a sender, send() fails (receiver gone)
        // → reported as not resolved, matching the deny-by-default outcome.
        let mut map = HashMap::new();
        let (tx, rx) = oneshot::channel();
        map.insert("tok".to_string(), tx);
        drop(rx); // receiver gone, as after a timeout
        assert!(
            !resolve_parked_approval(&mut map, "tok", ApprovalDecision::AllowOnce),
            "send to a dropped receiver is not a successful resolve"
        );
    }
}
