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

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};

static PENDING_DELIVERY_ACKS: LazyLock<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pending() -> MutexGuard<'static, HashMap<String, tokio::sync::oneshot::Sender<()>>> {
    PENDING_DELIVERY_ACKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Register the acknowledgement for a selection about to be enqueued. Must
/// run before the queue handoff so a runtime that consumes the selection
/// immediately cannot confirm into a not-yet-registered id.
pub(crate) fn register(message_id: &str) -> tokio::sync::oneshot::Receiver<()> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    pending().insert(message_id.to_string(), tx);
    rx
}

/// Confirm that the selection reached runtime command handling. Messages
/// that did not originate from the picker have no registration, so this is
/// a no-op for ordinary traffic.
pub(crate) fn confirm(message_id: &str) {
    let sender = pending().remove(message_id);
    if let Some(sender) = sender {
        // A send error only means the callback already stopped waiting and
        // dropped the receiver; nothing left to propagate.
        let _ = sender.send(());
    }
}

/// Drop a registration without confirming, e.g. when the enqueue failed or
/// the callback's bounded wait elapsed first.
pub(crate) fn cancel(message_id: &str) {
    pending().remove(message_id);
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn registered_selection_confirm_fires() {
        let ack = super::register("selection-confirm");
        super::confirm("selection-confirm");
        assert!(ack.await.is_ok());
    }

    #[tokio::test]
    async fn confirm_without_registration_is_a_noop() {
        // Ordinary traffic never registers; confirming its id must not
        // panic or disturb a later registration under the same id.
        super::confirm("ordinary-message");
        let ack = super::register("ordinary-message");
        super::confirm("ordinary-message");
        assert!(ack.await.is_ok());
    }

    #[tokio::test]
    async fn cancelled_registration_never_fires() {
        let ack = super::register("selection-cancelled");
        super::cancel("selection-cancelled");
        super::confirm("selection-cancelled");
        assert!(ack.await.is_err());
    }
}
