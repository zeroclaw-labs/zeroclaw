//! Bounded, replayable subscription sources for RPC streams.
//!
//! A [`SubscriptionHub`] holds one ring per [`Source`]. Producers append to a
//! ring and never wait for a subscriber; each subscriber is only a cursor (the
//! next sequence number it wants). Every ring is bounded by a frame count and a
//! byte cap, and all rings share one process-wide byte budget: when it is
//! exceeded, the oldest frame across all rings is evicted.
//!
//! Nothing is dropped silently. A cursor that points into a gap, whether the
//! frames were evicted, never reached the hub, or predate a restart, reads
//! [`Read::Lagged`] with the first missing sequence and the first one still
//! available, and the subscriber resumes from there.
//!
//! Sequence numbers start at 1 per source and are never reused within a hub,
//! so a client can resume with `since_seq` (the last sequence it saw).
//!
//! Besides the fixed daemon streams, each session can have its own ring
//! ([`SubscriptionHub::session_source`]). A session-lifetime turn publishes
//! its `session/update` frames there, and every attached viewer reads them
//! through a cursor like any other subscriber, so a turn never waits on a
//! viewer and a viewer that reconnects resumes with `since_seq`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// A subscribable stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Every frame on the daemon event bus (`logs/subscribe`).
    Logs,
    /// Observer frames only: agent, tool, LLM, history-trim, error
    /// (`events/subscribe`).
    Events,
    /// One session's `session/update` frames (`session/attach`). The number
    /// is the hub's handle for the session id, from
    /// [`SubscriptionHub::session_source`]; it is never reused, so a session
    /// recreated under the same id gets a fresh ring.
    Session(u64),
}

impl Source {
    const ALL: [Self; 2] = [Self::Logs, Self::Events];
}

/// Per-ring caps.
#[derive(Clone, Copy, Debug)]
pub struct RingLimits {
    pub max_frames: usize,
    pub max_bytes: usize,
}

/// Default per-source ring caps.
pub const DEFAULT_RING_LIMITS: RingLimits = RingLimits {
    max_frames: 2_048,
    max_bytes: 4 * 1024 * 1024,
};

/// Default process-wide byte budget across every ring.
pub const DEFAULT_BYTE_BUDGET: usize = 16 * 1024 * 1024;

/// Frames a subscriber reads per wakeup before yielding.
pub const READ_BATCH: usize = 64;

/// What a cursor reads.
#[derive(Debug, PartialEq)]
pub enum Read {
    /// Frames at or after the cursor, in order, with no gap before the first.
    Frames(Vec<(u64, Arc<Value>)>),
    /// Frames from `from_seq` up to `resume_seq` are gone. Continue from
    /// `resume_seq`.
    Lagged { from_seq: u64, resume_seq: u64 },
}

struct Entry {
    seq: u64,
    arrival: u64,
    bytes: usize,
    frame: Arc<Value>,
}

struct Ring {
    limits: RingLimits,
    entries: VecDeque<Entry>,
    bytes: usize,
    /// Sequence number the next frame will get.
    next_seq: u64,
}

impl Ring {
    fn new(limits: RingLimits) -> Self {
        Self {
            limits,
            entries: VecDeque::new(),
            bytes: 0,
            next_seq: 1,
        }
    }

    fn pop_front(&mut self) -> Option<usize> {
        let entry = self.entries.pop_front()?;
        self.bytes -= entry.bytes;
        Some(entry.bytes)
    }
}

struct HubState {
    rings: HashMap<Source, Ring>,
    total_bytes: usize,
    next_arrival: u64,
}

/// A viewer attached to a session ring.
struct Viewer {
    /// The connection that attached it (see [`SubscriptionHub::add_viewer`]).
    connection: u64,
    cancel: CancellationToken,
}

/// Per-session bookkeeping: the ring handle, its viewers, and whether a turn
/// is currently delivering through the ring.
struct SessionEntry {
    id: u64,
    viewers: HashMap<String, Viewer>,
    routed: usize,
}

#[derive(Default)]
struct Sessions {
    by_id: HashMap<String, SessionEntry>,
    next_id: u64,
}

/// The process's subscription sources. Cheap to share behind an `Arc`.
pub struct SubscriptionHub {
    state: Mutex<HubState>,
    notify: Mutex<HashMap<Source, Arc<Notify>>>,
    sessions: Mutex<Sessions>,
    /// Signalled whenever a session's viewer set changes.
    viewers_changed: Notify,
    session_ring: RingLimits,
    byte_budget: usize,
    bus_attached: AtomicBool,
}

impl Default for SubscriptionHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionHub {
    /// A hub with the default ring caps and byte budget.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_RING_LIMITS, DEFAULT_BYTE_BUDGET)
    }

    /// A hub with explicit caps (tests use small ones).
    #[must_use]
    pub fn with_limits(ring: RingLimits, byte_budget: usize) -> Self {
        Self {
            state: Mutex::new(HubState {
                rings: Source::ALL
                    .into_iter()
                    .map(|source| (source, Ring::new(ring)))
                    .collect(),
                total_bytes: 0,
                next_arrival: 0,
            }),
            notify: Mutex::new(
                Source::ALL
                    .into_iter()
                    .map(|source| (source, Arc::new(Notify::new())))
                    .collect(),
            ),
            sessions: Mutex::new(Sessions::default()),
            viewers_changed: Notify::new(),
            session_ring: ring,
            byte_budget,
            bus_attached: AtomicBool::new(false),
        }
    }

    /// Append a frame and wake that source's subscribers. Never waits.
    /// Returns the frame's sequence number.
    pub fn publish(&self, source: Source, frame: Value) -> u64 {
        let bytes = serde_json::to_vec(&frame).map_or(0, |encoded| encoded.len());
        let seq = {
            let mut state = self.state.lock();
            let arrival = state.next_arrival;
            state.next_arrival += 1;
            let mut evicted = 0;
            let session_ring = self.session_ring;
            let ring = state
                .rings
                .entry(source)
                .or_insert_with(|| Ring::new(session_ring));
            let seq = ring.next_seq;
            ring.next_seq += 1;
            ring.entries.push_back(Entry {
                seq,
                arrival,
                bytes,
                frame: Arc::new(frame),
            });
            ring.bytes += bytes;
            while ring.entries.len() > ring.limits.max_frames || ring.bytes > ring.limits.max_bytes
            {
                match ring.pop_front() {
                    Some(freed) => evicted += freed,
                    None => break,
                }
            }
            state.total_bytes = state.total_bytes + bytes - evicted;
            Self::enforce_budget(&mut state, self.byte_budget);
            seq
        };
        self.notifier(source).notify_waiters();
        seq
    }

    /// Evict the oldest frame across all rings until the budget holds.
    fn enforce_budget(state: &mut HubState, budget: usize) {
        while state.total_bytes > budget {
            let oldest = state
                .rings
                .iter()
                .filter_map(|(source, ring)| ring.entries.front().map(|e| (e.arrival, *source)))
                .min_by_key(|(arrival, _)| *arrival);
            let Some((_, source)) = oldest else { break };
            let freed = state
                .rings
                .get_mut(&source)
                .and_then(Ring::pop_front)
                .unwrap_or(0);
            state.total_bytes -= freed;
        }
    }

    /// Record that `count` frames for `source` were lost before reaching the
    /// hub. Their sequence numbers are skipped, so any cursor that would have
    /// read them reads [`Read::Lagged`] instead.
    pub fn note_loss(&self, source: Source, count: u64) {
        if count == 0 {
            return;
        }
        let session_ring = self.session_ring;
        self.state
            .lock()
            .rings
            .entry(source)
            .or_insert_with(|| Ring::new(session_ring))
            .next_seq += count;
        self.notifier(source).notify_waiters();
    }

    /// The sequence number of the newest frame (0 before the first).
    #[must_use]
    pub fn head_seq(&self, source: Source) -> u64 {
        self.state
            .lock()
            .rings
            .get(&source)
            .map_or(0, |ring| ring.next_seq - 1)
    }

    /// Up to `max` frames at or after `cursor`, or the gap the cursor is in.
    #[must_use]
    pub fn read(&self, source: Source, cursor: u64, max: usize) -> Read {
        let state = self.state.lock();
        // A session ring exists from its first frame; before that there is
        // nothing to read and no gap.
        let Some(ring) = state.rings.get(&source) else {
            return Read::Frames(Vec::new());
        };
        let first = ring.entries.partition_point(|entry| entry.seq < cursor);
        match ring.entries.get(first) {
            Some(entry) if entry.seq > cursor => Read::Lagged {
                from_seq: cursor,
                resume_seq: entry.seq,
            },
            Some(_) => Read::Frames(
                ring.entries
                    .range(first..)
                    .take(max)
                    .map(|entry| (entry.seq, Arc::clone(&entry.frame)))
                    .collect(),
            ),
            None if cursor < ring.next_seq => Read::Lagged {
                from_seq: cursor,
                resume_seq: ring.next_seq,
            },
            None => Read::Frames(Vec::new()),
        }
    }

    /// The wakeup for `source`, signalled on every publish and loss.
    #[must_use]
    pub fn notifier(&self, source: Source) -> Arc<Notify> {
        Arc::clone(
            self.notify
                .lock()
                .entry(source)
                .or_insert_with(|| Arc::new(Notify::new())),
        )
    }

    /// The ring for `session_id`, created on first use.
    pub fn session_source(&self, session_id: &str) -> Source {
        let mut sessions = self.sessions.lock();
        if let Some(entry) = sessions.by_id.get(session_id) {
            return Source::Session(entry.id);
        }
        sessions.next_id += 1;
        let id = sessions.next_id;
        sessions.by_id.insert(
            session_id.to_string(),
            SessionEntry {
                id,
                viewers: HashMap::new(),
                routed: 0,
            },
        );
        Source::Session(id)
    }

    /// Record a viewer of `session_id` whose delivery ends when `cancel`
    /// fires. `connection` identifies the attaching connection, so a
    /// connection can tell whether it already views the session.
    pub fn add_viewer(
        &self,
        session_id: &str,
        subscription_id: &str,
        connection: u64,
        cancel: CancellationToken,
    ) {
        let source = self.session_source(session_id);
        let mut sessions = self.sessions.lock();
        if let Some(entry) = sessions
            .by_id
            .get_mut(session_id)
            .filter(|entry| Source::Session(entry.id) == source)
        {
            entry
                .viewers
                .insert(subscription_id.to_string(), Viewer { connection, cancel });
        }
        drop(sessions);
        self.viewers_changed.notify_waiters();
    }

    /// Forget a viewer when its delivery ends.
    pub fn remove_viewer(&self, session_id: &str, subscription_id: &str) {
        let removed = self
            .sessions
            .lock()
            .by_id
            .get_mut(session_id)
            .and_then(|entry| entry.viewers.remove(subscription_id))
            .is_some();
        if removed {
            self.viewers_changed.notify_waiters();
        }
    }

    /// How many viewers `session_id` has.
    #[must_use]
    pub fn viewer_count(&self, session_id: &str) -> usize {
        self.sessions
            .lock()
            .by_id
            .get(session_id)
            .map_or(0, |entry| entry.viewers.len())
    }

    /// Whether `connection` already views `session_id`.
    #[must_use]
    pub fn viewed_by(&self, session_id: &str, connection: u64) -> bool {
        self.sessions
            .lock()
            .by_id
            .get(session_id)
            .is_some_and(|entry| {
                entry
                    .viewers
                    .values()
                    .any(|viewer| viewer.connection == connection)
            })
    }

    /// Resolves once `session_id` has no viewers. Returns at once when it
    /// has none now.
    pub async fn viewers_gone(&self, session_id: &str) {
        loop {
            let changed = self.viewers_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.viewer_count(session_id) == 0 {
                return;
            }
            changed.await;
        }
    }

    /// Deliver `session_id`'s turn notifications through its ring until the
    /// returned guard drops.
    pub fn route_session(self: &Arc<Self>, session_id: &str) -> SessionRoute {
        let _ = self.session_source(session_id);
        if let Some(entry) = self.sessions.lock().by_id.get_mut(session_id) {
            entry.routed += 1;
        }
        SessionRoute {
            hub: Arc::clone(self),
            session_id: session_id.to_string(),
        }
    }

    /// The session's ring, when a turn is delivering through it now.
    #[must_use]
    pub fn routed_source(&self, session_id: &str) -> Option<Source> {
        self.sessions
            .lock()
            .by_id
            .get(session_id)
            .filter(|entry| entry.routed > 0)
            .map(|entry| Source::Session(entry.id))
    }

    /// Drop a session's ring and end its viewers' deliveries. Called when
    /// the session itself goes away; a later session under the same id gets
    /// a fresh ring.
    pub fn release_session(&self, session_id: &str) {
        let Some(entry) = self.sessions.lock().by_id.remove(session_id) else {
            return;
        };
        let source = Source::Session(entry.id);
        for viewer in entry.viewers.values() {
            viewer.cancel.cancel();
        }
        {
            let mut state = self.state.lock();
            if let Some(ring) = state.rings.remove(&source) {
                state.total_bytes -= ring.bytes;
            }
        }
        if let Some(notify) = self.notify.lock().remove(&source) {
            notify.notify_waiters();
        }
        self.viewers_changed.notify_waiters();
    }

    /// Total bytes held across all rings.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.state.lock().total_bytes
    }

    /// Feed this hub from the daemon event bus: every frame into
    /// [`Source::Logs`], observer frames into [`Source::Events`]. Idempotent:
    /// only the first call starts the pump.
    ///
    /// Pairing credentials are dropped before they reach a ring, so they are
    /// never replayed, and the internal marker is stripped from everything
    /// else. A broadcast overrun is recorded with [`Self::note_loss`], so
    /// subscribers see `lagged`, not a silent gap.
    pub fn attach_bus(self: &Arc<Self>, tx: &tokio::sync::broadcast::Sender<Value>) {
        if self.bus_attached.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut rx = tx.subscribe();
        let hub = Arc::downgrade(self);
        zeroclaw_spawn::spawn!(async move {
            loop {
                let received = rx.recv().await;
                let Some(hub) = hub.upgrade() else { break };
                match received {
                    Ok(mut frame) => {
                        if zeroclaw_log::frame_carries_ephemeral_credentials(&frame) {
                            continue;
                        }
                        zeroclaw_log::strip_ephemeral_broadcast_marker(&mut frame);
                        if frame.get("source").and_then(Value::as_str) == Some("observability") {
                            hub.publish(Source::Events, frame.clone());
                        }
                        hub.publish(Source::Logs, frame);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        hub.note_loss(Source::Logs, missed);
                        // Some of the missed frames may have been observer
                        // frames; mark one gap so an events cursor cannot
                        // read across the overrun as if it were continuous.
                        hub.note_loss(Source::Events, 1);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

/// Keeps a session's turn notifications on its ring while held. See
/// [`SubscriptionHub::route_session`].
pub struct SessionRoute {
    hub: Arc<SubscriptionHub>,
    session_id: String,
}

impl Drop for SessionRoute {
    fn drop(&mut self) {
        if let Some(entry) = self.hub.sessions.lock().by_id.get_mut(&self.session_id) {
            entry.routed = entry.routed.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frames(read: Read) -> Vec<u64> {
        match read {
            Read::Frames(frames) => frames.into_iter().map(|(seq, _)| seq).collect(),
            Read::Lagged { .. } => panic!("expected frames, got {read:?}"),
        }
    }

    fn small_hub(max_frames: usize) -> SubscriptionHub {
        SubscriptionHub::with_limits(
            RingLimits {
                max_frames,
                max_bytes: usize::MAX,
            },
            usize::MAX,
        )
    }

    #[test]
    fn sequence_numbers_start_at_one_per_source() {
        let hub = small_hub(8);
        assert_eq!(hub.head_seq(Source::Logs), 0);
        assert_eq!(hub.publish(Source::Logs, json!({"n": 1})), 1);
        assert_eq!(hub.publish(Source::Logs, json!({"n": 2})), 2);
        assert_eq!(hub.publish(Source::Events, json!({"n": 1})), 1);
        assert_eq!(hub.head_seq(Source::Logs), 2);
    }

    #[test]
    fn resume_replays_exactly_the_missing_range() {
        let hub = small_hub(64);
        for n in 1..=10 {
            hub.publish(Source::Logs, json!({ "n": n }));
        }
        // A client that saw seq 6 resumes at 7 and gets 7..=10, nothing else.
        assert_eq!(frames(hub.read(Source::Logs, 7, 64)), [7, 8, 9, 10]);
        // A caught-up cursor reads nothing, not a gap.
        assert_eq!(frames(hub.read(Source::Logs, 11, 64)), Vec::<u64>::new());
        // Batches respect the cap and stay contiguous.
        assert_eq!(frames(hub.read(Source::Logs, 3, 2)), [3, 4]);
    }

    #[test]
    fn overflow_reports_lagged_with_the_resume_point() {
        let hub = small_hub(4);
        for n in 1..=10 {
            hub.publish(Source::Logs, json!({ "n": n }));
        }
        // Frames 1..=6 were evicted by the count cap.
        assert_eq!(
            hub.read(Source::Logs, 2, 64),
            Read::Lagged {
                from_seq: 2,
                resume_seq: 7
            }
        );
        assert_eq!(frames(hub.read(Source::Logs, 7, 64)), [7, 8, 9, 10]);
    }

    #[test]
    fn the_byte_cap_evicts_within_a_ring() {
        let frame = json!({"payload": "x".repeat(100)});
        let size = serde_json::to_vec(&frame).unwrap().len();
        let hub = SubscriptionHub::with_limits(
            RingLimits {
                max_frames: usize::MAX,
                max_bytes: size * 3,
            },
            usize::MAX,
        );
        for _ in 0..5 {
            hub.publish(Source::Logs, frame.clone());
        }
        assert_eq!(hub.total_bytes(), size * 3);
        assert!(matches!(
            hub.read(Source::Logs, 1, 64),
            Read::Lagged {
                from_seq: 1,
                resume_seq: 3
            }
        ));
    }

    #[test]
    fn budget_eviction_takes_the_oldest_frame_across_rings_and_is_reported() {
        let frame = json!({"payload": "y".repeat(100)});
        let size = serde_json::to_vec(&frame).unwrap().len();
        let hub = SubscriptionHub::with_limits(
            RingLimits {
                max_frames: usize::MAX,
                max_bytes: usize::MAX,
            },
            size * 4,
        );
        // Oldest two frames go to Events, then Logs fills the budget.
        hub.publish(Source::Events, frame.clone());
        hub.publish(Source::Events, frame.clone());
        for _ in 0..4 {
            hub.publish(Source::Logs, frame.clone());
        }
        assert!(hub.total_bytes() <= size * 4);
        // The budget evicted from the other ring: its oldest frames, not Logs'.
        assert_eq!(
            hub.read(Source::Events, 1, 64),
            Read::Lagged {
                from_seq: 1,
                resume_seq: 3
            }
        );
        assert_eq!(frames(hub.read(Source::Logs, 1, 64)), [1, 2, 3, 4]);
    }

    #[test]
    fn lost_frames_are_a_gap_not_a_silent_skip() {
        let hub = small_hub(64);
        hub.publish(Source::Logs, json!({"n": 1}));
        hub.note_loss(Source::Logs, 3);
        hub.publish(Source::Logs, json!({"n": 5}));
        assert_eq!(frames(hub.read(Source::Logs, 1, 1)), [1]);
        assert_eq!(
            hub.read(Source::Logs, 2, 64),
            Read::Lagged {
                from_seq: 2,
                resume_seq: 5
            }
        );
        // A loss at the head is also reported, not read as "caught up".
        hub.note_loss(Source::Logs, 2);
        assert_eq!(
            hub.read(Source::Logs, 6, 64),
            Read::Lagged {
                from_seq: 6,
                resume_seq: 8
            }
        );
    }

    #[test]
    fn session_rings_are_created_on_first_use_and_released_with_the_session() {
        let hub = Arc::new(small_hub(64));
        let source = hub.session_source("s1");
        assert_eq!(hub.session_source("s1"), source, "one ring per session id");
        assert_eq!(hub.head_seq(source), 0);
        assert_eq!(frames(hub.read(source, 1, 64)), Vec::<u64>::new());

        hub.publish(source, json!({"type": "agent_message_chunk"}));
        hub.publish(source, json!({"type": "turn_complete"}));
        assert_eq!(frames(hub.read(source, 1, 64)), [1, 2]);
        assert!(hub.total_bytes() > 0);

        let viewer = CancellationToken::new();
        hub.add_viewer("s1", "sub-1", 7, viewer.clone());
        assert_eq!(hub.viewer_count("s1"), 1);
        assert!(hub.viewed_by("s1", 7));
        assert!(!hub.viewed_by("s1", 8));

        hub.release_session("s1");
        assert!(viewer.is_cancelled(), "release ends the session's viewers");
        assert_eq!(hub.viewer_count("s1"), 0);
        assert_eq!(hub.total_bytes(), 0, "the ring's bytes are returned");
        assert_ne!(
            hub.session_source("s1"),
            source,
            "a session recreated under the same id starts a fresh ring"
        );
    }

    #[test]
    fn a_route_lasts_as_long_as_its_guard() {
        let hub = Arc::new(small_hub(64));
        assert_eq!(hub.routed_source("s1"), None);
        let route = hub.route_session("s1");
        assert_eq!(hub.routed_source("s1"), Some(hub.session_source("s1")));
        drop(route);
        assert_eq!(hub.routed_source("s1"), None);
    }

    #[tokio::test]
    async fn viewers_gone_resolves_when_the_last_viewer_leaves() {
        let hub = Arc::new(small_hub(64));
        hub.viewers_gone("s1").await; // none attached: immediate
        hub.add_viewer("s1", "a", 1, CancellationToken::new());
        hub.add_viewer("s1", "b", 2, CancellationToken::new());
        let waiter = {
            let hub = Arc::clone(&hub);
            zeroclaw_spawn::spawn!(async move { hub.viewers_gone("s1").await })
        };
        hub.remove_viewer("s1", "a");
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished(), "one viewer is still attached");
        hub.remove_viewer("s1", "b");
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter)
            .await
            .expect("the last viewer leaving wakes the waiter")
            .unwrap();
    }

    #[tokio::test]
    async fn the_bus_pump_splits_sources_and_never_stores_credentials() {
        let hub = Arc::new(small_hub(64));
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        hub.attach_bus(&tx);
        hub.attach_bus(&tx); // idempotent: one pump

        tx.send(json!({
            "source": "observability",
            "attributes": { "login": { "qr_payload": "SECRET" } },
            zeroclaw_log::EPHEMERAL_BROADCAST_MARKER: true,
        }))
        .unwrap();
        tx.send(json!({"message": "log line"})).unwrap();
        tx.send(json!({"source": "observability", "type": "tool_call"}))
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while hub.head_seq(Source::Logs) < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let Read::Frames(logs) = hub.read(Source::Logs, 1, 64) else {
            panic!("logs ring must be contiguous");
        };
        let Read::Frames(events) = hub.read(Source::Events, 1, 64) else {
            panic!("events ring must be contiguous");
        };
        assert_eq!(
            logs.len(),
            2,
            "credential frame must not be stored: {logs:?}"
        );
        assert!(!format!("{logs:?}").contains("SECRET"));
        assert_eq!(events.len(), 1, "only the observer frame: {events:?}");
        assert_eq!(events[0].1["type"], "tool_call");
    }
}
