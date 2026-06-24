//! Inbound message debouncing for rapid senders.
//!
//! When users type fast and send multiple messages in quick succession, each
//! message would normally trigger a separate LLM call. [`MessageDebouncer`]
//! accumulates rapid messages per sender within a configurable time window and
//! emits them as a single concatenated message, reducing unnecessary agent runs.

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

    /// Submit a message for debouncing.
    ///
    /// - If the window is zero, returns [`DebounceResult::Passthrough`] immediately.
    /// - Otherwise, accumulates the message under `sender_key` and returns
    ///   [`DebounceResult::Pending`] with a receiver that will eventually yield the
    ///   concatenated messages once the window expires.
    ///
    /// Each new message resets the timer. When the timer fires it concatenates all
    /// accumulated messages with `"\n"` and sends them through the oneshot channel.
    pub async fn debounce(&self, sender_key: &str, message: &str) -> DebounceResult {
        if !self.enabled() {
            return DebounceResult::Passthrough(message.to_owned());
        }

        let mut entries = self.entries.lock().await;
        let entries_ref = Arc::clone(&self.entries);
        let key = sender_key.to_owned();
        let window = self.window;

        if let Some(entry) = entries.get_mut(&key) {
            // Cancel the previous timer — we'll start a fresh one.
            entry.timer_handle.abort();
            entry.messages.push(message.to_owned());

            // Replace the oneshot so the *new* caller gets the result.
            // The previous caller's receiver will see a `RecvError` (dropped sender),
            // which the dispatch loop interprets as "superseded — do nothing".
            let (tx, rx) = tokio::sync::oneshot::channel();
            entry.result_tx = Some(tx);

            // Spawn a new timer.
            let key_clone = key.clone();
            entry.timer_handle = tokio::spawn(async move {
                tokio::time::sleep(window).await;
                fire_debounced(&entries_ref, &key_clone).await;
            });

            DebounceResult::Pending(rx)
        } else {
            let (tx, rx) = tokio::sync::oneshot::channel();

            let key_clone = key.clone();
            let entries_spawn = Arc::clone(&self.entries);
            let handle = tokio::spawn(async move {
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

        // First message
        let _rx1 = match debouncer.debounce("user1", "hello").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        // Second message within window (resets timer)
        tokio::time::sleep(Duration::from_millis(30)).await;
        let rx2 = match debouncer.debounce("user1", "world").await {
            DebounceResult::Pending(rx) => rx,
            DebounceResult::Passthrough(_) => panic!("expected Pending"),
        };

        // The first receiver is dropped (superseded), second gets the combined result
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
}
