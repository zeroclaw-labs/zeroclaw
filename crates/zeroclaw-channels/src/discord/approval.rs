//! Buttoned tool-approval surface for the Discord channel.

use std::collections::HashMap;

use tokio::sync::oneshot;
use zeroclaw_api::channel::ChannelApprovalResponse;

use super::components::{ButtonStyle, DiscordActionRow, action_row, button};
use super::custom_id::CustomId;

pub(crate) const APPROVAL_KIND: &str = "apv";

/// `custom_id` kind for STATELESS SOP-gate buttons (`ChannelGatePrompt`
/// deliveries). Unlike [`APPROVAL_KIND`] there is no server-side registration:
/// the arg carries `<choice_id>:<reference>` directly, so the buttons survive
/// daemon restarts and can answer a gate parked hours earlier. A click becomes
/// an inbound message stamped with the internal `sop.gate:` marker (set only by
/// the interaction producer, never derivable from message text), which the
/// orchestrator resolves against the parked gate. Decision-in-wire is safe here
/// because a SOP gate's choices are operator-privilege-equivalent
/// (approve/deny), unlike the tool-approval escalation ladder above.
pub(crate) const SOP_GATE_KIND: &str = "sopgate";

/// `custom_id` kind for a SOP-gate button whose choice COLLECTS TEXT (Edit /
/// Revise): the click's interaction response is opening a modal (type 9), not
/// emitting a marker. Same stateless `<choice_id>:<reference>` arg as
/// [`SOP_GATE_KIND`]; the modal pre-fill comes from the in-memory prompt
/// registry when the process that sent the prompt is still alive (best-effort —
/// after a restart the modal opens blank, the draft is still in the embed).
pub(crate) const SOP_GATE_MODAL_KIND: &str = "sopgatem";

/// `custom_id` kind for the MODAL itself (echoed back on its type-5 submit):
/// the submit parses statelessly and becomes the inbound marker message, with
/// the typed text as the message content.
pub(crate) const SOP_GATE_SUBMIT_KIND: &str = "sopgates";

/// True for any of the stateless SOP-gate custom_id kinds — used by the
/// shared-token foreign-alias silent pass (every alias receives every click;
/// only the alias that owns the channel may answer, the rest stay quiet).
pub(crate) fn is_sop_gate_kind(kind: &str) -> bool {
    kind == SOP_GATE_KIND || kind == SOP_GATE_MODAL_KIND || kind == SOP_GATE_SUBMIT_KIND
}

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
