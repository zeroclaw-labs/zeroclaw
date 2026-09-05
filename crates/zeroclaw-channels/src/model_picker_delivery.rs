//! Delivery acknowledgement handoff for Telegram model picker selections.
//!
//! `Channel::listen` fixes the runtime queue item type at
//! `zeroclaw_api::channel::ChannelMessage`, so a confirmation cannot ride
//! along as a queue payload without a public API change. Instead the
//! Telegram callback registers a one-shot acknowledgement keyed by the
//! selection message id *before* enqueueing, and the orchestrator confirms
//! once the message has actually been consumed and dispatched to
//! `handle_runtime_command_if_needed`. A `try_send` success alone only
//! proves the bounded queue accepted the item — a receiver dropped before
//! consumption silently discards it — so the callback waits for this
//! confirmation before reporting the selection as queued.
//!
//! Ownership of the outcome is a per-selection claim state machine
//! (`Open` → `Applied` | `Revoked`) shared via `Arc<Mutex<ClaimState>>`.
//! Both the callback side ([`revoke`], [`DeliveryAck`] drop) and the
//! dispatch side ([`apply_if_not_revoked`]) transition the claim under the
//! same lock, and the route mutation runs while the dispatch side holds
//! that lock. Whichever side locks the claim first owns the outcome, so a
//! callback timeout can never slip between the dispatch-side check and the
//! route write.
//!
//! The failure boundaries covered on top of the happy path:
//!
//! - The bounded wait can elapse while the selection is still queued (or
//!   waiting on the dispatch semaphore). The callback then reports the
//!   picker as unavailable and [`revoke`]s the claim, so the late dispatch
//!   observes the `Revoked` state and leaves the selection inert instead
//!   of applying the route change after the UI reported failure. If the
//!   route mutation already ran, [`revoke`] reports
//!   [`RevokeOutcome::AlreadyApplied`] so the callback answers `queued`
//!   instead of restoring the picker.
//! - The callback task can be aborted while waiting. Registration returns
//!   a [`DeliveryAck`] guard whose `Drop` revokes an enqueued selection
//!   (the queued message is still live and must not apply without
//!   confirmation) and removes a never-enqueued one, so an aborted wait
//!   can neither leak the sender in the static map nor let an
//!   unacknowledged selection apply.
//! - Registry entries carry their insertion time and any registry op
//!   lazily purges stale `Open` entries older than
//!   [`DELIVERY_ACK_ENTRY_TTL`] — no background task. `Revoked` markers
//!   are never swept by time: they are consumed by the late dispatch via
//!   [`take_revoked`]/[`apply_if_not_revoked`], and a time-based sweep
//!   could otherwise drop the revocation while the selection is still
//!   queued, letting the route mutate after the UI reported failure. If
//!   the queued message was dropped because the runtime receiver went away
//!   before dequeue, nothing consumes the marker; [`clear_abandoned`]
//!   reclaims that residue once the dispatch pipeline is definitively gone
//!   (`start_channels` teardown), which is the only point where no live
//!   queued selection can still hold revocation authority.
//! - The dispatch that dequeued a selection can end before it ever reaches
//!   the mutation point: an `on_message_received` hook cancels it, a
//!   self-loop guard drops it, or the dispatch task is aborted. Nothing
//!   downstream would consume that registration, so the dispatch holds a
//!   [`DispatchOwnership`] token for the message's lifetime and its drop
//!   settles the claim as `Dropped`: a still-waiting callback is woken so
//!   it restores the picker at once instead of at its timeout, a revoked
//!   marker is reclaimed, and an applied or unregistered selection is left
//!   untouched. Only a message the dispatch definitively gave up on is
//!   settled this way; a selection that is still queued keeps its
//!   revocation authority.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// Upper bound on how long an `Open` registration may stay in the map. The
/// bounded ack wait is 5s (`TELEGRAM_MODEL_PICKER_DELIVERY_ACK_TIMEOUT`),
/// after which the callback either confirms, revokes, or drops the guard —
/// so by 60s any leftover `Open` entry is an orphan with no live queued
/// message. Uses `tokio::time::Instant` so tests can age entries under a
/// paused clock.
const DELIVERY_ACK_ENTRY_TTL: Duration = Duration::from_secs(60);

/// Terminal ownership state of a registered selection. Transitions happen
/// under the claim lock only; the route mutation runs while the dispatch
/// side holds that lock, so check and write are atomic with respect to a
/// concurrent revocation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClaimState {
    /// Registered; neither side has claimed the outcome yet.
    Open,
    /// The dispatch applied the route mutation while holding the claim.
    Applied,
    /// The callback timed out or was aborted after enqueue; the late
    /// dispatch must leave the selection inert.
    Revoked,
    /// The dispatch that owned the dequeued message ended before the
    /// mutation point (hook cancel, self-loop drop, task abort). No route
    /// write can follow; the entry only survives until the waiting
    /// callback reads it.
    Dropped,
}

struct PendingDeliveryAck {
    /// `None` once the owning dispatch dropped the message: releasing the
    /// sender ends the callback's bounded wait immediately with
    /// `RecvError` instead of at the timeout.
    sender: Option<tokio::sync::oneshot::Sender<()>>,
    claim: Arc<Mutex<ClaimState>>,
    inserted_at: tokio::time::Instant,
}

static PENDING_DELIVERY_ACKS: LazyLock<Mutex<HashMap<String, PendingDeliveryAck>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> MutexGuard<'static, HashMap<String, PendingDeliveryAck>> {
    PENDING_DELIVERY_ACKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn claim_lock(claim: &Arc<Mutex<ClaimState>>) -> MutexGuard<'_, ClaimState> {
    claim
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Shared serialization boundary for every test that touches the
/// process-global registry, in any module of this crate: the default test
/// runner is parallel, and a global op such as [`clear_abandoned`] (or a
/// purge under a paused clock) must never observe or destroy another
/// test's in-flight registration.
#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn registry_test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lazily reclaim stale `Open` and `Dropped` entries older than
/// [`DELIVERY_ACK_ENTRY_TTL`]. A stale `Open` entry is always an orphan:
/// the callback's ack wait is bounded at 5s, so no live queued message can
/// still own it; a stale `Dropped` entry likewise has no dispatch left that
/// could act on it. `Revoked` (and `Applied`) markers are kept regardless
/// of age — only the late dispatch may consume a revocation. Called from
/// every registry op so no background task is needed.
fn purge_expired(pending: &mut HashMap<String, PendingDeliveryAck>) {
    pending.retain(|_, entry| {
        if entry.inserted_at.elapsed() < DELIVERY_ACK_ENTRY_TTL {
            return true;
        }
        matches!(
            *claim_lock(&entry.claim),
            ClaimState::Revoked | ClaimState::Applied
        )
    });
}

/// Receiver side of a registered selection acknowledgement. Dropping the
/// guard settles a still-`Open` registration: an enqueued selection is
/// revoked (callback aborted mid-wait; the queued message must not apply
/// without confirmation), a never-enqueued one is removed outright, so the
/// sender cannot outlive the callback task in the static map. A `Dropped`
/// entry (the dispatch already gave the message up) is removed as well:
/// this guard is its last owner. Entries in a terminal state
/// (`Applied`/`Revoked`) are owned by the dispatch/revoke paths and left
/// alone.
pub(crate) struct DeliveryAck {
    message_id: String,
    receiver: tokio::sync::oneshot::Receiver<()>,
    claim: Arc<Mutex<ClaimState>>,
    enqueued: bool,
}

impl DeliveryAck {
    /// Wait for the dispatch-side confirmation. Resolves `Err` when the
    /// registration was dropped without confirming.
    pub(crate) async fn wait(&mut self) -> Result<(), tokio::sync::oneshot::error::RecvError> {
        (&mut self.receiver).await
    }

    /// Mark the selection as handed to the runtime queue. Only an enqueued
    /// selection needs a surviving `Revoked` marker when the guard drops
    /// without an answer; a selection that never entered the queue is
    /// simply unregistered on drop.
    pub(crate) fn mark_enqueued(&mut self) {
        self.enqueued = true;
    }
}

impl Drop for DeliveryAck {
    fn drop(&mut self) {
        let mut pending = pending();
        if !pending.contains_key(&self.message_id) {
            return;
        }
        let mut state = claim_lock(&self.claim);
        match *state {
            ClaimState::Open if self.enqueued => {
                *state = ClaimState::Revoked;
            }
            ClaimState::Open | ClaimState::Dropped => {
                drop(state);
                pending.remove(&self.message_id);
            }
            ClaimState::Applied | ClaimState::Revoked => {}
        }
    }
}

/// Register the acknowledgement for a selection about to be enqueued. Must
/// run before the queue handoff so a runtime that consumes the selection
/// immediately cannot confirm into a not-yet-registered id.
pub(crate) fn register(message_id: &str) -> DeliveryAck {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let claim = Arc::new(Mutex::new(ClaimState::Open));
    let mut pending = pending();
    purge_expired(&mut pending);
    pending.insert(
        message_id.to_string(),
        PendingDeliveryAck {
            sender: Some(sender),
            claim: Arc::clone(&claim),
            inserted_at: tokio::time::Instant::now(),
        },
    );
    DeliveryAck {
        message_id: message_id.to_string(),
        receiver,
        claim,
        enqueued: false,
    }
}

/// Confirm that the selection reached runtime command handling. Messages
/// that did not originate from the picker have no registration, so this is
/// a no-op for ordinary traffic.
pub(crate) fn confirm(message_id: &str) {
    let mut pending = pending();
    purge_expired(&mut pending);
    let entry = pending.remove(message_id);
    if let Some(entry) = entry {
        *claim_lock(&entry.claim) = ClaimState::Applied;
        // A send error only means the callback already stopped waiting and
        // dropped the receiver; nothing left to propagate.
        if let Some(sender) = entry.sender {
            let _ = sender.send(());
        }
    }
}

/// Drop a registration without confirming, e.g. when the enqueue failed and
/// the selection never entered the queue.
pub(crate) fn cancel(message_id: &str) {
    pending().remove(message_id);
}

/// Outcome of a callback-side revocation attempt.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RevokeOutcome {
    /// The revocation won: the selection had not been applied, so the late
    /// dispatch will observe the `Revoked` claim and stay inert.
    Won,
    /// The route mutation already ran (or the registration was already
    /// confirmed): the callback must report the selection as queued
    /// instead of restoring the picker.
    AlreadyApplied,
}

/// Mark a registered selection as revoked: the callback's bounded wait
/// elapsed while the selection was already enqueued, so the late dispatch
/// must observe the revocation instead of applying the route change after
/// the picker UI already reported failure. The claim lock serializes
/// against a concurrent [`apply_if_not_revoked`]: whichever side locks the
/// claim first owns the outcome.
pub(crate) fn revoke(message_id: &str) -> RevokeOutcome {
    let mut pending = pending();
    purge_expired(&mut pending);
    let Some(claim) = pending
        .get(message_id)
        .map(|entry| Arc::clone(&entry.claim))
    else {
        // Only `confirm`/`apply_if_not_revoked` remove an entry the live
        // callback still owns, so a missing entry means the route already
        // mutated.
        return RevokeOutcome::AlreadyApplied;
    };
    let mut state = claim_lock(&claim);
    match *state {
        ClaimState::Open | ClaimState::Revoked => {
            *state = ClaimState::Revoked;
            RevokeOutcome::Won
        }
        // The dispatch already gave the dequeued message up before the
        // mutation point: the route can never apply and no late dispatch
        // will consume the entry, so reclaim it here and let the callback
        // restore the picker.
        ClaimState::Dropped => {
            drop(state);
            pending.remove(message_id);
            RevokeOutcome::Won
        }
        ClaimState::Applied => RevokeOutcome::AlreadyApplied,
    }
}

/// Consume the revoked marker for a dequeued message. Returns `true` exactly
/// once for a selection whose callback already timed out and reported the
/// picker as unavailable. Ordinary traffic never registered, and a confirmed
/// selection was already removed by [`confirm`], so both return `false`. A
/// `Dropped` entry fails closed the same way: its dispatch already gave the
/// message up, so nothing may apply it.
pub(crate) fn take_revoked(message_id: &str) -> bool {
    let mut pending = pending();
    purge_expired(&mut pending);
    let revoked = pending.get(message_id).is_some_and(|entry| {
        matches!(
            *claim_lock(&entry.claim),
            ClaimState::Revoked | ClaimState::Dropped
        )
    });
    if revoked {
        pending.remove(message_id);
        return true;
    }
    false
}

/// Reclaim every registration whose owning dispatch pipeline is gone.
/// Must only run once the runtime queue is definitively dead
/// (`start_channels` teardown): a revoked marker is authority for a
/// selection that may still be queued, and clearing it while the message
/// remains live would let the late dispatch apply a route change the
/// picker UI already reported as unavailable.
pub(crate) fn clear_abandoned() {
    pending().clear();
}

/// Route-mutation handoff at the authoritative mutation point. Runs `f`
/// (the route write) while holding the selection's claim lock, so a
/// callback revocation can never slip between the check and the mutation:
/// whichever side locks the claim first owns the outcome. Returns `true`
/// when the mutation ran; a revoked selection consumes its registration
/// and returns `false` without running `f`. Messages without a
/// registration (ordinary `/model` traffic) always apply.
pub(crate) fn apply_if_not_revoked(message_id: &str, f: impl FnOnce()) -> bool {
    let claim = {
        let mut pending = pending();
        purge_expired(&mut pending);
        pending
            .get(message_id)
            .map(|entry| Arc::clone(&entry.claim))
    };
    let Some(claim) = claim else {
        f();
        return true;
    };
    let mut state = claim_lock(&claim);
    match *state {
        ClaimState::Open => {
            *state = ClaimState::Applied;
            f();
            drop(state);
            // Consume the registration and release the callback's ack wait
            // only now that the route mutation actually ran.
            let mut pending = pending();
            if let Some(sender) = pending.remove(message_id).and_then(|entry| entry.sender) {
                let _ = sender.send(());
            }
            true
        }
        ClaimState::Revoked | ClaimState::Dropped => {
            drop(state);
            pending().remove(message_id);
            false
        }
        // Unreachable in practice: an applied selection already consumed
        // its registration. Fail closed.
        ClaimState::Applied => false,
    }
}

/// Settle the registration of a message whose owning dispatch ended without
/// reaching the mutation point. Runs from [`DispatchOwnership`]'s drop only.
fn settle_dropped(message_id: &str) {
    let mut pending = pending();
    purge_expired(&mut pending);
    let Some(claim) = pending
        .get(message_id)
        .map(|entry| Arc::clone(&entry.claim))
    else {
        return;
    };
    let mut state = claim_lock(&claim);
    match *state {
        ClaimState::Open => {
            // The callback is still inside its bounded wait: mark the claim
            // so its revoke reads "dropped" rather than "already applied",
            // and release the sender so that wait ends now instead of at
            // its timeout. The callback's revoke (or its guard drop)
            // removes the entry.
            *state = ClaimState::Dropped;
            drop(state);
            if let Some(entry) = pending.get_mut(message_id) {
                entry.sender.take();
            }
        }
        ClaimState::Revoked => {
            // The callback already reported the picker as unavailable; the
            // marker only existed for a late dispatch that can no longer
            // happen.
            drop(state);
            pending.remove(message_id);
        }
        ClaimState::Applied | ClaimState::Dropped => {}
    }
}

/// Ownership token held by the dispatch that dequeued a message, for the
/// whole dispatch lifetime of that message. Dropping it settles a
/// registration that no downstream path will consume any more — the
/// dispatch returned early (hook cancel, self-loop guard, passive context)
/// or its task was aborted. A selection that reached [`confirm`],
/// [`apply_if_not_revoked`] or [`take_revoked`] has already left the
/// registry, and ordinary traffic never registered, so the drop is inert
/// on every completed path.
pub(crate) struct DispatchOwnership {
    message_id: String,
}

impl DispatchOwnership {
    pub(crate) fn hold(message_id: &str) -> Self {
        Self {
            message_id: message_id.to_string(),
        }
    }
}

impl Drop for DispatchOwnership {
    fn drop(&mut self) {
        settle_dropped(&self.message_id);
    }
}

#[cfg(test)]
pub(crate) fn is_registered(message_id: &str) -> bool {
    pending().contains_key(message_id)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex, MutexGuard};
    use std::time::Duration;

    use super::{ClaimState, is_registered};

    /// Every test in this module serializes on the crate-wide registry
    /// test lock shared with the Telegram and orchestrator picker tests:
    /// a concurrently running test must not purge or clear another test's
    /// entries (the default runner is parallel).
    fn test_lock() -> MutexGuard<'static, ()> {
        super::registry_test_lock()
    }

    /// Insert an entry with a backdated timestamp. TTL tests cannot advance
    /// a paused clock instead: the map is process-global, so entries created
    /// under another test's frozen clock would be measured against this
    /// runtime's real clock (and vice versa) by any concurrent purge.
    fn insert_entry_for_test(message_id: &str, claim: ClaimState, age: Duration) {
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        super::pending().insert(
            message_id.to_string(),
            super::PendingDeliveryAck {
                sender: Some(sender),
                claim: Arc::new(Mutex::new(claim)),
                inserted_at: tokio::time::Instant::now()
                    .checked_sub(age)
                    .unwrap_or_else(tokio::time::Instant::now),
            },
        );
    }

    // The test-serialization lock is held across the whole test
    // (including the ack wait) on purpose: the registry is
    // process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn registered_selection_confirm_fires() {
        let _guard = test_lock();
        let mut ack = super::register("selection-confirm");
        super::confirm("selection-confirm");
        assert!(ack.wait().await.is_ok());
    }

    // The test-serialization lock is held across the whole test
    // (including the ack wait) on purpose: the registry is
    // process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn confirm_without_registration_is_a_noop() {
        let _guard = test_lock();
        // Ordinary traffic never registers; confirming its id must not
        // panic or disturb a later registration under the same id.
        super::confirm("ordinary-message");
        let mut ack = super::register("ordinary-message");
        super::confirm("ordinary-message");
        assert!(ack.wait().await.is_ok());
    }

    // The test-serialization lock is held across the whole test
    // (including the ack wait) on purpose: the registry is
    // process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn cancelled_registration_never_fires() {
        let _guard = test_lock();
        let mut ack = super::register("selection-cancelled");
        super::cancel("selection-cancelled");
        assert!(!is_registered("selection-cancelled"));
        super::confirm("selection-cancelled");
        assert!(ack.wait().await.is_err());
    }

    #[tokio::test]
    async fn dropped_registration_removes_entry() {
        let _guard = test_lock();
        // The callback task was aborted before the queue handoff: no
        // confirm and no cancel ever ran, and the selection never entered
        // the queue. The guard drop must still remove the entry so the
        // sender cannot leak in the static map.
        let ack = super::register("selection-aborted");
        assert!(is_registered("selection-aborted"));
        drop(ack);
        assert!(!is_registered("selection-aborted"));
        // A late confirm for the removed id is a no-op.
        super::confirm("selection-aborted");
        assert!(!is_registered("selection-aborted"));
    }

    #[tokio::test]
    async fn aborted_after_enqueue_leaves_revoked_marker() {
        let _guard = test_lock();
        // The callback task was aborted after the queue handoff succeeded
        // but before the runtime consumed the message: the queued
        // selection is still live, so the guard drop must revoke it
        // instead of letting it apply without confirmation.
        let mut ack = super::register("selection-aborted-enqueued");
        ack.mark_enqueued();
        drop(ack);
        assert!(is_registered("selection-aborted-enqueued"));
        // The late dispatch observes the revocation and stays inert.
        let mut applied = false;
        assert!(!super::apply_if_not_revoked(
            "selection-aborted-enqueued",
            || {
                applied = true;
            }
        ));
        assert!(!applied, "aborted selection must not mutate the route");
        assert!(!is_registered("selection-aborted-enqueued"));
    }

    #[tokio::test]
    async fn revoked_selection_survives_guard_drop_and_is_taken_once() {
        let _guard = test_lock();
        let mut ack = super::register("selection-revoked");
        ack.mark_enqueued();
        assert!(matches!(
            super::revoke("selection-revoked"),
            super::RevokeOutcome::Won
        ));
        // The revoked marker must outlive the callback task so the late
        // dispatch can observe it.
        drop(ack);
        assert!(is_registered("selection-revoked"));
        assert!(super::take_revoked("selection-revoked"));
        assert!(!is_registered("selection-revoked"));
        assert!(!super::take_revoked("selection-revoked"));
    }

    #[tokio::test]
    async fn cancelled_selection_is_not_revoked() {
        let _guard = test_lock();
        // The enqueue-failed path cancels outright: the selection never
        // entered the queue, so there is nothing for dispatch to revoke.
        let ack = super::register("selection-enqueue-failed");
        super::cancel("selection-enqueue-failed");
        assert!(!super::take_revoked("selection-enqueue-failed"));
        drop(ack);
    }

    #[tokio::test]
    async fn ordinary_message_is_never_revoked() {
        let _guard = test_lock();
        assert!(!super::take_revoked("ordinary-never-registered"));
    }

    #[tokio::test]
    async fn apply_runs_for_unregistered_message() {
        let _guard = test_lock();
        // Ordinary `/model` traffic has no claim and always applies.
        let mut applied = false;
        assert!(super::apply_if_not_revoked("ordinary-apply", || {
            applied = true;
        }));
        assert!(applied);
    }

    #[tokio::test]
    async fn apply_after_revoke_leaves_selection_inert() {
        let _guard = test_lock();
        // The callback's ack wait elapsed while the selection was queued:
        // the late dispatch must not run the route mutation.
        let _ack = super::register("selection-revoked-apply");
        assert!(matches!(
            super::revoke("selection-revoked-apply"),
            super::RevokeOutcome::Won
        ));
        let mut applied = false;
        assert!(!super::apply_if_not_revoked(
            "selection-revoked-apply",
            || {
                applied = true;
            }
        ));
        assert!(!applied, "revoked selection must not mutate the route");
        assert!(!is_registered("selection-revoked-apply"));
    }

    // The test-serialization lock is held across the whole test
    // (including the ack wait) on purpose: the registry is
    // process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn apply_holds_claim_through_mutation_so_revoke_reports_already_applied() {
        let _guard = test_lock();
        // Deterministic revocation-during-mutation race: the dispatch
        // holds the claim lock across the route write, so a concurrent
        // `revoke` blocks until the mutation finished and then observes
        // `Applied` — the callback reports the selection as queued instead
        // of restoring the picker over an already-switched route.
        let mut ack = super::register("selection-apply-revoke-race");
        ack.mark_enqueued();
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let entered_worker = Arc::clone(&entered);
        let release_worker = Arc::clone(&release);
        let applier = std::thread::spawn(move || {
            super::apply_if_not_revoked("selection-apply-revoke-race", move || {
                entered_worker.wait();
                release_worker.wait();
            })
        });
        // The mutation started and holds the claim lock.
        entered.wait();
        let (revoke_tx, revoke_rx) = std::sync::mpsc::channel();
        let revoker = std::thread::spawn(move || {
            revoke_tx
                .send(super::revoke("selection-apply-revoke-race"))
                .unwrap();
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            revoke_rx.try_recv().is_err(),
            "revoke must block while the claim is held across the mutation"
        );
        release.wait();
        assert!(
            applier.join().unwrap(),
            "unrevoked selection must apply the route mutation"
        );
        assert!(matches!(
            revoke_rx.recv().unwrap(),
            super::RevokeOutcome::AlreadyApplied
        ));
        revoker.join().unwrap();
        // The applied selection released the callback's ack wait.
        assert!(ack.wait().await.is_ok());
        assert!(!super::take_revoked("selection-apply-revoke-race"));
    }

    #[tokio::test]
    async fn revoked_entry_older_than_ttl_survives_lazy_sweep() {
        let _guard = test_lock();
        // Regression: a stalled runtime queue can delay dispatch past any
        // elapsed-time bound. The revocation must stay authoritative until
        // the late dispatch consumes it — a time-based sweep may never
        // drop a revoked marker, or the route would mutate after the
        // picker UI already reported the selection as unavailable.
        // Backdated to 2× the TTL so every runtime clock measures it as
        // expired.
        insert_entry_for_test(
            "selection-stale-revoked",
            ClaimState::Revoked,
            super::DELIVERY_ACK_ENTRY_TTL * 2,
        );
        // Any registry op performs the lazy sweep; confirming an id that
        // was never registered keeps ordinary traffic a no-op.
        super::confirm("ordinary-message");
        assert!(
            is_registered("selection-stale-revoked"),
            "revoked marker must survive the TTL sweep until dispatch consumes it"
        );
        // The late dispatch beyond the TTL still observes the revocation
        // and stays inert.
        let mut applied = false;
        assert!(!super::apply_if_not_revoked(
            "selection-stale-revoked",
            || {
                applied = true;
            }
        ));
        assert!(!applied);
        assert!(!is_registered("selection-stale-revoked"));
    }

    #[tokio::test]
    async fn stale_open_entry_is_purged_without_consumption() {
        // Exercise the sweep against local state: publishing an intentionally
        // expired entry in the process-global registry would let any concurrent
        // runtime-command path purge the fixture before this test observes it.
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        let mut pending = std::collections::HashMap::from([(
            "selection-stale-open".to_string(),
            super::PendingDeliveryAck {
                sender: Some(sender),
                claim: Arc::new(Mutex::new(ClaimState::Open)),
                inserted_at: tokio::time::Instant::now()
                    .checked_sub(super::DELIVERY_ACK_ENTRY_TTL * 2)
                    .unwrap_or_else(tokio::time::Instant::now),
            },
        )]);

        assert!(pending.contains_key("selection-stale-open"));
        super::purge_expired(&mut pending);
        assert!(
            !pending.contains_key("selection-stale-open"),
            "stale open entry must be reclaimed without consumption"
        );
    }

    #[tokio::test]
    async fn clear_abandoned_reclaims_revoked_entry_after_queue_death() {
        let _guard = test_lock();
        // The callback timed out (selection revoked) and the runtime
        // receiver was then dropped before dequeue: nothing will ever
        // consume the marker. Once the dispatch pipeline is definitively
        // gone, `clear_abandoned` reclaims the residue — and never before,
        // so a still-queued selection cannot lose its revocation.
        let mut ack = super::register("selection-abandoned");
        ack.mark_enqueued();
        assert!(matches!(
            super::revoke("selection-abandoned"),
            super::RevokeOutcome::Won
        ));
        drop(ack);
        assert!(is_registered("selection-abandoned"));
        super::clear_abandoned();
        assert!(!is_registered("selection-abandoned"));
    }

    #[tokio::test]
    async fn revoked_entry_younger_than_ttl_survives_lazy_sweep() {
        let _guard = test_lock();
        // Half the TTL: comfortably inside the bound even if a concurrent
        // paused-clock test measures this entry with a slightly advanced
        // clock (the ack-timeout tests advance ~5s).
        insert_entry_for_test(
            "selection-fresh-revoked",
            ClaimState::Revoked,
            super::DELIVERY_ACK_ENTRY_TTL / 2,
        );
        // The sweep runs on every registry op but must not touch entries
        // still inside the TTL: the late dispatch can still observe them.
        let _other = super::register("selection-unrelated");
        assert!(super::take_revoked("selection-fresh-revoked"));
    }

    // The test-serialization lock is held across the ack wait on purpose:
    // the registry is process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn dropped_selection_wakes_waiting_callback_and_settles_on_revoke() {
        let _guard = test_lock();
        // The dispatch dequeued the selection, but an `on_message_received`
        // hook cancelled it before the mutation point while the callback
        // was still inside its bounded ack wait. Dropping the dispatch
        // ownership must end that wait at once (not at the timeout), and
        // the callback's revoke must read "dropped": it restores the picker
        // and leaves no residue behind.
        let mut ack = super::register("selection-dropped-waiting");
        ack.mark_enqueued();
        drop(super::DispatchOwnership::hold("selection-dropped-waiting"));
        let woken = tokio::time::timeout(Duration::from_secs(1), ack.wait()).await;
        assert!(
            matches!(woken, Ok(Err(_))),
            "dropped dispatch must release the waiting callback: {woken:?}"
        );
        assert!(matches!(
            super::revoke("selection-dropped-waiting"),
            super::RevokeOutcome::Won
        ));
        assert!(!is_registered("selection-dropped-waiting"));
        drop(ack);
        assert!(!is_registered("selection-dropped-waiting"));
    }

    #[tokio::test]
    async fn dropped_revoked_marker_is_reclaimed_by_dispatch_owner() {
        let _guard = test_lock();
        // The callback timed out first (revoked marker retained for the late
        // dispatch), then the dispatch dropped the dequeued message before
        // the gate. Nothing can consume the marker any more; the ownership
        // drop must reclaim it instead of leaving it for the daemon's
        // lifetime.
        let mut ack = super::register("selection-dropped-revoked");
        ack.mark_enqueued();
        assert!(matches!(
            super::revoke("selection-dropped-revoked"),
            super::RevokeOutcome::Won
        ));
        drop(ack);
        assert!(is_registered("selection-dropped-revoked"));
        drop(super::DispatchOwnership::hold("selection-dropped-revoked"));
        assert!(!is_registered("selection-dropped-revoked"));
        assert!(!super::take_revoked("selection-dropped-revoked"));
    }

    #[tokio::test]
    async fn dropped_selection_is_reclaimed_when_callback_aborts_before_revoke() {
        let _guard = test_lock();
        // The dispatch dropped the message while the callback was waiting,
        // and the callback task was then aborted before it could revoke:
        // the ack guard is the last owner and must remove the entry.
        let mut ack = super::register("selection-dropped-aborted");
        ack.mark_enqueued();
        drop(super::DispatchOwnership::hold("selection-dropped-aborted"));
        assert!(is_registered("selection-dropped-aborted"));
        drop(ack);
        assert!(!is_registered("selection-dropped-aborted"));
    }

    #[tokio::test]
    async fn dropped_selection_never_applies() {
        let _guard = test_lock();
        // Fail closed: should a dropped entry ever meet a dispatch-side
        // check, it reads as revoked and the route stays untouched.
        let mut ack = super::register("selection-dropped-apply");
        ack.mark_enqueued();
        drop(super::DispatchOwnership::hold("selection-dropped-apply"));
        let mut applied = false;
        assert!(!super::apply_if_not_revoked(
            "selection-dropped-apply",
            || {
                applied = true;
            }
        ));
        assert!(!applied, "dropped selection must not mutate the route");
        assert!(!is_registered("selection-dropped-apply"));

        let mut gated = super::register("selection-dropped-gate");
        gated.mark_enqueued();
        drop(super::DispatchOwnership::hold("selection-dropped-gate"));
        assert!(super::take_revoked("selection-dropped-gate"));
        assert!(!is_registered("selection-dropped-gate"));
    }

    // The test-serialization lock is held across the ack wait on purpose:
    // the registry is process-global and the default runner is parallel.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn dispatch_ownership_is_inert_for_applied_and_unregistered_messages() {
        let _guard = test_lock();
        // Ordinary traffic never registered, and an applied selection
        // already left the registry: neither drop may change anything.
        drop(super::DispatchOwnership::hold("ordinary-dropped"));
        assert!(!is_registered("ordinary-dropped"));

        let mut ack = super::register("selection-applied-then-dropped");
        ack.mark_enqueued();
        let ownership = super::DispatchOwnership::hold("selection-applied-then-dropped");
        assert!(super::apply_if_not_revoked(
            "selection-applied-then-dropped",
            || {}
        ));
        drop(ownership);
        assert!(matches!(ack.wait().await, Ok(())));
        assert!(matches!(
            super::revoke("selection-applied-then-dropped"),
            super::RevokeOutcome::AlreadyApplied
        ));
        assert!(!is_registered("selection-applied-then-dropped"));
    }

    #[tokio::test]
    async fn stale_dropped_entry_is_purged_without_consumption() {
        // Local state for the same reason as the stale `Open` case: a
        // dropped entry whose callback never came back has no owner left,
        // so the lazy sweep may reclaim it once it is past the TTL.
        let mut pending = std::collections::HashMap::from([(
            "selection-stale-dropped".to_string(),
            super::PendingDeliveryAck {
                sender: None,
                claim: Arc::new(Mutex::new(ClaimState::Dropped)),
                inserted_at: tokio::time::Instant::now()
                    .checked_sub(super::DELIVERY_ACK_ENTRY_TTL * 2)
                    .unwrap_or_else(tokio::time::Instant::now),
            },
        )]);

        assert!(pending.contains_key("selection-stale-dropped"));
        super::purge_expired(&mut pending);
        assert!(
            !pending.contains_key("selection-stale-dropped"),
            "stale dropped entry must be reclaimed without consumption"
        );
    }
}
