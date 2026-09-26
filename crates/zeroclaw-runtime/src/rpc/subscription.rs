//! Bounded, replayable subscription sources for RPC streams.
//!
//! A [`SubscriptionHub`] holds one ring per [`Source`]. Producers append to a
//! ring and never wait for a subscriber; each subscriber is only a cursor (the
//! next sequence number it wants). Every ring is bounded by a frame count and a
//! byte cap, and all rings share one process-wide byte budget: when it is
//! exceeded, the oldest frame across all rings is evicted.
//!
//! Nothing is dropped silently. A cursor that points into a gap, whether the
//! frames were evicted or never reached the hub, reads [`Read::Lagged`] with
//! the first missing sequence and the first one still available, and the
//! subscriber resumes from there.
//!
//! Sequence numbers start at 1 per source and are never reused within a hub.
//! Each hub has a random [`SubscriptionHub::epoch`]; a new hub (a daemon
//! restart or reload) starts numbering again, so a client resumes with the
//! epoch and `since_seq` it last saw, and a different epoch is reported as a
//! break in continuity rather than matched by number.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;

/// A subscribable stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    /// Every frame on the daemon event bus (`logs/subscribe`).
    Logs,
    /// Observer frames only: agent, tool, LLM, history-trim, error
    /// (`events/subscribe`).
    Events,
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

/// The process's subscription sources. Cheap to share behind an `Arc`.
pub struct SubscriptionHub {
    epoch: String,
    state: Mutex<HubState>,
    notify: HashMap<Source, Notify>,
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
            epoch: uuid::Uuid::new_v4().to_string(),
            state: Mutex::new(HubState {
                rings: Source::ALL
                    .into_iter()
                    .map(|source| (source, Ring::new(ring)))
                    .collect(),
                total_bytes: 0,
                next_arrival: 0,
            }),
            notify: Source::ALL
                .into_iter()
                .map(|source| (source, Notify::new()))
                .collect(),
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
            let ring = state
                .rings
                .get_mut(&source)
                .expect("every source has a ring");
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
        self.notify[&source].notify_waiters();
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
        self.state
            .lock()
            .rings
            .get_mut(&source)
            .expect("every source has a ring")
            .next_seq += count;
        self.notify[&source].notify_waiters();
    }

    /// This hub's identity. Sequence numbers are only comparable within one
    /// epoch.
    #[must_use]
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The sequence number of the oldest frame still buffered, or the next
    /// sequence number when the ring is empty.
    #[must_use]
    pub fn oldest_seq(&self, source: Source) -> u64 {
        let state = self.state.lock();
        let ring = &state.rings[&source];
        ring.entries
            .front()
            .map_or(ring.next_seq, |entry| entry.seq)
    }

    /// The sequence number of the newest frame (0 before the first).
    #[must_use]
    pub fn head_seq(&self, source: Source) -> u64 {
        self.state.lock().rings[&source].next_seq - 1
    }

    /// Up to `max` frames at or after `cursor`, or the gap the cursor is in.
    #[must_use]
    pub fn read(&self, source: Source, cursor: u64, max: usize) -> Read {
        let state = self.state.lock();
        let ring = &state.rings[&source];
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
    pub fn notifier(&self, source: Source) -> &Notify {
        &self.notify[&source]
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
    fn epochs_differ_per_hub_and_oldest_tracks_eviction() {
        let first = small_hub(2);
        let second = small_hub(2);
        assert_ne!(first.epoch(), second.epoch());
        assert_eq!(first.oldest_seq(Source::Logs), 1);
        for n in 1..=5 {
            first.publish(Source::Logs, json!({ "n": n }));
        }
        assert_eq!(first.oldest_seq(Source::Logs), 4);
        assert_eq!(first.oldest_seq(Source::Events), 1);
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
