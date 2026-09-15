//! Inbound message debouncing for rapid senders.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Result of submitting a message to the debouncer.
pub enum DebounceResult {
    /// The message was accumulated and a timer is running. The caller should
    /// skip processing — the debounced message will arrive via the returned
    /// [`tokio::sync::oneshot::Receiver`] when the window expires.
    Pending(tokio::sync::oneshot::Receiver<String>),
    /// Debouncing is disabled (window = 0); pass the message through immediately.
    Passthrough(String),
}

struct DebouncerEntry {
    messages: Vec<String>,
    timer_handle: JoinHandle<()>,
    /// Sender for the final concatenated message. Replaced on each reset.
    result_tx: Option<tokio::sync::oneshot::Sender<String>>,
}

/// Accumulates rapid inbound messages per sender and fires a single combined
/// message after the debounce window elapses without new input.
pub struct MessageDebouncer {
    window: Duration,
    entries: Arc<Mutex<HashMap<String, DebouncerEntry>>>,
}

impl MessageDebouncer {
    /// Create a new debouncer with the given window.
    /// A zero duration disables debouncing (all messages pass through).
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Returns `true` when debouncing is active (non-zero window).
    pub fn enabled(&self) -> bool {
        !self.window.is_zero()
    }

    /// Submit a message for debouncing using the debouncer's default window.
    ///
    /// - If the window is zero, returns [`DebounceResult::Passthrough`] immediately.
    /// - Otherwise, accumulates the message under `sender_key` and returns
    ///   [`DebounceResult::Pending`] with a receiver that will eventually yield the
    ///   concatenated messages once the window expires.
    ///
    /// Each new message resets the timer. When the timer fires it concatenates all
    /// accumulated messages with `"\n"` and sends them through the oneshot channel.
    pub async fn debounce(&self, sender_key: &str, message: &str) -> DebounceResult {
        self.debounce_inner(sender_key, message, self.window).await
    }

    /// Submit a message for debouncing with an explicit per-call window.
    ///
    /// Behaves identically to [`debounce`](Self::debounce) but uses the provided
    /// `window` instead of the debouncer's default. This is used by channels that
    /// override the global debounce window (e.g., per-alias Telegram config).
    pub async fn debounce_with_window(
        &self,
        sender_key: &str,
        message: &str,
        window: Duration,
    ) -> DebounceResult {
        self.debounce_inner(sender_key, message, window).await
    }

    async fn debounce_inner(
        &self,
        sender_key: &str,
        message: &str,
        window: Duration,
    ) -> DebounceResult {
        if window.is_zero() {
            return DebounceResult::Passthrough(message.to_owned());
        }

        let mut entries = self.entries.lock().await;
        let entries_ref = Arc::clone(&self.entries);
        let key = sender_key.to_owned();

        if let Some(entry) = entries.get_mut(&key) {
            entry.timer_handle.abort();
            entry.messages.push(message.to_owned());

            let (tx, rx) = tokio::sync::oneshot::channel();
            entry.result_tx = Some(tx);

            entry.timer_handle = zeroclaw_spawn::spawn!(async move {
                tokio::time::sleep(window).await;
                fire_debounced(&entries_ref, &key).await;
            });

            DebounceResult::Pending(rx)
        } else {
            let (tx, rx) = tokio::sync::oneshot::channel();

            let key_clone = key.clone();
            let entries_spawn = Arc::clone(&self.entries);
            let handle = zeroclaw_spawn::spawn!(async move {
                tokio::time::sleep(window).await;
                fire_debounced(&entries_spawn, &key_clone).await;
            });

            entries.insert(
                key,
                DebouncerEntry {
                    messages: vec![message.to_owned()],
                    timer_handle: handle,
                    result_tx: Some(tx),
                },
            );

            DebounceResult::Pending(rx)
        }
    }

    /// Retire the buffered payload for `sender_key` without dispatching it.
    ///
    /// Aborts the pending timer and drops the accumulated messages together
    /// with the result sender, so the continuation that is waiting on the
    /// receiver observes a closed channel and exits without processing.
    ///
    /// Cancelling tracked work is not enough on its own: text that has already
    /// been folded into a bucket is not a task, and a later message for the
    /// same key would inherit it (the bucket is shared by key, its sender is
    /// replaced, and its messages are concatenated when the timer fires).
    /// Retiring the bucket is what retracts stopped
    /// instructions before any later message can reuse them.
    ///
    /// Returns `true` when a buffered payload was retired.
    pub async fn retire_pending(&self, sender_key: &str) -> bool {
        let mut entries = self.entries.lock().await;
        match entries.remove(sender_key) {
            Some(entry) => {
                entry.timer_handle.abort();
                true
            }
            None => false,
        }
    }
}

/// Called when the debounce timer fires. Removes the entry, concatenates all
/// accumulated messages, and sends the result through the oneshot channel.
async fn fire_debounced(entries: &Mutex<HashMap<String, DebouncerEntry>>, key: &str) {
    let mut map = entries.lock().await;
    if let Some(entry) = map.remove(key) {
        let combined = entry.messages.join("\n");
        if let Some(tx) = entry.result_tx {
            let _ = tx.send(combined);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passthrough_when_disabled() {
        let debouncer = MessageDebouncer::new(Duration::ZERO);
        assert!(!debouncer.enabled());
        match debouncer.debounce("user1", "hello").await {
            DebounceResult::Passthrough(msg) => assert_eq!(msg, "hello"),
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }

    #[tokio::test]
    async fn single_message_fires_after_window() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(50));
        let rx = match debouncer.debounce("user1", "hello").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let combined = rx.await.unwrap();
        assert_eq!(combined, "hello");
    }

    #[tokio::test]
    async fn multiple_messages_concatenated() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));

        let _rx1 = match debouncer.debounce("user1", "hello").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        tokio::time::sleep(Duration::from_millis(30)).await;
        let rx2 = match debouncer.debounce("user1", "world").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        let combined = rx2.await.unwrap();
        assert_eq!(combined, "hello\nworld");
    }

    #[tokio::test]
    async fn different_senders_independent() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(50));

        let rx_a = match debouncer.debounce("alice", "hi alice").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let rx_b = match debouncer.debounce("bob", "hi bob").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        assert_eq!(rx_a.await.unwrap(), "hi alice");
        assert_eq!(rx_b.await.unwrap(), "hi bob");
    }

    #[tokio::test]
    async fn debounce_with_window_passthrough_when_zero() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(100));
        assert!(debouncer.enabled());
        match debouncer
            .debounce_with_window("user1", "hello", Duration::ZERO)
            .await
        {
            DebounceResult::Passthrough(msg) => assert_eq!(msg, "hello"),
            DebounceResult::Pending(_) => panic!("expected Passthrough"),
        }
    }

    #[tokio::test]
    async fn debounce_with_window_overrides_default() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(5000)); // long default
        let rx = match debouncer
            .debounce_with_window("user1", "fast", Duration::from_millis(50))
            .await
        {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let combined = rx.await.unwrap();
        assert_eq!(combined, "fast");
    }

    #[tokio::test]
    async fn retire_pending_drops_buffered_payload_and_closes_receiver() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(5000));
        let rx = match debouncer.debounce("user1", "stopped instruction").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        assert!(debouncer.retire_pending("user1").await);

        // The continuation observes a cancelled receiver: it can never process
        // the retired text, even though the window has not expired.
        assert!(rx.await.is_err());
        // The bucket is gone, so a later message starts a fresh batch instead of
        // inheriting the retired one.
        let rx_next = match debouncer.debounce("user1", "later message").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        assert!(debouncer.retire_pending("user1").await);
        assert!(rx_next.await.is_err());
    }

    #[tokio::test]
    async fn retire_pending_only_touches_the_named_key() {
        // The window only has to outlive the retire below, which happens
        // immediately after both buckets are registered: a long window would
        // just make this test wait for bob's payload to expire.
        let debouncer = MessageDebouncer::new(Duration::from_millis(300));
        let rx_a = match debouncer.debounce("alice", "alice text").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        let rx_b = match debouncer.debounce("bob", "bob text").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        assert!(debouncer.retire_pending("alice").await);

        assert!(rx_a.await.is_err(), "retired key must not dispatch");
        assert_eq!(
            rx_b.await.unwrap(),
            "bob text",
            "other senders must keep their pending payload"
        );
    }

    #[tokio::test]
    async fn retire_pending_reports_missing_key() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(50));
        assert!(!debouncer.retire_pending("nobody").await);

        let rx = match debouncer.debounce("user1", "hello").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        assert!(debouncer.retire_pending("user1").await);
        assert!(rx.await.is_err());
        // Second retire finds nothing.
        assert!(!debouncer.retire_pending("user1").await);
    }

    #[tokio::test]
    async fn retired_payload_never_fires_after_window_expiry() {
        let debouncer = MessageDebouncer::new(Duration::from_millis(60));
        let rx = match debouncer.debounce("user1", "stopped").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };
        assert!(debouncer.retire_pending("user1").await);

        // Well past the window: the timer must not resurrect the payload.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(rx.await.is_err());
    }
}
