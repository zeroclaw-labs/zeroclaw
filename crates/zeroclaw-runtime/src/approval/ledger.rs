//! Single-use confirmation ledger (RFC 7155 §5.2) — the consumption half
//! of the trusted-confirmation boundary.
//!
//! The approval gate MINTS a
//! [`TrustedConfirmation`] after a real operator answer; this ledger is
//! where that confirmation
//! lives and is consumed at most once. Every consumption returns a
//! deterministic [`ConsumeOutcome`] terminal state, and none of the
//! non-happy states silently allows execution:
//!
//! | situation | outcome |
//! |---|---|
//! | first use, fingerprint matches, within window | `Consumed` |
//! | second use of an already-consumed confirmation | `Replay` |
//! | use after the validity window elapsed | `Expired` |
//! | use of a confirmation superseded by a duplicate answer | `Superseded` |
//! | fingerprint does not match the action being executed | `Stale` |
//!
//! `Conflicting` (approve + deny answers both arrived; the most restrictive
//! wins and is resolved as a deny by the gate before minting) and
//! `Cancelled` (turn/session abort) are contract states on
//! [`ConsumeOutcome`]; the v1 gate resolves conflicts before minting and
//! drops pending confirmations wholesale when its manager is dropped, so
//! the ledger never needs to represent them as stored states.
//!
//! Pure in-memory and per-`ApprovalManager` (never persisted, never shared
//! across managers): durable pending-action recovery is the separately
//! ratified unbounded-wait follow-up (RFC 7155 §R).

use std::collections::HashMap;

use parking_lot::Mutex;
use uuid::Uuid;
use zeroclaw_api::permission::{
    ActionFingerprint, ApproveOrDeny, ConsumeOutcome, TrustedConfirmation,
};

/// Lifecycle state of one ledger entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryState {
    /// Minted, not yet consumed.
    Pending,
    /// Consumed by exactly one execution; any further use is `Replay`.
    Consumed,
    /// The validity window elapsed before the first valid consumption.
    Expired,
    /// The first consumption attempt presented different action facts.
    Stale,
    /// A duplicate response lost the race; the entry is dead. Returns
    /// `Superseded` rather than `Replay` so the caller can tell "this
    /// confirmation was never the operative one" from "this confirmation
    /// already executed".
    Superseded,
}

#[derive(Debug, Clone)]
struct LedgerEntry {
    confirmation: TrustedConfirmation,
    state: EntryState,
}

/// The mint/consume store for trusted confirmations. See the module docs
/// for the lifecycle contract.
#[derive(Debug, Default)]
pub struct ConfirmationLedger {
    entries: Mutex<HashMap<Uuid, LedgerEntry>>,
}

impl ConfirmationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a freshly minted confirmation. Any prior entry under the same
    /// id (a uuid collision — not reachable in practice) is replaced.
    pub fn mint(&self, confirmation: TrustedConfirmation) {
        self.entries.lock().insert(
            confirmation.confirmation_id,
            LedgerEntry {
                confirmation,
                state: EntryState::Pending,
            },
        );
    }

    /// Try to consume a confirmation for the action with this fingerprint.
    ///
    /// `Consumed` is the only outcome that authorizes execution; every
    /// other outcome fails closed. Existing terminal states are returned
    /// before evaluating the window so an entry's outcome stays deterministic;
    /// only a pending entry can transition to `Expired`, `Stale`, or
    /// `Consumed`.
    pub fn consume(
        &self,
        confirmation_id: &Uuid,
        action_fingerprint: &ActionFingerprint,
        now_unix: u64,
    ) -> ConsumeOutcome {
        self.consume_with_expiry(confirmation_id, action_fingerprint, now_unix)
            .0
    }

    /// Consume and return the trusted validity deadline on success so a
    /// downstream spawn boundary can reject work delayed past the window.
    pub fn consume_with_expiry(
        &self,
        confirmation_id: &Uuid,
        action_fingerprint: &ActionFingerprint,
        now_unix: u64,
    ) -> (ConsumeOutcome, Option<u64>, bool) {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.get_mut(confirmation_id) else {
            // Unknown id: the confirmation never existed (or belonged to a
            // different manager/turn). Fail closed; `Stale` is the closest
            // terminal state — nothing about this action was confirmed.
            return (ConsumeOutcome::Stale, None, false);
        };
        match entry.state {
            EntryState::Consumed => return (ConsumeOutcome::Replay, None, false),
            EntryState::Expired => return (ConsumeOutcome::Expired, None, false),
            EntryState::Stale => return (ConsumeOutcome::Stale, None, false),
            EntryState::Superseded => return (ConsumeOutcome::Superseded, None, false),
            EntryState::Pending => {}
        }
        if !entry.confirmation.is_valid_at(now_unix) {
            entry.state = EntryState::Expired;
            return (ConsumeOutcome::Expired, None, true);
        }
        if &entry.confirmation.action_fingerprint != action_fingerprint {
            entry.state = EntryState::Stale;
            return (ConsumeOutcome::Stale, None, true);
        }
        if entry.confirmation.decision == ApproveOrDeny::Deny {
            // A minted deny records the operator's refusal; consuming it
            // must not authorize anything. Report it as the replay-guard
            // outcome so the caller fails closed with a distinct reason.
            entry.state = EntryState::Consumed;
            return (ConsumeOutcome::Replay, None, true);
        }
        let expires_at = entry.confirmation.validity_window.expires_at_unix();
        entry.state = EntryState::Consumed;
        (ConsumeOutcome::Consumed, Some(expires_at), true)
    }

    /// Mark an entry superseded (a duplicate response lost the race).
    /// Unknown ids are ignored — there is nothing to supersede.
    pub fn supersede(&self, confirmation_id: &Uuid) {
        if let Some(entry) = self.entries.lock().get_mut(confirmation_id)
            && entry.state == EntryState::Pending
        {
            entry.state = EntryState::Superseded;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::permission::{
        ActionFingerprint, ApproveOrDeny, ApproverKind, RouteId, TimeWindow,
    };

    fn confirmation(
        id: Uuid,
        facts: &serde_json::Value,
        decision: ApproveOrDeny,
    ) -> TrustedConfirmation {
        TrustedConfirmation::new(
            id,
            ActionFingerprint::compute(facts),
            decision,
            TimeWindow::new(1_000, 300),
            ApproverKind::Human,
            RouteId::cli(),
            None,
        )
    }

    fn facts(command: &str) -> serde_json::Value {
        serde_json::json!({"command": command})
    }

    #[test]
    fn first_use_within_window_consumes() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        let fp = ActionFingerprint::compute(&facts("rm -rf /tmp/x"));
        ledger.mint(confirmation(
            id,
            &facts("rm -rf /tmp/x"),
            ApproveOrDeny::Approve,
        ));

        assert_eq!(ledger.consume(&id, &fp, 1_200), ConsumeOutcome::Consumed);
        // The fingerprint is the same object; the second consume must be a
        // replay, not another Consumed.
        assert_eq!(ledger.consume(&id, &fp, 1_201), ConsumeOutcome::Replay);
    }

    #[test]
    fn use_after_window_expires() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        let fp = ActionFingerprint::compute(&facts("ls"));
        ledger.mint(confirmation(id, &facts("ls"), ApproveOrDeny::Approve));

        assert_eq!(ledger.consume(&id, &fp, 1_300), ConsumeOutcome::Expired);
        assert_eq!(ledger.consume(&id, &fp, 9_999), ConsumeOutcome::Expired);
    }

    #[test]
    fn mismatched_fingerprint_is_stale() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        ledger.mint(confirmation(id, &facts("ls"), ApproveOrDeny::Approve));

        let other = ActionFingerprint::compute(&facts("rm -rf /"));
        assert_eq!(ledger.consume(&id, &other, 1_100), ConsumeOutcome::Stale);
        assert_eq!(ledger.consume(&id, &other, 1_500), ConsumeOutcome::Stale);
    }

    #[test]
    fn unknown_id_is_stale() {
        let ledger = ConfirmationLedger::new();
        let fp = ActionFingerprint::compute(&facts("ls"));
        assert_eq!(
            ledger.consume(&Uuid::new_v4(), &fp, 1_100),
            ConsumeOutcome::Stale
        );
    }

    #[test]
    fn superseded_entry_fails_closed() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        let fp = ActionFingerprint::compute(&facts("ls"));
        ledger.mint(confirmation(id, &facts("ls"), ApproveOrDeny::Approve));
        ledger.supersede(&id);

        assert_eq!(ledger.consume(&id, &fp, 1_100), ConsumeOutcome::Superseded);
        assert_eq!(ledger.consume(&id, &fp, 1_500), ConsumeOutcome::Superseded);
        // Superseding an already-consumed entry changes nothing.
        ledger.supersede(&id);
    }

    #[test]
    fn minted_deny_never_authorizes() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        let fp = ActionFingerprint::compute(&facts("ls"));
        ledger.mint(confirmation(id, &facts("ls"), ApproveOrDeny::Deny));

        assert_eq!(ledger.consume(&id, &fp, 1_100), ConsumeOutcome::Replay);
    }

    #[test]
    fn consumed_entry_remains_replay_after_expiry() {
        let ledger = ConfirmationLedger::new();
        let id = Uuid::new_v4();
        let fp = ActionFingerprint::compute(&facts("ls"));
        ledger.mint(confirmation(id, &facts("ls"), ApproveOrDeny::Approve));
        assert_eq!(ledger.consume(&id, &fp, 1_100), ConsumeOutcome::Consumed);
        assert_eq!(ledger.consume(&id, &fp, 1_500), ConsumeOutcome::Replay);
    }
}
