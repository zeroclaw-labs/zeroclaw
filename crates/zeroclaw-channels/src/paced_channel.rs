//! Per-(channel, peer) outbound pacing wrapper.
//!
//! Wraps a `dyn Channel` so consecutive `send` calls to the same recipient
//! honour a configured floor on cadence. Drafts and progress updates are
//! NOT paced — they are streaming UX events where slowing down would
//! visibly degrade the live response. Only the final `send` (the wire-
//! level outbound message) and `finalize_draft` enter the queue.
//!
//! `min_interval_secs == 0` returns the inner channel unchanged so the
//! pacing path has zero overhead for the default config.
//!
//! When the floor is active the wrapper holds a bounded FIFO queue
//! per recipient. A send that arrives while the floor still has time
//! left enqueues. A worker task drains the queue at the floor rate.
//! When the queue is full the newest send is dropped and a `WARN` is
//! emitted carrying enough attribution to diagnose the source without
//! leaking message body. See `PACING_RECIPIENT_CAP` for the upper bound
//! on distinct recipient state retained.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::{Mutex, oneshot};
use zeroclaw_api::attribution::{Attributable, Role};
use zeroclaw_api::channel::{
    Channel, ChannelApprovalRequest, ChannelApprovalResponse, ChannelMessage, SendMessage,
};
use zeroclaw_config::schema::{DEFAULT_REPLY_QUEUE_DEPTH, HasReplyPacing, PACING_RECIPIENT_CAP};

/// The outbound operation a queued slot will perform when its turn through
/// the pacing floor arrives. Both `send` and `finalize_draft` are paced, but
/// they dispatch to different inner-channel methods — normalizing both to a
/// plain `send` would route a draft finalization (an edit of an existing
/// message identified by `message_id`) through `send`, creating a new message
/// and leaving the draft stale on channels that support drafts.
enum PacedOp {
    /// A final outbound message. Dispatches to `inner.send`.
    Send(SendMessage),
    /// A terminal draft write. Dispatches to `inner.finalize_draft` so the
    /// channel edits the existing draft rather than posting a new message.
    FinalizeDraft {
        recipient: String,
        message_id: String,
        text: String,
    },
}

impl PacedOp {
    /// The recipient key this op paces against.
    fn recipient(&self) -> &str {
        match self {
            Self::Send(message) => &message.recipient,
            Self::FinalizeDraft { recipient, .. } => recipient,
        }
    }

    /// Character count of the payload, for the overflow-drop log.
    fn payload_chars(&self) -> usize {
        match self {
            Self::Send(message) => message.content.chars().count(),
            Self::FinalizeDraft { text, .. } => text.chars().count(),
        }
    }

    /// Dispatch to the correct inner-channel method for this op.
    async fn dispatch(self, inner: &Arc<dyn Channel>) -> Result<()> {
        match self {
            Self::Send(message) => inner.send(&message).await,
            Self::FinalizeDraft {
                recipient,
                message_id,
                text,
            } => inner.finalize_draft(&recipient, &message_id, &text).await,
        }
    }
}

/// Per-recipient queued operation waiting on its turn through the pacing floor.
struct PendingSend {
    op: PacedOp,
    /// One-shot back-channel for delivering the eventual send result to
    /// the caller. The caller awaits this so a paced `send()` still
    /// returns the inner channel's result rather than swallowing it.
    reply: oneshot::Sender<Result<()>>,
}

/// Per-recipient pacing state.
struct RecipientState {
    /// Wall-clock time after which the next send to this recipient may fire.
    next_allowed_at: Instant,
    /// Pending sends queued behind the floor. Drained FIFO by the worker.
    queue: VecDeque<PendingSend>,
    /// `true` while a worker task owns this recipient's queue. Prevents
    /// spawning a second worker for the same recipient.
    worker_running: bool,
    /// Sequence counter so the LRU eviction picks the least-recently-touched
    /// recipient when the cap is hit.
    last_touched: u64,
}

pub struct PacedChannel {
    inner: Arc<dyn Channel>,
    min_interval: Duration,
    queue_depth: usize,
    /// Per-recipient state. `tokio::sync::Mutex` so the worker can hold
    /// the lock across `.await` while draining.
    recipients: Arc<Mutex<RecipientMap>>,
}

/// Bounded recipient map with LRU eviction. Wrapped so the eviction logic
/// stays next to the touch counter rather than scattering.
struct RecipientMap {
    inner: HashMap<String, RecipientState>,
    touch_counter: u64,
}

impl RecipientMap {
    fn new() -> Self {
        Self {
            inner: HashMap::new(),
            touch_counter: 0,
        }
    }

    /// Bump the touch counter and return the new value. Used to stamp
    /// `last_touched` on whatever recipient row is being modified.
    fn touch(&mut self) -> u64 {
        self.touch_counter = self.touch_counter.wrapping_add(1);
        self.touch_counter
    }

    /// Evict the least-recently-touched recipient when the cap is reached.
    /// Only enforced on insert paths (existing rows are not affected, so an
    /// active queue can never be discarded out from under its worker — a
    /// recipient with a queue is by definition recently touched).
    fn evict_if_over_cap(&mut self) {
        if self.inner.len() < PACING_RECIPIENT_CAP {
            return;
        }
        // `iter()` walks the recipients to find the smallest `last_touched`.
        // Idle rows (no queue, no running worker) are preferred; an active
        // row only loses its slot if every other row is even more recent.
        let victim = self
            .inner
            .iter()
            .filter(|(_, s)| s.queue.is_empty() && !s.worker_running)
            .min_by_key(|(_, s)| s.last_touched)
            .map(|(k, _)| k.clone());
        if let Some(key) = victim {
            self.inner.remove(&key);
        }
    }
}

impl Attributable for PacedChannel {
    fn role(&self) -> Role {
        self.inner.role()
    }
    fn alias(&self) -> &str {
        self.inner.alias()
    }
}

impl PacedChannel {
    /// Wrap `inner` with a pacing floor sourced from `cfg`. When
    /// `cfg.reply_min_interval_secs() == 0` the inner `Arc` is returned
    /// unchanged so the default config has zero overhead — no wrapper,
    /// no mutex, no allocation.
    pub fn wrap(inner: Arc<dyn Channel>, cfg: &dyn HasReplyPacing) -> Arc<dyn Channel> {
        let min_interval_secs = cfg.reply_min_interval_secs();
        if min_interval_secs == 0 {
            return inner;
        }
        let depth_cfg = cfg.reply_queue_depth_max();
        let queue_depth = if depth_cfg == 0 {
            usize::from(DEFAULT_REPLY_QUEUE_DEPTH)
        } else {
            usize::from(depth_cfg)
        };
        Arc::new(Self {
            inner,
            min_interval: Duration::from_secs(min_interval_secs),
            queue_depth,
            recipients: Arc::new(Mutex::new(RecipientMap::new())),
        })
    }

    /// Enqueue or immediately dispatch a paced operation. Returns the inner
    /// channel's result (immediate path) or the worker's result awaited on a
    /// oneshot (queued path). Drops the newest op with a `WARN` when the queue
    /// is full and returns `Ok(())` — overflow is intentional behaviour, not an
    /// error the agent loop should retry.
    async fn paced_dispatch(&self, op: PacedOp) -> Result<()> {
        let recipient_key = op.recipient().to_string();

        // `decision` is built under the lock and consumed after release.
        // Three shapes share the same outcome carrier so the post-lock
        // section can use plain `if let` instead of branching on an enum.
        //
        // - `(Some(op), None, false)`  — immediate dispatch via inner channel
        // - `(None, Some(rx), spawn)` — enqueued; await result; maybe spawn worker
        // - `(None, None, false)` — overflow drop; return Ok
        let decision: (Option<PacedOp>, Option<oneshot::Receiver<Result<()>>>, bool) = {
            let mut map = self.recipients.lock().await;
            map.evict_if_over_cap();
            let now = Instant::now();
            let touch = map.touch();
            let state = map
                .inner
                .entry(recipient_key.clone())
                .or_insert(RecipientState {
                    next_allowed_at: now,
                    queue: VecDeque::new(),
                    worker_running: false,
                    last_touched: touch,
                });
            state.last_touched = touch;

            if state.queue.is_empty() && !state.worker_running && now >= state.next_allowed_at {
                state.next_allowed_at = now + self.min_interval;
                (Some(op), None, false)
            } else if state.queue.len() >= self.queue_depth {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject,)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "channel_alias": self.inner.alias(),
                            "recipient": redact_recipient(&recipient_key),
                            "queue_depth": state.queue.len(),
                            "queue_max": self.queue_depth,
                            "dropped_chars": op.payload_chars(),
                        })),
                    "paced channel queue full: dropping newest outbound message"
                );
                (None, None, false)
            } else {
                let (tx, rx) = oneshot::channel();
                state.queue.push_back(PendingSend { op, reply: tx });
                let spawn = !state.worker_running;
                if spawn {
                    state.worker_running = true;
                }
                (None, Some(rx), spawn)
            }
        };

        let (immediate, awaited, spawn_worker) = decision;
        if let Some(op) = immediate {
            return op.dispatch(&self.inner).await;
        }
        if let Some(rx) = awaited {
            if spawn_worker {
                self.spawn_drain_worker(recipient_key);
            }
            return rx.await.unwrap_or_else(|_| {
                Err(anyhow::Error::msg(
                    "paced channel worker dropped before send completed",
                ))
            });
        }
        Ok(())
    }

    /// Spawn the worker that drains a recipient's queue at the floor rate.
    /// One worker per recipient — re-entry is prevented by the
    /// `worker_running` flag held under the same lock that enqueues.
    fn spawn_drain_worker(&self, recipient: String) {
        let recipients = Arc::clone(&self.recipients);
        let inner = Arc::clone(&self.inner);
        let min_interval = self.min_interval;
        zeroclaw_spawn::spawn!(async move {
            loop {
                // Wait until the floor has elapsed for this recipient.
                let sleep_for = {
                    let map = recipients.lock().await;
                    let Some(state) = map.inner.get(&recipient) else {
                        return;
                    };
                    state
                        .next_allowed_at
                        .saturating_duration_since(Instant::now())
                };
                if !sleep_for.is_zero() {
                    tokio::time::sleep(sleep_for).await;
                }

                // Pop the next pending send, release the lock before
                // awaiting the actual wire call. Re-stamp the floor based
                // on when we dispatched.
                let pending = {
                    let mut map = recipients.lock().await;
                    let Some(state) = map.inner.get_mut(&recipient) else {
                        return;
                    };
                    if state.queue.is_empty() {
                        state.worker_running = false;
                        return;
                    }
                    state.next_allowed_at = Instant::now() + min_interval;
                    state.queue.pop_front()
                };
                if let Some(PendingSend { op, reply }) = pending {
                    let result = op.dispatch(&inner).await;
                    let _ = reply.send(result);
                }
            }
        });
    }
}

/// Redact a recipient identifier for log surfaces. The privacy contract
/// forbids raw JIDs / phones / user IDs in production logs; the redaction
/// preserves the channel-alias-scoped shape (length + first/last char) so
/// operators can still correlate dropped sends to recipient cohorts.
fn redact_recipient(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    if chars.len() <= 2 {
        return "***".to_string();
    }
    let first = chars.first().copied().unwrap_or('*');
    let last = chars.last().copied().unwrap_or('*');
    format!("{first}***{last}<len={}>", chars.len())
}

#[async_trait]
impl Channel for PacedChannel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.paced_dispatch(PacedOp::Send(message.clone())).await
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
        // Finalise is the terminal write to the draft — route it through the
        // same pacing queue as `send` so a burst of streamed replies respects
        // the floor and the overflow contract. The op preserves its identity
        // so the worker dispatches to `inner.finalize_draft` (editing the
        // existing draft) rather than `inner.send` (posting a new message).
        self.paced_dispatch(PacedOp::FinalizeDraft {
            recipient: recipient.to_string(),
            message_id: message_id.to_string(),
            text: text.to_string(),
        })
        .await
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
        timeout: Duration,
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

    /// Minimal `HasReplyPacing` for tests so we can construct pacing
    /// configs without dragging a full `*Config` literal into every
    /// case. Mirrors the production trait shape exactly.
    struct PacingFixture {
        interval_secs: u64,
        depth: u16,
    }
    impl HasReplyPacing for PacingFixture {
        fn reply_min_interval_secs(&self) -> u64 {
            self.interval_secs
        }
        fn reply_queue_depth_max(&self) -> u16 {
            self.depth
        }
    }

    struct CountingChannel {
        sends: AtomicUsize,
        finalize_drafts: AtomicUsize,
    }

    impl Attributable for CountingChannel {
        fn role(&self) -> Role {
            // Reuse an existing channel kind for testing only.
            Role::Channel(zeroclaw_api::attribution::ChannelKind::Cli)
        }
        fn alias(&self) -> &str {
            "counting"
        }
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
        fn supports_draft_updates(&self) -> bool {
            true
        }
        async fn finalize_draft(
            &self,
            _recipient: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<()> {
            self.finalize_drafts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn zero_interval_is_passthrough() {
        let inner = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let cfg = PacingFixture {
            interval_secs: 0,
            depth: 0,
        };
        let wrapped = PacedChannel::wrap(inner.clone(), &cfg);
        // wrap() returns the inner Arc unchanged when interval == 0 — no
        // wrapper allocated, no atomic overhead, the default config pays
        // nothing for pacing it never asked for.
        assert!(Arc::ptr_eq(&wrapped, &(inner as Arc<dyn Channel>)));
    }

    #[tokio::test]
    async fn first_send_records_recipient_state() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        // Use 1h to make the wait long enough that we can assert the
        // recipient row landed in the map without the test actually
        // sleeping. We never trigger a second send to the same peer,
        // so no real time elapses.
        let cfg = PacingFixture {
            interval_secs: 3600,
            depth: 0,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        paced
            .send(&SendMessage::new("hello", "alice"))
            .await
            .unwrap();
        assert_eq!(
            counting.sends.load(Ordering::SeqCst),
            1,
            "first send to a recipient must forward immediately",
        );
    }

    #[tokio::test]
    async fn different_recipients_track_state_independently() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        // 1h interval again — we only ever send once per recipient, so
        // pacing never actually triggers a sleep on the immediate path.
        let cfg = PacingFixture {
            interval_secs: 3600,
            depth: 0,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        paced
            .send(&SendMessage::new("hi alice", "alice"))
            .await
            .unwrap();
        paced
            .send(&SendMessage::new("hi bob", "bob"))
            .await
            .unwrap();
        assert_eq!(
            counting.sends.load(Ordering::SeqCst),
            2,
            "each recipient must dispatch on its own; alice's floor must not block bob's send",
        );
    }

    #[tokio::test]
    async fn small_interval_sleeps_long_enough_between_repeats() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        let cfg = PacingFixture {
            interval_secs: 1,
            depth: 4,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        paced
            .send(&SendMessage::new("first", "alice"))
            .await
            .unwrap();
        let t1 = Instant::now();
        paced
            .send(&SendMessage::new("second", "alice"))
            .await
            .unwrap();
        let elapsed = t1.elapsed();
        assert!(
            elapsed >= Duration::from_millis(900),
            "second send to same recipient should wait ~min_interval; got {elapsed:?}",
        );
        assert_eq!(counting.sends.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn queue_overflow_drops_newest_and_warns() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        let cfg = PacingFixture {
            interval_secs: 1,
            depth: 2,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        // First send fires immediately and starts the floor (next allowed
        // ~1s out). It also takes no path through the queue, so the worker
        // is not yet spawned.
        paced
            .send(&SendMessage::new("first", "alice"))
            .await
            .unwrap();
        // Enqueue `a` and `b` by spawning the sends as tasks so they
        // actually drive the recipient state's `queue.push_back`. If we
        // just held them as un-polled futures the queue would stay empty
        // and the overflow path would never trigger.
        let paced_a = Arc::clone(&paced);
        let paced_b = Arc::clone(&paced);
        let h_a =
            zeroclaw_spawn::spawn!(
                async move { paced_a.send(&SendMessage::new("a", "alice")).await }
            );
        let h_b =
            zeroclaw_spawn::spawn!(
                async move { paced_b.send(&SendMessage::new("b", "alice")).await }
            );
        // Yield enough times for both spawned tasks to make it past the
        // lock acquire and into the queue. 50ms is well inside the 1s
        // pacing floor so the worker hasn't drained anything yet.
        tokio::time::sleep(Duration::from_millis(50)).await;
        // The third lands while the queue is full → drop + WARN, returns Ok.
        paced
            .send(&SendMessage::new("overflow", "alice"))
            .await
            .unwrap();
        // Allow the workers to drain at the 1s floor.
        let (a, b) = tokio::join!(h_a, h_b);
        a.unwrap().unwrap();
        b.unwrap().unwrap();
        // 1 immediate + 2 queued = 3 dispatches; the overflow drop must not
        // have reached the inner channel.
        assert_eq!(
            counting.sends.load(Ordering::SeqCst),
            3,
            "queue overflow must drop the newest send before the inner channel sees it",
        );
    }

    #[tokio::test]
    async fn finalize_draft_dispatches_to_inner_finalize_not_send() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        // 1h floor: the first op fires immediately, so no real time elapses.
        let cfg = PacingFixture {
            interval_secs: 3600,
            depth: 4,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        paced
            .finalize_draft("alice", "msg-1", "final text")
            .await
            .unwrap();
        // A draft finalization must edit the existing draft via the inner
        // channel's finalize_draft — routing it through send would post a new
        // message and leave the draft stale.
        assert_eq!(
            counting.finalize_drafts.load(Ordering::SeqCst),
            1,
            "finalize_draft must dispatch to inner.finalize_draft",
        );
        assert_eq!(
            counting.sends.load(Ordering::SeqCst),
            0,
            "finalize_draft must not be routed through inner.send",
        );
    }

    #[tokio::test]
    async fn queued_finalize_draft_preserves_op_through_worker() {
        let counting = Arc::new(CountingChannel {
            sends: AtomicUsize::new(0),
            finalize_drafts: AtomicUsize::new(0),
        });
        let inner: Arc<dyn Channel> = counting.clone();
        let cfg = PacingFixture {
            interval_secs: 1,
            depth: 4,
        };
        let paced = PacedChannel::wrap(inner, &cfg);
        // First op fires immediately and starts the floor.
        paced
            .send(&SendMessage::new("first", "alice"))
            .await
            .unwrap();
        // Second op (a finalize) is queued behind the floor and drained by
        // the worker — it must still dispatch as a finalize, not a send.
        paced
            .finalize_draft("alice", "msg-1", "final text")
            .await
            .unwrap();
        assert_eq!(
            counting.finalize_drafts.load(Ordering::SeqCst),
            1,
            "queued finalize_draft must dispatch to inner.finalize_draft via the worker",
        );
        assert_eq!(
            counting.sends.load(Ordering::SeqCst),
            1,
            "only the first send should reach inner.send; the finalize must not",
        );
    }
}
