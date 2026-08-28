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
//! Two failure boundaries are covered on top of the happy path:
//!
//! - The bounded wait can elapse while the selection is still queued (or
//!   waiting on the dispatch semaphore). The callback then reports the
//!   picker as unavailable and [`revoke`]s the registration, so the late
//!   dispatch observes [`take_revoked`] and leaves the selection inert
//!   instead of applying the route change after the UI reported failure.
//! - The callback task can be aborted while waiting. Registration returns a
//!   [`DeliveryAck`] guard whose `Drop` removes a still-pending entry, so an
//!   aborted wait cannot leak the sender in the static map.
//! - A revoked entry whose queued selection is dropped before dispatch
//!   (runtime receiver shutdown) would otherwise sit in the map forever.
//!   Entries carry their insertion time and any registry op lazily purges
//!   entries older than [`DELIVERY_ACK_ENTRY_TTL`] — no background task.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Duration;

/// Upper bound on how long a registration may stay in the map. The bounded
/// ack wait is 5s (`TELEGRAM_MODEL_PICKER_DELIVERY_ACK_TIMEOUT`), after which
/// the callback either confirms, revokes, or drops the guard — so by 60s any
/// leftover entry belongs to a selection that was discarded before dispatch
/// and can never be observed again. Uses `tokio::time::Instant` so tests can
/// age entries under a paused clock.
const DELIVERY_ACK_ENTRY_TTL: Duration = Duration::from_secs(60);

struct PendingDeliveryAck {
    sender: tokio::sync::oneshot::Sender<()>,
    revoked: bool,
    inserted_at: tokio::time::Instant,
}

static PENDING_DELIVERY_ACKS: LazyLock<Mutex<HashMap<String, PendingDeliveryAck>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> MutexGuard<'static, HashMap<String, PendingDeliveryAck>> {
    PENDING_DELIVERY_ACKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Lazily reclaim entries older than [`DELIVERY_ACK_ENTRY_TTL`]. A revoked
/// entry is normally consumed by the late dispatch via [`take_revoked`]; if
/// the runtime receiver shut down first, the queued selection is dropped
/// without ever reaching that check, and only this sweep removes the entry.
/// Called from every registry op so no background task is needed.
fn purge_expired(pending: &mut HashMap<String, PendingDeliveryAck>) {
    pending.retain(|_, entry| entry.inserted_at.elapsed() < DELIVERY_ACK_ENTRY_TTL);
}

/// Receiver side of a registered selection acknowledgement. Dropping the
/// guard removes a still-pending registration (callback aborted, enqueue
/// failed after [`cancel`], or an answered callback after [`confirm`]), so
/// the sender cannot outlive the callback task in the static map. A revoked
/// entry is left in place: the late dispatch still has to observe the
/// revocation through [`take_revoked`].
pub(crate) struct DeliveryAck {
    message_id: String,
    receiver: tokio::sync::oneshot::Receiver<()>,
}

impl DeliveryAck {
    /// Wait for the dispatch-side confirmation. Resolves `Err` when the
    /// registration was dropped without confirming.
    pub(crate) async fn wait(&mut self) -> Result<(), tokio::sync::oneshot::error::RecvError> {
        (&mut self.receiver).await
    }
}

impl Drop for DeliveryAck {
    fn drop(&mut self) {
        let mut pending = pending();
        if pending
            .get(&self.message_id)
            .is_some_and(|entry| !entry.revoked)
        {
            pending.remove(&self.message_id);
        }
    }
}

/// Register the acknowledgement for a selection about to be enqueued. Must
/// run before the queue handoff so a runtime that consumes the selection
/// immediately cannot confirm into a not-yet-registered id.
pub(crate) fn register(message_id: &str) -> DeliveryAck {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut pending = pending();
    purge_expired(&mut pending);
    pending.insert(
        message_id.to_string(),
        PendingDeliveryAck {
            sender,
            revoked: false,
            inserted_at: tokio::time::Instant::now(),
        },
    );
    DeliveryAck {
        message_id: message_id.to_string(),
        receiver,
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
        // A send error only means the callback already stopped waiting and
        // dropped the receiver; nothing left to propagate.
        let _ = entry.sender.send(());
    }
}

/// Drop a registration without confirming, e.g. when the enqueue failed and
/// the selection never entered the queue.
pub(crate) fn cancel(message_id: &str) {
    pending().remove(message_id);
}

/// Mark a registered selection as revoked: the callback's bounded wait
/// elapsed while the selection was already enqueued, so the late dispatch
/// must observe the revocation via [`take_revoked`] instead of applying the
/// route change after the picker UI already reported failure.
pub(crate) fn revoke(message_id: &str) {
    let mut pending = pending();
    purge_expired(&mut pending);
    if let Some(entry) = pending.get_mut(message_id) {
        entry.revoked = true;
    }
}

/// Consume the revoked marker for a dequeued message. Returns `true` exactly
/// once for a selection whose callback already timed out and reported the
/// picker as unavailable. Ordinary traffic never registered, and a confirmed
/// selection was already removed by [`confirm`], so both return `false`.
pub(crate) fn take_revoked(message_id: &str) -> bool {
    let mut pending = pending();
    purge_expired(&mut pending);
    if pending.get(message_id).is_some_and(|entry| entry.revoked) {
        pending.remove(message_id);
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    fn is_registered(message_id: &str) -> bool {
        super::pending().contains_key(message_id)
    }

    /// Insert an entry with a backdated timestamp. TTL tests cannot advance
    /// a paused clock instead: the map is process-global, so entries created
    /// under another test's frozen clock would be measured against this
    /// runtime's real clock (and vice versa) by any concurrent purge.
    fn insert_entry_for_test(message_id: &str, revoked: bool, age: Duration) {
        let (sender, _receiver) = tokio::sync::oneshot::channel();
        super::pending().insert(
            message_id.to_string(),
            super::PendingDeliveryAck {
                sender,
                revoked,
                inserted_at: tokio::time::Instant::now()
                    .checked_sub(age)
                    .unwrap_or_else(tokio::time::Instant::now),
            },
        );
    }

    #[tokio::test]
    async fn registered_selection_confirm_fires() {
        let mut ack = super::register("selection-confirm");
        super::confirm("selection-confirm");
        assert!(ack.wait().await.is_ok());
    }

    #[tokio::test]
    async fn confirm_without_registration_is_a_noop() {
        // Ordinary traffic never registers; confirming its id must not
        // panic or disturb a later registration under the same id.
        super::confirm("ordinary-message");
        let mut ack = super::register("ordinary-message");
        super::confirm("ordinary-message");
        assert!(ack.wait().await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_registration_never_fires() {
        let mut ack = super::register("selection-cancelled");
        super::cancel("selection-cancelled");
        assert!(!is_registered("selection-cancelled"));
        super::confirm("selection-cancelled");
        assert!(ack.wait().await.is_err());
    }

    #[tokio::test]
    async fn dropped_registration_removes_entry() {
        // The callback task was aborted while waiting: no confirm and no
        // cancel ever ran. The guard drop must still remove the entry so
        // the sender cannot leak in the static map.
        let ack = super::register("selection-aborted");
        assert!(is_registered("selection-aborted"));
        drop(ack);
        assert!(!is_registered("selection-aborted"));
        // A late confirm for the removed id is a no-op.
        super::confirm("selection-aborted");
        assert!(!is_registered("selection-aborted"));
    }

    #[tokio::test]
    async fn revoked_selection_survives_guard_drop_and_is_taken_once() {
        let ack = super::register("selection-revoked");
        super::revoke("selection-revoked");
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
        // The enqueue-failed path cancels outright: the selection never
        // entered the queue, so there is nothing for dispatch to revoke.
        let ack = super::register("selection-enqueue-failed");
        super::cancel("selection-enqueue-failed");
        assert!(!super::take_revoked("selection-enqueue-failed"));
        drop(ack);
    }

    #[tokio::test]
    async fn ordinary_message_is_never_revoked() {
        assert!(!super::take_revoked("ordinary-never-registered"));
    }

    #[tokio::test]
    async fn revoked_entry_older_than_ttl_is_purged_without_consumption() {
        // The queued selection was dropped before dispatch (runtime
        // receiver shutdown), so nothing ever calls `take_revoked` for it:
        // only the lazy TTL sweep can reclaim the revoked entry. Backdated
        // to 2× the TTL so every runtime clock measures it as expired.
        insert_entry_for_test(
            "selection-stale-revoked",
            true,
            super::DELIVERY_ACK_ENTRY_TTL * 2,
        );
        assert!(is_registered("selection-stale-revoked"));
        // Any registry op performs the lazy sweep; confirming an id that
        // was never registered keeps ordinary traffic a no-op.
        super::confirm("ordinary-message");
        assert!(
            !is_registered("selection-stale-revoked"),
            "expired revoked entry must be reclaimed without consumption"
        );
        // The purged marker is gone for good: no late take can observe it.
        assert!(!super::take_revoked("selection-stale-revoked"));
    }

    #[tokio::test]
    async fn revoked_entry_younger_than_ttl_survives_lazy_sweep() {
        // Half the TTL: comfortably inside the bound even if a concurrent
        // paused-clock test measures this entry with a slightly advanced
        // clock (the ack-timeout tests advance ~5s).
        insert_entry_for_test(
            "selection-fresh-revoked",
            true,
            super::DELIVERY_ACK_ENTRY_TTL / 2,
        );
        // The sweep runs on every registry op but must not touch entries
        // still inside the TTL: the late dispatch can still observe them.
        let _other = super::register("selection-unrelated");
        assert!(super::take_revoked("selection-fresh-revoked"));
    }
}
