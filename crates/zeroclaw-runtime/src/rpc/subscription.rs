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
//!
//! Besides the fixed daemon streams, each session can have its own ring
//! ([`SubscriptionHub::session_source`]). A session-lifetime turn publishes
//! its `session/update` frames there, and every attached viewer reads them
//! through a cursor like any other subscriber, so a turn never waits on a
//! viewer and a viewer that reconnects resumes with its epoch and `since_seq`.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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
    /// The turns of its own connection it carries (see [`OwnTurns`]).
    own: Arc<OwnTurns>,
}

/// Per-session bookkeeping: the ring handle, its viewers, whether a turn is
/// currently delivering through it, and which session incarnation it holds
/// frames for.
struct SessionEntry {
    id: u64,
    viewers: HashMap<String, Viewer>,
    routed: usize,
    /// The incarnation the ring holds frames for, as given by the caller
    /// that created it. `None` only for a ring created without one, which
    /// the first identified caller adopts. A ring is never handed to a
    /// different identity: that caller retires it and gets a fresh ring, so
    /// a session recreated under a reused id cannot read the old frames.
    identity: Option<RingIdentity>,
}

/// Which session incarnation a ring's frames belong to, and to whom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RingIdentity {
    /// Names the incarnation: its storage domain and durable row, or its
    /// live generation, together with its owner.
    pub incarnation: String,
    /// The principal that owns the incarnation, and so every frame the ring
    /// holds; `None` for an unowned session.
    pub owner: Option<String>,
}

/// One turn's span of a session ring: from its first frame to the ring's
/// head when the turn ended. Open while the turn runs.
pub struct TurnScope {
    first_seq: u64,
    /// The ring's head when the turn ended, or [`Self::OPEN`] while it runs.
    last_seq: AtomicU64,
    closed: Notify,
}

impl TurnScope {
    /// No sequence number reaches it, so it cannot be mistaken for a real
    /// bound. A turn that ends on an empty ring closes at 0, which bounds
    /// its span to nothing at all.
    const OPEN: u64 = u64::MAX;

    /// A turn starting on a ring whose newest frame is `head`.
    #[must_use]
    pub fn after(head: u64) -> Self {
        Self {
            first_seq: head + 1,
            last_seq: AtomicU64::new(Self::OPEN),
            closed: Notify::new(),
        }
    }

    /// The ring's head when the turn ended, once it has.
    #[must_use]
    pub fn last_seq(&self) -> Option<u64> {
        match self.last_seq.load(Ordering::Acquire) {
            Self::OPEN => None,
            seq => Some(seq),
        }
    }

    /// Whether frame `seq` was published during this turn.
    #[must_use]
    pub fn covers(&self, seq: u64) -> bool {
        seq >= self.first_seq && self.last_seq().is_none_or(|last| seq <= last)
    }

    /// End the scope at `seq`, the turn's last frame.
    pub fn close_at(&self, seq: u64) {
        self.last_seq.store(seq, Ordering::Release);
        self.closed.notify_waiters();
    }

    /// Resolves when the scope closes. Enable it before reading
    /// [`Self::last_seq`] so a close in between is not missed.
    pub fn closed(&self) -> tokio::sync::futures::Notified<'_> {
        self.closed.notified()
    }
}

/// The turns of its own connection a session viewer carries. Their frames
/// are that connection's own prompt output, which the prompt's authority
/// (sessions:execute) entitles it to whether or not it may also read the
/// whole session. A connection's turns ride its existing viewer of the ring
/// when it has one (see [`SubscriptionHub::carry_turn`]), so each frame
/// reaches the connection once and in order.
#[derive(Default)]
pub struct OwnTurns {
    scopes: Mutex<Vec<Arc<TurnScope>>>,
}

impl OwnTurns {
    /// Carrying one turn.
    #[must_use]
    pub fn of(scope: Arc<TurnScope>) -> Self {
        Self {
            scopes: Mutex::new(vec![scope]),
        }
    }

    /// Whether frame `seq` belongs to one of the carried turns.
    #[must_use]
    pub fn covers(&self, seq: u64) -> bool {
        self.scopes.lock().iter().any(|scope| scope.covers(seq))
    }

    /// The carried turn still running, if any. Turns of one session run one
    /// at a time, so there is at most one.
    #[must_use]
    pub fn running(&self) -> Option<Arc<TurnScope>> {
        self.scopes
            .lock()
            .iter()
            .find(|scope| scope.last_seq().is_none())
            .cloned()
    }

    /// Stop carrying the turn frame `seq` belongs to: the connection may no
    /// longer see that turn.
    pub fn drop_turn_of(&self, seq: u64) {
        self.scopes.lock().retain(|scope| !scope.covers(seq));
    }

    /// Forget the carried turns that ended before `cursor`.
    pub fn forget_before(&self, cursor: u64) {
        self.scopes
            .lock()
            .retain(|scope| scope.last_seq().is_none_or(|last| last >= cursor));
    }

    fn push(&self, scope: Arc<TurnScope>) {
        self.scopes.lock().push(scope);
    }

    /// Whether every carried turn has ended and `cursor` is past its last
    /// frame.
    fn delivered_before(&self, cursor: u64) -> bool {
        self.scopes
            .lock()
            .iter()
            .all(|scope| scope.last_seq().is_some_and(|last| cursor > last))
    }
}

/// How a session viewer's frame fared at its commit.
#[derive(Debug, PartialEq, Eq)]
pub enum Commit {
    /// Handed to the connection's writer.
    Sent,
    /// Not the viewer's to see. The viewer is detached.
    Refused,
    /// The ring was retired, or the viewer detached, before the commit.
    Gone,
}

#[derive(Default)]
struct Sessions {
    by_id: HashMap<String, SessionEntry>,
    next_id: u64,
}

/// The process's subscription sources. Cheap to share behind an `Arc`.
pub struct SubscriptionHub {
    epoch: String,
    state: Mutex<HubState>,
    notify: Mutex<HashMap<Source, Arc<Notify>>>,
    sessions: Mutex<Sessions>,
    /// Signalled whenever a session's viewer set changes.
    viewers_changed: Notify,
    session_ring: RingLimits,
    byte_budget: usize,
    bus_attached: AtomicBool,
    /// Test-only park point in delivery, after a writer slot is reserved and
    /// before the disclosure is rechecked. Fires once.
    #[cfg(test)]
    test_delivery_pause: Mutex<Option<DeliveryPause>>,
    #[cfg(test)]
    test_delivery_facts_pause: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

/// Handles for the test-only delivery park point. `slot_held_at_pause` is
/// recorded by the delivery path when it parks, so a test can assert the park
/// came after the writer slot was won.
#[cfg(test)]
#[derive(Clone)]
pub(crate) struct DeliveryPause {
    pub(crate) entered: Arc<Notify>,
    pub(crate) release: Arc<Notify>,
    pub(crate) slot_held_at_pause: Arc<AtomicBool>,
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
            #[cfg(test)]
            test_delivery_pause: Mutex::new(None),
            #[cfg(test)]
            test_delivery_facts_pause: Mutex::new(None),
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
        // A session ring exists from its first frame; before that the next
        // sequence number is 1.
        state.rings.get(&source).map_or(1, |ring| {
            ring.entries
                .front()
                .map_or(ring.next_seq, |entry| entry.seq)
        })
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
            Some(_) => {
                // Stop at the first gap inside the batch (a recorded loss) so
                // the next read reports it as `Lagged` instead of the cursor
                // jumping over it.
                let frames = ring
                    .entries
                    .range(first..)
                    .take(max)
                    .zip(cursor..)
                    .take_while(|(entry, expected)| entry.seq == *expected)
                    .map(|(entry, _)| (entry.seq, Arc::clone(&entry.frame)))
                    .collect();
                Read::Frames(frames)
            }
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

    #[cfg(test)]
    pub(crate) fn set_test_delivery_pause(&self) -> DeliveryPause {
        let pause = DeliveryPause {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            slot_held_at_pause: Arc::new(AtomicBool::new(false)),
        };
        *self.test_delivery_pause.lock() = Some(pause.clone());
        pause
    }

    /// Park delivery once, if a test armed it. `slot_held` is whether the
    /// caller holds its writer slot at this point.
    #[cfg(test)]
    pub(crate) async fn wait_test_delivery_pause(&self, slot_held: bool) {
        let Some(pause) = self.test_delivery_pause.lock().take() else {
            return;
        };
        pause.slot_held_at_pause.store(slot_held, Ordering::SeqCst);
        pause.entered.notify_one();
        pause.release.notified().await;
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) async fn wait_test_delivery_pause(&self, _slot_held: bool) {}

    /// A second test-only park point in delivery: after the session facts
    /// are read and before the disclosure is decided. Fires once.
    #[cfg(test)]
    pub(crate) fn set_test_delivery_facts_pause(&self) -> (Arc<Notify>, Arc<Notify>) {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *self.test_delivery_facts_pause.lock() = Some((Arc::clone(&entered), Arc::clone(&release)));
        (entered, release)
    }

    #[cfg(test)]
    pub(crate) async fn wait_test_delivery_facts_pause(&self) {
        let Some((entered, release)) = self.test_delivery_facts_pause.lock().take() else {
            return;
        };
        entered.notify_one();
        release.notified().await;
    }

    #[cfg(not(test))]
    #[inline(always)]
    pub(crate) async fn wait_test_delivery_facts_pause(&self) {}

    /// The ring for `session_id`, created on first use without an identity.
    /// Production callers use [`Self::session_source_for`]; this form serves
    /// callers that only publish into a ring that already exists.
    pub fn session_source(&self, session_id: &str) -> Source {
        let mut sessions = self.sessions.lock();
        if let Some(entry) = sessions.by_id.get(session_id) {
            return Source::Session(entry.id);
        }
        Self::create_session_entry(&mut sessions, session_id, None)
    }

    /// The ring holding frames for the session incarnation `identity` names
    /// under `session_id`, created on first use. A ring kept for a different
    /// incarnation under the same id (a session deleted, or replaced by
    /// another owner, without its ring being released) is retired first: its
    /// viewers end and its frames are dropped, so they can never be replayed
    /// to the new session.
    pub fn session_source_for(&self, session_id: &str, identity: &RingIdentity) -> Source {
        let mut sessions = self.sessions.lock();
        if let Some(entry) = sessions.by_id.get_mut(session_id) {
            match entry.identity.as_ref() {
                Some(current) if current == identity => return Source::Session(entry.id),
                None => {
                    entry.identity = Some(identity.clone());
                    return Source::Session(entry.id);
                }
                Some(_) => {}
            }
        }
        let retired = sessions.by_id.remove(session_id);
        let source = Self::create_session_entry(&mut sessions, session_id, Some(identity));
        drop(sessions);
        if let Some(entry) = retired {
            self.retire_session_entry(entry);
        }
        source
    }

    fn create_session_entry(
        sessions: &mut Sessions,
        session_id: &str,
        identity: Option<&RingIdentity>,
    ) -> Source {
        sessions.next_id += 1;
        let id = sessions.next_id;
        sessions.by_id.insert(
            session_id.to_string(),
            SessionEntry {
                id,
                viewers: HashMap::new(),
                routed: 0,
                identity: identity.cloned(),
            },
        );
        Source::Session(id)
    }

    /// Record a viewer of `session_id`'s ring `source` whose delivery ends
    /// when `cancel` fires. `connection` identifies the attaching
    /// connection; `own` holds the turns of that connection it carries.
    /// Returns `false`, recording nothing, when `source` is no longer the
    /// session's ring.
    pub fn add_viewer(
        &self,
        session_id: &str,
        source: Source,
        subscription_id: &str,
        connection: u64,
        cancel: CancellationToken,
        own: Arc<OwnTurns>,
    ) -> bool {
        let mut sessions = self.sessions.lock();
        let Some(entry) = sessions
            .by_id
            .get_mut(session_id)
            .filter(|entry| Source::Session(entry.id) == source)
        else {
            return false;
        };
        entry.viewers.insert(
            subscription_id.to_string(),
            Viewer {
                connection,
                cancel,
                own,
            },
        );
        drop(sessions);
        self.viewers_changed.notify_waiters();
        true
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

    /// Hand `connection`'s new turn `scope` to every viewer that connection
    /// already has on `source`, still `session_id`'s ring. Returns whether
    /// there was one; if not, the caller gives the turn a viewer of its own.
    ///
    /// This and [`Self::retire_viewer_if_done`] run under the same lock, so a
    /// viewer either takes the turn before it decides it is done, and then
    /// is not done, or is gone before the turn looks for it.
    pub fn carry_turn(
        &self,
        session_id: &str,
        source: Source,
        connection: u64,
        scope: &Arc<TurnScope>,
    ) -> bool {
        let sessions = self.sessions.lock();
        let Some(entry) = sessions
            .by_id
            .get(session_id)
            .filter(|entry| Source::Session(entry.id) == source)
        else {
            return false;
        };
        let mut carried = false;
        for viewer in entry.viewers.values() {
            if viewer.connection == connection && !viewer.cancel.is_cancelled() {
                viewer.own.push(Arc::clone(scope));
                carried = true;
            }
        }
        carried
    }

    /// Detach a viewer that carries only its connection's own turns once it
    /// has delivered all of them up to `cursor`. Returns `true` when the
    /// viewer is gone (detached now, or already), so its delivery ends.
    pub fn retire_viewer_if_done(
        &self,
        session_id: &str,
        source: Source,
        subscription_id: &str,
        cursor: u64,
    ) -> bool {
        let mut sessions = self.sessions.lock();
        let Some(entry) = sessions
            .by_id
            .get_mut(session_id)
            .filter(|entry| Source::Session(entry.id) == source)
        else {
            return true;
        };
        match entry.viewers.get(subscription_id) {
            None => return true,
            Some(viewer) if !viewer.own.delivered_before(cursor) => return false,
            Some(_) => {}
        }
        entry.viewers.remove(subscription_id);
        drop(sessions);
        self.viewers_changed.notify_waiters();
        true
    }

    /// Commit one of a viewer's frames: `send` runs only while `source` is
    /// still `session_id`'s ring and the viewer is attached to it, and only
    /// if `permitted` accepts the ring's identity, the owner of every frame
    /// it holds. Retiring a ring takes the same lock, so a frame is either
    /// committed before its ring is retired, to a viewer permitted to see
    /// that ring's frames, or not at all. A refusal detaches the viewer
    /// under the same lock.
    pub fn commit_viewer_frame(
        &self,
        session_id: &str,
        source: Source,
        subscription_id: &str,
        permitted: impl FnOnce(Option<&RingIdentity>) -> bool,
        send: impl FnOnce(),
    ) -> Commit {
        let mut sessions = self.sessions.lock();
        let Some(entry) = sessions.by_id.get_mut(session_id).filter(|entry| {
            Source::Session(entry.id) == source && entry.viewers.contains_key(subscription_id)
        }) else {
            return Commit::Gone;
        };
        if permitted(entry.identity.as_ref()) {
            send();
            return Commit::Sent;
        }
        entry.viewers.remove(subscription_id);
        drop(sessions);
        self.viewers_changed.notify_waiters();
        Commit::Refused
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

    /// Deliver `session_id`'s turn notifications through `source`, its ring,
    /// until the returned guard drops. When the guard drops it closes `scope`,
    /// if given, at the ring's newest sequence number: the turn's last frame.
    pub fn route_session(
        self: &Arc<Self>,
        session_id: &str,
        source: Source,
        scope: Option<Arc<TurnScope>>,
    ) -> SessionRoute {
        if let Some(entry) = self
            .sessions
            .lock()
            .by_id
            .get_mut(session_id)
            .filter(|entry| Source::Session(entry.id) == source)
        {
            entry.routed += 1;
        }
        SessionRoute {
            hub: Arc::clone(self),
            session_id: session_id.to_string(),
            source,
            scope,
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
        self.retire_session_entry(entry);
    }

    /// End a removed ring's viewers and drop its frames.
    fn retire_session_entry(&self, entry: SessionEntry) {
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
    source: Source,
    scope: Option<Arc<TurnScope>>,
}

impl Drop for SessionRoute {
    fn drop(&mut self) {
        if let Some(entry) = self
            .hub
            .sessions
            .lock()
            .by_id
            .get_mut(&self.session_id)
            .filter(|entry| Source::Session(entry.id) == self.source)
        {
            entry.routed = entry.routed.saturating_sub(1);
        }
        if let Some(scope) = self.scope.take() {
            scope.close_at(self.hub.head_seq(self.source));
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
        // A full-size batch (the production READ_BATCH) stops before the gap
        // rather than returning [1, 5] and letting the cursor jump to 6.
        assert_eq!(frames(hub.read(Source::Logs, 1, READ_BATCH)), [1]);
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
        assert!(hub.add_viewer("s1", source, "sub-1", 7, viewer.clone(), Arc::default()));
        assert_eq!(hub.viewer_count("s1"), 1);

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
        let route = hub.route_session("s1", hub.session_source("s1"), None);
        assert_eq!(hub.routed_source("s1"), Some(hub.session_source("s1")));
        drop(route);
        assert_eq!(hub.routed_source("s1"), None);
    }

    fn identity(incarnation: &str, owner: &str) -> RingIdentity {
        RingIdentity {
            incarnation: incarnation.to_string(),
            owner: Some(owner.to_string()),
        }
    }

    #[test]
    fn a_ring_is_never_handed_to_a_different_session_incarnation() {
        let hub = Arc::new(small_hub(64));
        let bobs = identity("bob-incarnation", "user:bob");
        let bob = hub.session_source_for("s1", &bobs);
        hub.publish(bob, json!({"text": "bob's private frame"}));
        let viewer = CancellationToken::new();
        assert!(hub.add_viewer("s1", bob, "sub-bob", 7, viewer.clone(), Arc::default()));
        assert_eq!(
            hub.session_source_for("s1", &bobs),
            bob,
            "the same incarnation keeps its ring and its replay"
        );

        let alice = hub.session_source_for("s1", &identity("alice-incarnation", "user:alice"));
        assert_ne!(alice, bob, "a reused id gets a fresh ring");
        assert!(
            frames(hub.read(alice, 1, 64)).is_empty(),
            "nothing of the previous incarnation is readable from the new ring"
        );
        assert!(
            viewer.is_cancelled(),
            "the previous incarnation's viewers end"
        );
        assert_eq!(
            hub.total_bytes(),
            0,
            "the retired ring's bytes are returned"
        );
        assert!(
            !hub.add_viewer(
                "s1",
                bob,
                "late",
                7,
                CancellationToken::new(),
                Arc::default()
            ),
            "a viewer cannot attach to a retired ring"
        );
    }

    #[test]
    fn a_ring_created_without_an_identity_is_adopted_not_retired() {
        let hub = Arc::new(small_hub(64));
        let source = hub.session_source("s1");
        hub.publish(source, json!({"text": "published before identification"}));
        assert_eq!(
            hub.session_source_for("s1", &identity("the-incarnation", "user:alice")),
            source
        );
        assert_eq!(frames(hub.read(source, 1, 64)), vec![1]);
    }

    // A frame a viewer has already read is committed under the lock that
    // retires rings, and judged against the ring's own identity. These pin
    // the order: whichever of commit and retirement takes the lock first
    // decides, and no later owner of the id is ever consulted.

    #[test]
    fn a_frame_is_not_committed_once_its_ring_is_retired() {
        let hub = Arc::new(small_hub(64));
        let alices = identity("alice-incarnation", "user:alice");
        let source = hub.session_source_for("s1", &alices);
        assert!(hub.add_viewer(
            "s1",
            source,
            "v",
            7,
            CancellationToken::new(),
            Arc::default()
        ));

        hub.release_session("s1");
        // The id is reused before the frame, read earlier, is committed.
        let reused = hub.session_source_for("s1", &identity("bob-incarnation", "user:bob"));
        assert!(hub.add_viewer(
            "s1",
            reused,
            "v2",
            7,
            CancellationToken::new(),
            Arc::default()
        ));
        let mut sent = false;
        let outcome = hub.commit_viewer_frame("s1", source, "v", |_| true, || sent = true);
        assert_eq!(outcome, Commit::Gone);
        assert!(!sent, "a retired ring's frame is never handed to a writer");
    }

    #[test]
    fn a_frame_is_judged_against_its_own_rings_owner() {
        let hub = Arc::new(small_hub(64));
        let alices = identity("alice-incarnation", "user:alice");
        let source = hub.session_source_for("s1", &alices);
        assert!(hub.add_viewer(
            "s1",
            source,
            "v",
            7,
            CancellationToken::new(),
            Arc::default()
        ));

        let mut judged = None;
        let mut sent = false;
        let outcome = hub.commit_viewer_frame(
            "s1",
            source,
            "v",
            |ring| {
                judged = ring.cloned();
                ring.is_some_and(|ring| ring.owner.as_deref() == Some("user:bob"))
            },
            || sent = true,
        );
        assert_eq!(judged, Some(alices), "the ring's identity, not the id's");
        assert_eq!(outcome, Commit::Refused);
        assert!(!sent);
        assert_eq!(hub.viewer_count("s1"), 0, "a refusal detaches the viewer");

        let mut sent = false;
        assert_eq!(
            hub.commit_viewer_frame("s1", source, "v", |_| true, || sent = true),
            Commit::Gone,
            "a detached viewer commits nothing more"
        );
        assert!(!sent);
    }

    #[test]
    fn a_turn_is_carried_by_a_viewer_that_is_not_yet_done() {
        let hub = Arc::new(small_hub(64));
        let source = hub.session_source_for("s1", &identity("i", "user:alice"));
        let first = Arc::new(TurnScope::after(0));
        let own = Arc::new(OwnTurns::of(Arc::clone(&first)));
        assert!(hub.add_viewer(
            "s1",
            source,
            "t",
            7,
            CancellationToken::new(),
            Arc::clone(&own)
        ));
        hub.publish(source, json!({"n": 1}));
        first.close_at(1);

        // The next turn is carried before the viewer checks whether it is
        // done: it is not done, and it carries the new turn's frames.
        let second = Arc::new(TurnScope::after(1));
        assert!(hub.carry_turn("s1", source, 7, &second));
        assert!(
            !hub.carry_turn("s1", source, 8, &second),
            "only its own connection's"
        );
        assert!(!hub.retire_viewer_if_done("s1", source, "t", 2));
        hub.publish(source, json!({"n": 2}));
        assert!(own.covers(2));

        // Once it is done it is gone, and a later turn finds no viewer to
        // ride: the caller gives that turn a viewer of its own.
        second.close_at(2);
        assert!(hub.retire_viewer_if_done("s1", source, "t", 3));
        assert!(!hub.carry_turn("s1", source, 7, &Arc::new(TurnScope::after(2))));
        assert_eq!(hub.viewer_count("s1"), 0);
    }

    #[test]
    fn own_turns_cover_exactly_their_frames() {
        let one = Arc::new(TurnScope::after(2));
        let own = OwnTurns::of(Arc::clone(&one));
        assert!(!own.covers(2), "before the turn");
        assert!(own.covers(3) && own.covers(99), "open while the turn runs");
        one.close_at(4);
        assert!(own.covers(4) && !own.covers(5), "closed at its last frame");
        assert!(own.running().is_none());
        assert!(!own.delivered_before(4) && own.delivered_before(5));
        own.drop_turn_of(3);
        assert!(!own.covers(3), "a dropped turn is no longer carried");
        assert!(own.delivered_before(0), "nothing left to deliver");
    }

    #[test]
    fn a_turn_scope_closes_at_the_last_frame_when_its_route_drops() {
        let hub = Arc::new(small_hub(64));
        let source = hub.session_source_for("s1", &identity("i", "user:alice"));
        let scope = Arc::new(TurnScope::after(hub.head_seq(source)));
        let route = hub.route_session("s1", source, Some(Arc::clone(&scope)));
        hub.publish(source, json!({"text": "one"}));
        hub.publish(source, json!({"text": "two"}));
        assert_eq!(scope.last_seq(), None, "open while the turn runs");
        drop(route);
        assert_eq!(scope.last_seq(), Some(2), "closed at the turn's last frame");
        assert!(scope.covers(1) && scope.covers(2) && !scope.covers(3));
    }

    #[test]
    fn a_turn_that_published_nothing_on_an_empty_ring_bounds_its_viewer_to_nothing() {
        let hub = Arc::new(small_hub(64));
        let source = hub.session_source_for("s1", &identity("i", "user:alice"));
        let scope = Arc::new(TurnScope::after(0));
        drop(hub.route_session("s1", source, Some(Arc::clone(&scope))));
        assert_eq!(
            scope.last_seq(),
            Some(0),
            "the bound is the empty head, not the next turn's first frame"
        );
        assert!(!scope.covers(1));
    }

    #[tokio::test]
    async fn viewers_gone_resolves_when_the_last_viewer_leaves() {
        let hub = Arc::new(small_hub(64));
        hub.viewers_gone("s1").await; // none attached: immediate
        let source = hub.session_source("s1");
        hub.add_viewer(
            "s1",
            source,
            "a",
            1,
            CancellationToken::new(),
            Arc::default(),
        );
        hub.add_viewer(
            "s1",
            source,
            "b",
            2,
            CancellationToken::new(),
            Arc::default(),
        );
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
