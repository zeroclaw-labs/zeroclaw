//! Per-(channel, peer) outbound pacing wrapper.
//!
//! Wraps a `dyn Channel` so consecutive `send` calls to the same recipient
//! honour a configured floor on cadence. Drafts and progress updates are
//! NOT paced — they are streaming UX events where slowing down would
//! visibly degrade the live response. Only the final `send` (the wire-
//! level outbound message) waits.
//!
//! `min_interval_secs == 0` returns the inner channel unchanged so the
//! pacing path has zero overhead for the default config.
//!
//! Pacing semantics: when a `send` arrives, if the previous send to the
//! same `recipient` finished less than `min_interval` ago, sleep the
//! difference before forwarding. Subsequent sends queue behind the lock
//! so a burst of N replies to the same peer takes ~`(N-1) * min_interval`
//! wall-clock. Different recipients are independent.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex;
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};

pub struct PacedChannel {
    inner: Arc<dyn Channel>,
    min_interval: Duration,
    /// Per-recipient timestamp of the most recently *scheduled* send. We
    /// store the scheduled time (not the start time) so a burst of replies
    /// keeps spreading rather than collapsing once the floor is met once.
    last_send: Mutex<HashMap<String, Instant>>,
}

impl PacedChannel {
    /// Wrap `inner` with a pacing floor. `min_interval_secs == 0` returns
    /// `inner` unchanged.
    pub fn wrap(inner: Arc<dyn Channel>, min_interval_secs: u64) -> Arc<dyn Channel> {
        if min_interval_secs == 0 {
            return inner;
        }
        Arc::new(Self {
            inner,
            min_interval: Duration::from_secs(min_interval_secs),
            last_send: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl Channel for PacedChannel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        let sleep_for = {
            let mut map = self.last_send.lock().await;
            let now = Instant::now();
            let next_allowed = map
                .get(&message.recipient)
                .copied()
                .map(|t| t + self.min_interval)
                .unwrap_or(now);
            let wait = next_allowed.saturating_duration_since(now);
            // Record the actual send instant so the next caller's floor
            // is measured from when this one fired, not when it queued.
            map.insert(message.recipient.clone(), now + wait);
            wait
        };
        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
        self.inner.send(message).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        self.inner.listen(tx).await
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }

    async fn start_typing(&self, recipient: &str) -> Result<()> {
        self.inner.start_typing(recipient).await
    }

    async fn stop_typing(&self, recipient: &str) -> Result<()> {
        self.inner.stop_typing(recipient).await
    }

    fn supports_draft_updates(&self) -> bool {
        self.inner.supports_draft_updates()
    }

    fn supports_multi_message_streaming(&self) -> bool {
        self.inner.supports_multi_message_streaming()
    }

    fn multi_message_delay_ms(&self) -> u64 {
        self.inner.multi_message_delay_ms()
    }

    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        // Drafts are streaming UX, not final outbound replies — pacing
        // them would freeze the live preview. Forward unchanged.
        self.inner.send_draft(message).await
    }

    async fn update_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        self.inner.update_draft(recipient, message_id, text).await
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        message_id: &str,
        text: &str,
    ) -> Result<()> {
        self.inner
            .update_draft_progress(recipient, message_id, text)
            .await
    }

    async fn finalize_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        // Finalise is the terminal write to the draft — pace it the same
        // way as `send` so a burst of streamed replies still respects the
        // floor.
        let sleep_for = {
            let mut map = self.last_send.lock().await;
            let now = Instant::now();
            let next_allowed = map
                .get(recipient)
                .copied()
                .map(|t| t + self.min_interval)
                .unwrap_or(now);
            let wait = next_allowed.saturating_duration_since(now);
            map.insert(recipient.to_string(), now + wait);
            wait
        };
        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
        self.inner.finalize_draft(recipient, message_id, text).await
    }

    async fn cancel_draft(&self, recipient: &str, message_id: &str) -> Result<()> {
        self.inner.cancel_draft(recipient, message_id).await
    }

    async fn add_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        self.inner.add_reaction(channel_id, message_id, emoji).await
    }

    async fn remove_reaction(&self, channel_id: &str, message_id: &str, emoji: &str) -> Result<()> {
        self.inner
            .remove_reaction(channel_id, message_id, emoji)
            .await
    }

    async fn pin_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        self.inner.pin_message(channel_id, message_id).await
    }

    async fn unpin_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        self.inner.unpin_message(channel_id, message_id).await
    }

    async fn redact_message(
        &self,
        channel_id: &str,
        message_id: &str,
        reason: Option<String>,
    ) -> Result<()> {
        self.inner
            .redact_message(channel_id, message_id, reason)
            .await
    }

    async fn request_approval(
        &self,
        recipient: &str,
        request: &ChannelApprovalRequest,
    ) -> Result<Option<ChannelApprovalResponse>> {
        self.inner.request_approval(recipient, request).await
    }

    async fn request_choice(
        &self,
        question: &str,
        choices: &[String],
        timeout: std::time::Duration,
    ) -> Result<Option<String>> {
        self.inner.request_choice(question, choices, timeout).await
    }

    fn supports_free_form_ask(&self) -> bool {
        self.inner.supports_free_form_ask()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingChannel {
        sends: AtomicUsize,
    }

    #[async_trait]
    impl Channel for CountingChannel {
        fn name(&self) -> &str {
            "counting"
        }
        async fn send(&self, _message: &SendMessage) -> Result<()> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn zero_interval_is_passthrough() {
        let inner = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
        });
        let wrapped = PacedChannel::wrap(inner.clone(), 0);
        // wrap() returns the inner Arc unchanged when interval == 0 — no
        // wrapper allocated, no atomic overhead, the default config pays
        // nothing for pacing it never asked for.
        assert!(Arc::ptr_eq(&wrapped, &(inner as Arc<dyn Channel>)));
    }

    #[tokio::test]
    async fn first_send_records_recipient_state() {
        let inner = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
        });
        // Use 1h to make the wait long enough that we can assert the
        // recipient row landed in the map without the test actually
        // sleeping. We never trigger a second send to the same peer,
        // so no real time elapses.
        let paced = PacedChannel {
            inner: inner.clone(),
            min_interval: Duration::from_secs(3600),
            last_send: Mutex::new(HashMap::new()),
        };
        paced
            .send(&SendMessage::new("hello", "alice"))
            .await
            .unwrap();
        assert_eq!(
            inner.sends.load(Ordering::SeqCst),
            1,
            "first send to a recipient must forward immediately",
        );
        let map = paced.last_send.lock().await;
        assert!(
            map.contains_key("alice"),
            "first send must record the recipient's last-send timestamp",
        );
    }

    #[tokio::test]
    async fn different_recipients_track_state_independently() {
        let inner = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
        });
        // 1h interval again — we only ever send once per recipient, so
        // pacing never actually triggers a sleep.
        let paced = PacedChannel {
            inner: inner.clone(),
            min_interval: Duration::from_secs(3600),
            last_send: Mutex::new(HashMap::new()),
        };
        paced
            .send(&SendMessage::new("hi alice", "alice"))
            .await
            .unwrap();
        paced
            .send(&SendMessage::new("hi bob", "bob"))
            .await
            .unwrap();
        assert_eq!(inner.sends.load(Ordering::SeqCst), 2);
        let map = paced.last_send.lock().await;
        assert_eq!(
            map.len(),
            2,
            "each recipient must own a row; the wait state for `alice` must not block `bob`",
        );
    }

    #[tokio::test]
    async fn small_interval_sleeps_long_enough_between_repeats() {
        // Real-time test with a tiny floor (50ms) so the suite stays fast.
        // Verifies that a second send to the same recipient actually waits
        // — covers the wire contract end-to-end without needing the
        // tokio test-util fake-time feature.
        let inner = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
        });
        let paced = PacedChannel {
            inner: inner.clone(),
            min_interval: Duration::from_millis(50),
            last_send: Mutex::new(HashMap::new()),
        };
        paced
            .send(&SendMessage::new("first", "alice"))
            .await
            .unwrap();
        let t1 = std::time::Instant::now();
        paced
            .send(&SendMessage::new("second", "alice"))
            .await
            .unwrap();
        let elapsed = t1.elapsed();
        assert!(
            elapsed >= Duration::from_millis(45),
            "second send to same recipient should wait ~min_interval; got {elapsed:?}",
        );
    }
}
