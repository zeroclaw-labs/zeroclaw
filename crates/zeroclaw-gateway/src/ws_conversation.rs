//! Shared WebSocket conversations (#376).
//!
//! Every `/ws/chat` socket that opens the same session key attaches to one
//! [`Conversation`]: one agent, one in-memory history, one approval map, one
//! running turn at a time. Sockets are subscribers. A turn runs in its own
//! task and publishes its frames to every subscriber, so:
//!
//! - two sockets on one session see the same ordered history and events;
//! - a message sent while a turn is running steers that turn instead of
//!   starting a second one;
//! - an approval raised during a turn can be answered from any subscriber,
//!   and only the first answer counts;
//! - closing a socket only unsubscribes. The turn keeps running; it stops
//!   only on an explicit cancel (a `cancel` frame or the REST abort route).
//!
//! The hub is generic over the agent state so its bookkeeping is testable
//! without building a real agent.

use crate::ws_approval::{PendingApprovals, new_pending_approvals};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use tokio::sync::{OnceCell, broadcast, mpsc};
use tokio_util::sync::CancellationToken;
use zeroclaw_api::channel::ChannelApprovalResponse;
use zeroclaw_infra::session_backend::RequestReceipt;

/// A serialized JSON frame and its in-process history revision. The revision
/// never enters the wire format; it invalidates unread frames after deletion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    text: Arc<str>,
    reset_revision: u64,
}

impl Frame {
    pub(crate) fn is_current(&self, revision: u64) -> bool {
        self.reset_revision == revision
    }
}

impl std::ops::Deref for Frame {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for Frame {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.text)
    }
}

/// Frames buffered per conversation for slow subscribers. A subscriber that
/// falls further behind skips frames (logged); the `done` frame still carries
/// the full response.
const FRAME_BUFFER: usize = 1024;

/// Steering messages queued for a running turn.
const STEERING_BUFFER: usize = 32;

/// Client request ids a conversation remembers when the session store keeps
/// no receipts. Mirrors the SQLite backend's per-session bound.
const RECENT_REQUESTS: usize = 256;

const RECENT_SESSIONS: usize = 1024;
const RECEIPT_TTL: std::time::Duration = std::time::Duration::from_secs(
    zeroclaw_infra::session_backend::REQUEST_RECEIPT_PROTECTION_MINUTES as u64 * 60,
);

/// Recently accepted client request ids and their last state, in arrival
/// order. The in-memory stand-in for the session store's receipts: it
/// catches a retry across conversation recreation, not one after a restart.
#[derive(Default)]
struct RecentRequests {
    entries: std::collections::VecDeque<(String, String, std::time::Instant)>,
}

impl RecentRequests {
    fn record(&mut self, request_id: &str, state: &str) -> std::io::Result<RequestReceipt> {
        if let Some((_, known, _)) = self.entries.iter().find(|(id, _, _)| id == request_id) {
            return Ok(RequestReceipt::Duplicate {
                state: known.clone(),
            });
        }
        if self.entries.len() == RECENT_REQUESTS {
            return Err(std::io::Error::other("recent request capacity exhausted"));
        }
        self.entries.push_back((
            request_id.to_string(),
            state.to_string(),
            std::time::Instant::now(),
        ));
        Ok(RequestReceipt::Recorded)
    }

    fn set_state(&mut self, request_id: &str, state: &str) {
        if let Some((_, known, _)) = self.entries.iter_mut().find(|(id, _, _)| id == request_id) {
            *known = state.to_string();
        }
    }
}

/// Publishes frames to every subscriber of one conversation.
#[derive(Clone)]
pub(crate) struct FrameSink {
    frames: broadcast::Sender<Frame>,
    subscribers: Arc<AtomicUsize>,
    reset_revision: Arc<AtomicU64>,
}

impl FrameSink {
    pub(crate) fn publish(&self, frame: &serde_json::Value) {
        self.publish_at_revision(frame, self.reset_revision.load(Ordering::Acquire));
    }

    fn publish_at_revision(&self, frame: &serde_json::Value, reset_revision: u64) {
        let _ = self.frames.send(Frame {
            text: Arc::from(frame.to_string()),
            reset_revision,
        });
    }

    /// Whether any socket is currently subscribed.
    pub(crate) fn has_subscribers(&self) -> bool {
        self.subscribers.load(Ordering::Acquire) > 0
    }
}

/// What the agent factory gets to wire the agent into its conversation.
pub(crate) struct Seed {
    pub(crate) pending_approvals: PendingApprovals,
    pub(crate) questions: Arc<super::ws_question::Questions>,
    pub(crate) frames: FrameSink,
}

/// The turn currently running in a conversation.
struct ActiveTurn {
    correction_invalidated: Arc<AtomicBool>,
    generation: u64,
    id: String,
    cancel: CancellationToken,
    steering: mpsc::Sender<String>,
}

/// What happened to a message submitted to a conversation.
pub(crate) enum Submitted {
    /// No turn was running; the caller must run this one.
    Start(Box<TurnClaim>),
    /// A turn is running and the message was queued as steering for it.
    Steered,
    /// A turn is running and its steering queue is full.
    SteeringFull,
    /// A turn is running but no longer accepts steering.
    SteeringClosed,
}

/// The right to run one turn. Created only by [`Conversation::submit`].
pub(crate) struct TurnClaim {
    /// The message that starts the turn.
    pub(crate) input: String,
    /// Original socket text before attachment expansion. This is evidence,
    /// never an authority grant; derived/bridge claims leave it absent.
    pub(crate) original_input: Option<String>,
    /// The client's id for that message, if it sent one.
    pub(crate) request_id: Option<String>,
    /// Reference to the canonical durable bridge input, when this is one.
    pub(crate) intake: Option<crate::ws::intake::Claim>,
    pub(crate) surface: Option<zeroclaw_api::chat_surface::ChatSurface>,
    pub(crate) generation: u64,
    /// Session deletion invalidates admissions that have not run yet.
    pub(crate) reset_revision: u64,
    pub(crate) turn_id: String,
    pub(crate) cancel: CancellationToken,
    pub(crate) steering: mpsc::Receiver<String>,
}

pub(crate) trait ConversationAgent {
    fn side_query_handle(&self) -> Option<zeroclaw_runtime::agent::SideQueryHandle> {
        None
    }
}

impl ConversationAgent for () {}

pub(crate) struct Conversation<A> {
    key: String,
    /// The agent state. A turn holds this lock for its whole duration.
    pub(crate) agent: tokio::sync::Mutex<A>,
    /// Lock-free read-only snapshot handle for one-shot `/btw` side queries.
    pub(crate) side_query: Option<zeroclaw_runtime::agent::SideQueryHandle>,
    side_query_cancel: parking_lot::Mutex<CancellationToken>,
    active_side_queries: AtomicUsize,
    frames: FrameSink,
    control_frames: broadcast::Sender<Frame>,
    pub(crate) pending_approvals: PendingApprovals,
    pub(crate) questions: Arc<super::ws_question::Questions>,
    turn: parking_lot::Mutex<Option<ActiveTurn>>,
    next_generation: AtomicU64,
    turn_revision: AtomicU64,
    controls_enabled: AtomicBool,
}

impl<A: ConversationAgent> Conversation<A> {
    fn new(key: String, agent: A, seed: Seed) -> Self {
        let Seed {
            pending_approvals,
            questions,
            frames,
        } = seed;
        let side_query = agent.side_query_handle();
        Self {
            key,
            agent: tokio::sync::Mutex::new(agent),
            side_query,
            side_query_cancel: parking_lot::Mutex::new(CancellationToken::new()),
            active_side_queries: AtomicUsize::new(0),
            frames,
            control_frames: broadcast::channel(64).0,
            pending_approvals,
            questions,
            turn: parking_lot::Mutex::new(None),
            next_generation: AtomicU64::new(1),
            turn_revision: AtomicU64::new(0),
            controls_enabled: AtomicBool::new(false),
        }
    }
}

impl<A> Conversation<A> {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) fn active_turn_id(&self) -> Option<String> {
        self.turn.lock().as_ref().map(|t| t.id.clone())
    }

    pub(crate) fn reset_revision(&self) -> u64 {
        self.frames.reset_revision.load(Ordering::Acquire)
    }

    pub(crate) fn side_query_generation(&self) -> (u64, CancellationToken) {
        let cancel = self.side_query_cancel.lock();
        (self.reset_revision(), cancel.clone())
    }

    /// Called with the session lifecycle and execution guards held, after
    /// clearing the Agent. Old ACK writers/queued claims cannot revive a turn.
    pub(crate) fn reset(&self) {
        let mut turn = self.turn.lock();
        if let Some(active) = turn.as_ref() {
            active.cancel.cancel();
        }
        *turn = None;
        // Publishers hold their pending-map guard through publication. Revoke
        // both maps before advancing the revision so an already-running relay
        // cannot stamp a revoked prompt with the new history revision.
        self.drain_approvals();
        self.questions.drain();
        {
            let mut cancel = self.side_query_cancel.lock();
            cancel.cancel();
            self.frames.reset_revision.fetch_add(1, Ordering::AcqRel);
            *cancel = CancellationToken::new();
        }
        self.publish_turn_state(&turn);
    }

    #[cfg(test)]
    pub(crate) fn frame_sink(&self) -> FrameSink {
        self.frames.clone()
    }

    /// Send a frame to every current subscriber.
    pub(crate) fn publish(&self, frame: &serde_json::Value) {
        self.frames.publish(frame);
    }

    pub(crate) fn publish_side_query(&self, frame: &serde_json::Value, reset_revision: u64) {
        // Even when completion races cancellation, subscribers see the
        // revision at admission and discard a result from deleted history.
        self.frames.publish_at_revision(frame, reset_revision);
    }

    /// Extension traffic has a separate bounded ring, so opting in cannot
    /// reduce the legacy subscriber's existing frame retention.
    pub(crate) fn publish_control(&self, frame: &serde_json::Value) {
        let _ = self.control_frames.send(Frame {
            text: Arc::from(frame.to_string()),
            reset_revision: self.reset_revision(),
        });
    }

    /// Start a turn if none is running, otherwise steer the running one.
    /// On [`Submitted::Start`] the claim carries `content` back as the turn's
    /// input; otherwise it went to the running turn or was dropped.
    pub(crate) fn submit(&self, content: String) -> Submitted {
        let mut turn = self.turn.lock();
        if let Some(active) = turn.as_ref() {
            return match active.steering.try_reserve() {
                Ok(permit) => {
                    // Reserve first: only accepted steering invalidates authority,
                    // and invalidation must precede publication to the receiver.
                    active.correction_invalidated.store(true, Ordering::Release);
                    permit.send(content);
                    Submitted::Steered
                }
                Err(mpsc::error::TrySendError::Full(_)) => Submitted::SteeringFull,
                Err(mpsc::error::TrySendError::Closed(_)) => Submitted::SteeringClosed,
            };
        }
        self.start_locked(&mut turn, content)
    }

    /// Reserve an idle turn without putting durable inputs in the ephemeral
    /// steering queue. Their session journal remains the owner while busy.
    pub(crate) fn start_if_idle(&self, content: String) -> Option<TurnClaim> {
        let mut turn = self.turn.lock();
        if turn.is_some() {
            return None;
        }
        match self.start_locked(&mut turn, content) {
            Submitted::Start(claim) => Some(*claim),
            _ => unreachable!("an idle slot always starts a turn"),
        }
    }

    /// Explicit steering can address only the observed live turn. Check and
    /// reserve under the same lock as start/finish/cancel, so an old control
    /// can never be delivered to a successor turn.
    pub(crate) fn steer_turn(&self, target: &str, content: String) -> &'static str {
        let turn = self.turn.lock();
        let Some(active) = turn.as_ref() else {
            return "no_active";
        };
        if active.id != target {
            return "stale";
        }
        if active.cancel.is_cancelled() {
            return "cancelling";
        }
        match active.steering.try_reserve() {
            Ok(permit) => {
                // A mid-turn correction cannot inherit the initial owner's
                // evidence grant, including when delivered from another socket.
                active.correction_invalidated.store(true, Ordering::Release);
                permit.send(content);
                "accepted"
            }
            Err(mpsc::error::TrySendError::Full(_)) => "full",
            Err(mpsc::error::TrySendError::Closed(_)) => "closed",
        }
    }

    fn start_locked(&self, turn: &mut Option<ActiveTurn>, content: String) -> Submitted {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let turn_id = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        let (steering_tx, steering_rx) = mpsc::channel(STEERING_BUFFER);
        *turn = Some(ActiveTurn {
            correction_invalidated: Arc::new(AtomicBool::new(false)),
            generation,
            id: turn_id.clone(),
            cancel: cancel.clone(),
            steering: steering_tx,
        });
        self.publish_turn_state(turn);
        Submitted::Start(Box::new(TurnClaim {
            input: content,
            original_input: None,
            request_id: None,
            intake: None,
            surface: None,
            generation,
            reset_revision: self.reset_revision(),
            turn_id,
            cancel,
            steering: steering_rx,
        }))
    }

    /// A successful steering submission invalidates the initial ingress as
    /// correction evidence for this generation, regardless of socket identity.
    pub(crate) fn correction_invalidation(&self, generation: u64) -> Option<Arc<AtomicBool>> {
        self.turn
            .lock()
            .as_ref()
            .filter(|turn| turn.generation == generation)
            .map(|turn| Arc::clone(&turn.correction_invalidated))
    }

    /// Release the turn slot, but only for the turn that holds it.
    pub(crate) fn finish_turn(&self, generation: u64) {
        let mut turn = self.turn.lock();
        if turn.as_ref().is_some_and(|t| t.generation == generation) {
            self.questions.drain();
            *turn = None;
            self.publish_turn_state(&turn);
        }
    }

    /// Cancel the running turn. Returns `false` when none is running.
    pub(crate) fn cancel_current(&self) -> bool {
        match self.turn.lock().as_ref() {
            Some(active) => {
                active.cancel.cancel();
                self.drain_approvals();
                self.questions.drain();
                true
            }
            None => false,
        }
    }

    /// Opt in to turn control while taking a snapshot under the same owner lock
    /// as admission and completion. Revisions order buffered reconnect events.
    pub(crate) fn enable_turn_controls(&self) -> serde_json::Value {
        let turn = self.turn.lock();
        self.controls_enabled.store(true, Ordering::Relaxed);
        self.turn_state_frame(&turn)
    }

    fn turn_state_frame(&self, turn: &Option<ActiveTurn>) -> serde_json::Value {
        serde_json::json!({
            "type":"turn_state", "control_version":1,
            "revision":self.turn_revision.load(Ordering::Relaxed),
            "turn_id":turn.as_ref().map(|turn| turn.id.as_str()),
        })
    }

    fn publish_turn_state(&self, turn: &Option<ActiveTurn>) {
        self.turn_revision.fetch_add(1, Ordering::Relaxed);
        if self.controls_enabled.load(Ordering::Relaxed) {
            self.publish(&self.turn_state_frame(turn));
        }
    }

    /// Compare and cancel under the admission lock. A stale UUID can never
    /// signal the token of a later turn, including after conversation recreation.
    pub(crate) fn cancel_turn(&self, target: &str) -> &'static str {
        let turn = self.turn.lock();
        match turn.as_ref() {
            None => "no_active",
            Some(active) if active.id != target => "stale",
            Some(active) => {
                active.cancel.cancel();
                self.drain_approvals();
                self.questions.drain();
                "requested"
            }
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.turn.lock().is_some()
    }

    /// Deliver an operator decision. Only the first answer for a request
    /// counts; later or unknown ones return `false`.
    pub(crate) fn resolve_approval(
        &self,
        request_id: &str,
        decision: ChannelApprovalResponse,
    ) -> bool {
        match self.pending_approvals.lock().remove(request_id) {
            Some(entry) => entry.resolve(decision),
            None => false,
        }
    }

    /// Drop every pending approval so a waiting turn resolves as unreachable
    /// instead of waiting for its timeout.
    pub(crate) fn drain_approvals(&self) {
        let drained: Vec<_> = self.pending_approvals.lock().drain().collect();
        drop(drained);
    }

    pub(crate) fn approval_frames(
        &self,
        leak_detection: &zeroclaw_config::schema::LeakDetectionConfig,
    ) -> Vec<Frame> {
        let pending = self.pending_approvals.lock();
        let revision = self.reset_revision();
        pending
            .iter()
            .filter_map(|(id, entry)| {
                let remaining = entry.remaining_secs()?;
                let frame = crate::ws::approval_request_ws_frame(
                    id,
                    &entry.tool_name,
                    &entry.arguments_summary,
                    remaining,
                    leak_detection,
                );
                Some(Frame {
                    text: Arc::from(frame.to_string()),
                    reset_revision: revision,
                })
            })
            .collect()
    }

    pub(crate) fn subscriber_count(&self) -> usize {
        self.frames.subscribers.load(Ordering::Acquire)
    }
}

type Slot<A> = Arc<OnceCell<Arc<Conversation<A>>>>;

/// Live conversations keyed by session key.
pub struct ConversationHub<A> {
    // Detached turns retain the hub, so releasing the listener alone cannot
    // allow a replacement gateway to settle work that is still executing.
    _session_owner: Option<zeroclaw_infra::gateway_session_owner::GatewaySessionOwner>,
    /// Bounded process-epoch request/turn replay; execution stays in Conversation.
    pub(crate) turn_control: crate::ws::turn_control::Store,
    /// Canonical bounded ephemeral payload bytes for this gateway.
    pub(crate) attachments: crate::api_attachments::Store,
    /// Scheduling ownership in this process, not a copy of durable input state.
    pub(crate) intake: crate::ws::intake::Scheduling,
    slots: parking_lot::Mutex<HashMap<String, Slot<A>>>,
    // Short-lived synchronization only: the session backend still owns durable
    // history and receipts, and Conversation still owns the live Agent/turn.
    lifecycles: parking_lot::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    // Canonical fallback receipts outlive idle conversations. No second copy
    // in Conversation and no agent/history retention for reconnect dedup.
    requests: parking_lot::Mutex<HashMap<String, RecentRequests>>,
}

impl<A> Default for ConversationHub<A> {
    fn default() -> Self {
        Self {
            _session_owner: None,
            slots: parking_lot::Mutex::new(HashMap::new()),
            lifecycles: parking_lot::Mutex::default(),
            attachments: crate::api_attachments::Store::default(),
            turn_control: crate::ws::turn_control::Store::default(),
            intake: crate::ws::intake::Scheduling::default(),
            requests: parking_lot::Mutex::default(),
        }
    }
}

impl<A> ConversationHub<A> {
    pub(crate) fn owned(data_dir: &std::path::Path) -> std::io::Result<Self> {
        let owner = zeroclaw_infra::gateway_session_owner::GatewaySessionOwner::acquire(data_dir)?;
        if owner.recovered_turns() > 0 {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(serde_json::json!({"recovered_turns": owner.recovered_turns()})),
                "Marked abandoned gateway turns as failed; external effects remain unknown and are not replayed"
            );
        }
        Ok(Self {
            _session_owner: Some(owner),
            ..Self::default()
        })
    }

    /// Serialize same-session hydration/admission with deletion, independently
    /// of the long-running execution queue. Never hold this guard while waiting
    /// for socket input/output. Weak entries retain no disconnected sessions.
    pub(crate) fn session_lifecycle(&self, session: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut lifecycles = self.lifecycles.lock();
        if let Some(lifecycle) = lifecycles.get(session).and_then(Weak::upgrade) {
            return lifecycle;
        }
        lifecycles.retain(|_, lifecycle| lifecycle.strong_count() > 0);
        let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
        lifecycles.insert(session.to_string(), Arc::downgrade(&lifecycle));
        lifecycle
    }

    /// Snapshot while holding the session lifecycle guard: concurrent factories
    /// cannot add a conversation seeded with pre-deletion history.
    pub(crate) fn session_conversations(&self, session: &str) -> Vec<Arc<Conversation<A>>> {
        self.slots
            .lock()
            .iter()
            .filter(|(key, _)| {
                key.rsplit_once('\u{1f}')
                    .is_some_and(|(key, _)| key == session)
            })
            .filter_map(|(_, slot)| slot.get().cloned())
            .collect()
    }

    pub(crate) fn record_request(
        &self,
        key: &str,
        id: &str,
        state: &str,
    ) -> std::io::Result<RequestReceipt> {
        let now = std::time::Instant::now();
        let mut requests = self.requests.lock();
        requests.retain(|_, receipts| {
            receipts
                .entries
                .retain(|(_, _, recorded)| now.duration_since(*recorded) < RECEIPT_TTL);
            !receipts.entries.is_empty()
        });
        if requests.len() >= RECENT_SESSIONS && !requests.contains_key(key) {
            return Err(std::io::Error::other(
                "recent request session capacity exhausted",
            ));
        }
        requests
            .entry(key.to_string())
            .or_default()
            .record(id, state)
    }

    pub(crate) fn set_request_state(&self, key: &str, id: &str, state: &str) {
        if let Some(receipts) = self.requests.lock().get_mut(key) {
            receipts.set_state(id, state);
        }
    }

    /// Attach to the conversation for `key`, building it with `create` if no
    /// socket holds it. `create` returns the agent state plus a value for
    /// the caller; the caller gets that value back only when this call built
    /// the conversation. A failed `create` leaves no entry behind.
    pub(crate) async fn attach<F, Fut, X, E>(
        self: &Arc<Self>,
        key: &str,
        mut create: F,
    ) -> Result<(Subscription<A>, Option<X>), E>
    where
        A: ConversationAgent,
        F: FnMut(Seed) -> Fut,
        Fut: std::future::Future<Output = Result<(A, X), E>>,
    {
        loop {
            let slot = Arc::clone(
                self.slots
                    .lock()
                    .entry(key.to_string())
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            );
            let mut built = None;
            let result = slot
                .get_or_try_init(|| async {
                    let pending_approvals = new_pending_approvals();
                    let questions = Arc::new(super::ws_question::Questions::default());
                    let (frames, _) = broadcast::channel(FRAME_BUFFER);
                    let frames = FrameSink {
                        frames,
                        subscribers: Arc::new(AtomicUsize::new(0)),
                        reset_revision: Arc::new(AtomicU64::new(0)),
                    };
                    let (agent, extra) = create(Seed {
                        pending_approvals: pending_approvals.clone(),
                        questions: questions.clone(),
                        frames: frames.clone(),
                    })
                    .await?;
                    built = Some(extra);
                    Ok(Arc::new(Conversation::new(
                        key.to_string(),
                        agent,
                        Seed {
                            pending_approvals,
                            questions,
                            frames,
                        },
                    )))
                })
                .await;
            let conversation = match result {
                Ok(conversation) => Arc::clone(conversation),
                Err(err) => {
                    self.remove_empty_slot(key, &slot);
                    return Err(err);
                }
            };

            // Count the subscriber under the map lock, and only while the map
            // still points at this conversation: an idle conversation can be
            // released between the build above and here. A vacant key takes
            // it back; a newer conversation under the key wins, and this
            // attach joins that one instead.
            let mut slots = self.slots.lock();
            match slots.get(key) {
                Some(held) if Arc::ptr_eq(held, &slot) => {}
                Some(_) => continue,
                None => {
                    slots.insert(key.to_string(), Arc::clone(&slot));
                }
            }
            conversation
                .frames
                .subscribers
                .fetch_add(1, Ordering::AcqRel);
            let frames = conversation.frames.frames.subscribe();
            let control_frames = conversation.control_frames.subscribe();
            drop(slots);
            return Ok((
                Subscription {
                    hub: Arc::clone(self),
                    conversation,
                    frames,
                    control_frames,
                },
                built,
            ));
        }
    }

    /// Keep a running side query visible to session deletion even if its last
    /// socket disconnects. The caller still holds its subscription here.
    pub(crate) fn retain_for_side_query(
        self: &Arc<Self>,
        conversation: &Arc<Conversation<A>>,
    ) -> SideQueryRetention<A> {
        conversation
            .active_side_queries
            .fetch_add(1, Ordering::AcqRel);
        SideQueryRetention {
            hub: Arc::clone(self),
            conversation: Arc::clone(conversation),
        }
    }

    /// Forget `conversation` once no socket or running task needs it.
    pub(crate) fn release_if_unused(&self, conversation: &Arc<Conversation<A>>) {
        let mut slots = self.slots.lock();
        if conversation.subscriber_count() > 0
            || conversation.is_running()
            || conversation.active_side_queries.load(Ordering::Acquire) > 0
        {
            return;
        }
        let same = slots
            .get(conversation.key())
            .and_then(|slot| slot.get())
            .is_some_and(|held| Arc::ptr_eq(held, conversation));
        if same {
            slots.remove(conversation.key());
        }
    }

    /// Drop a slot whose build failed. An attach still waiting on it builds
    /// in it and puts it back (see [`Self::attach`]).
    fn remove_empty_slot(&self, key: &str, slot: &Slot<A>) {
        let mut slots = self.slots.lock();
        if slot.get().is_none() && slots.get(key).is_some_and(|held| Arc::ptr_eq(held, slot)) {
            slots.remove(key);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.lock().len()
    }
}

pub(crate) struct SideQueryRetention<A> {
    hub: Arc<ConversationHub<A>>,
    conversation: Arc<Conversation<A>>,
}

impl<A> Drop for SideQueryRetention<A> {
    fn drop(&mut self) {
        self.conversation
            .active_side_queries
            .fetch_sub(1, Ordering::AcqRel);
        self.hub.release_if_unused(&self.conversation);
    }
}

/// One socket's attachment to a conversation. Dropping it unsubscribes.
pub(crate) struct Subscription<A> {
    hub: Arc<ConversationHub<A>>,
    pub(crate) conversation: Arc<Conversation<A>>,
    pub(crate) frames: broadcast::Receiver<Frame>,
    pub(crate) control_frames: broadcast::Receiver<Frame>,
}

impl<A> Drop for Subscription<A> {
    fn drop(&mut self) {
        let remaining = self
            .conversation
            .frames
            .subscribers
            .fetch_sub(1, Ordering::AcqRel)
            - 1;
        if remaining == 0 {
            // Tool approvals retain their original deadline for reconnect.
            // Questions keep their existing offline behavior.
            self.conversation
                .questions
                .drain_if_offline(&self.conversation.frames);
        }
        self.hub.release_if_unused(&self.conversation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn attach(hub: &Arc<ConversationHub<()>>, key: &str) -> (Subscription<()>, bool) {
        let (sub, built) = hub
            .attach(key, |_| async { Ok::<_, ()>(((), ())) })
            .await
            .unwrap();
        (sub, built.is_some())
    }

    fn pending_approval(
        sender: tokio::sync::oneshot::Sender<ChannelApprovalResponse>,
    ) -> crate::ws_approval::PendingApproval {
        crate::ws_approval::PendingApproval::new(
            sender,
            &zeroclaw_api::channel::ChannelApprovalRequest {
                tool_name: "file_write".into(),
                arguments_summary: "path=test.txt".into(),
                raw_arguments: None,
            },
            std::time::Duration::from_secs(30),
        )
    }

    #[tokio::test]
    async fn sockets_on_one_key_share_a_conversation_and_its_frames() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (mut a, created_a) = attach(&hub, "gw_s1").await;
        let (mut b, created_b) = attach(&hub, "gw_s1").await;
        let (other, _) = attach(&hub, "gw_s2").await;
        assert!(created_a && !created_b);
        assert!(Arc::ptr_eq(&a.conversation, &b.conversation));
        assert!(!Arc::ptr_eq(&a.conversation, &other.conversation));

        a.conversation
            .publish(&serde_json::json!({"type": "chunk", "content": "hi"}));
        for sub in [&mut a, &mut b] {
            let frame = sub.frames.recv().await.unwrap();
            assert_eq!(&*frame, r#"{"content":"hi","type":"chunk"}"#);
        }
    }

    #[tokio::test]
    async fn reset_invalidates_buffered_frames_but_keeps_new_output_and_other_sessions() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (mut sub, _) = attach(&hub, "gw_deleted").await;
        let (mut other, _) = attach(&hub, "gw_other").await;
        sub.conversation
            .publish(&serde_json::json!({"type":"chunk", "content":"old"}));
        other
            .conversation
            .publish(&serde_json::json!({"type":"chunk", "content":"retained"}));
        sub.conversation.reset();
        sub.conversation
            .publish(&serde_json::json!({"type":"chunk", "content":"new"}));
        assert!(
            !sub.frames
                .recv()
                .await
                .unwrap()
                .is_current(sub.conversation.reset_revision())
        );
        let fresh = sub.frames.recv().await.unwrap();
        assert!(fresh.is_current(sub.conversation.reset_revision()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fresh).unwrap()["content"],
            "new"
        );
        assert!(
            other
                .frames
                .recv()
                .await
                .unwrap()
                .is_current(other.conversation.reset_revision())
        );
    }

    #[tokio::test]
    async fn reset_on_retained_conversation_rejects_late_side_query_but_accepts_new_generation() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let key = "gw_retained\u{1f}web";
        let (mut sub, _) = attach(&hub, key).await;
        let conversation = Arc::clone(&sub.conversation);
        let (old_revision, old_cancel) = conversation.side_query_generation();
        let retention = hub.retain_for_side_query(&conversation);
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        let (publish_tx, publish_rx) = tokio::sync::oneshot::channel();
        let publisher = Arc::clone(&conversation);
        let publisher_cancel = old_cancel.clone();
        let old_query = zeroclaw_spawn::spawn!(async move {
            let _retention = retention;
            // Model a provider result that already passed its cancellation check.
            // Hold publication until after reset: cancellation alone cannot
            // retract this result, so its admission revision must protect it.
            assert!(!publisher_cancel.is_cancelled());
            completed_tx.send(()).unwrap();
            publish_rx.await.unwrap();
            publisher.publish_side_query(
                &serde_json::json!({"type": "btw_response", "id": "old", "content": "deleted history"}),
                old_revision,
            );
        });
        let limit = std::time::Duration::from_secs(5);
        tokio::time::timeout(limit, completed_rx)
            .await
            .expect("old query must finish before reset")
            .unwrap();

        // Keep the original subscriber and Conversation alive through reset.
        // The real DELETE path is covered by the SQLite-backed test in ws.rs;
        // this test pins its completion-versus-publication interleaving.
        let lifecycle = hub.session_lifecycle("gw_retained");
        {
            let _lifecycle = lifecycle.lock().await;
            let _agent = conversation.agent.lock().await;
            conversation.reset();
        }
        assert!(
            old_cancel.is_cancelled(),
            "reset must cancel the old generation"
        );
        let (new_revision, new_cancel) = conversation.side_query_generation();
        assert_ne!(new_revision, old_revision);
        assert!(
            !new_cancel.is_cancelled(),
            "the retained conversation must admit new queries"
        );
        let (reattached, created) = attach(&hub, key).await;
        assert!(
            !created,
            "a fresh query must not depend on rebuilding the conversation"
        );
        assert!(Arc::ptr_eq(&conversation, &reattached.conversation));

        publish_tx.send(()).unwrap();
        tokio::time::timeout(limit, old_query)
            .await
            .expect("old result must reach publication after reset")
            .unwrap();
        conversation.publish_side_query(
            &serde_json::json!({"type": "btw_response", "id": "fresh", "content": "new answer"}),
            new_revision,
        );

        // Consume actual broadcast Frames through the same revision predicate
        // used by the socket reader. This does not model already-sent bytes.
        let stale = tokio::time::timeout(limit, sub.frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!stale.is_current(conversation.reset_revision()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&stale).unwrap()["id"],
            "old"
        );
        let fresh = tokio::time::timeout(limit, sub.frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(fresh.is_current(conversation.reset_revision()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&fresh).unwrap()["id"],
            "fresh"
        );
        assert!(!new_cancel.is_cancelled());
    }

    #[tokio::test]
    async fn reset_waits_for_an_approval_publisher_before_advancing_frame_revision() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (mut sub, _) = attach(&hub, "gw_deleted").await;
        let conversation = Arc::clone(&sub.conversation);
        let Submitted::Start(claim) = conversation.submit("old turn".into()) else {
            panic!("idle conversation must start a turn")
        };
        let revision = conversation.reset_revision();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (reset, observed_revision) = {
            // Model the relay after it found the request but before it published.
            let mut pending = conversation.pending_approvals.lock();
            pending.insert("old-approval".into(), pending_approval(tx));
            let resetting = Arc::clone(&conversation);
            let reset = std::thread::spawn(move || resetting.reset());
            // Cancellation proves reset has entered its turn-locked section; the
            // held map guard prevents it from completing revocation.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !claim.cancel.is_cancelled() {
                assert!(std::time::Instant::now() < deadline, "reset did not enter");
                std::thread::yield_now();
            }
            let observed_revision = conversation.reset_revision();
            conversation.publish(&serde_json::json!({
                "type": "approval_request", "request_id": "old-approval",
                "arguments_summary": "deleted private prompt"
            }));
            (reset, observed_revision)
        };
        reset.join().unwrap();
        assert_eq!(observed_revision, revision);
        assert_eq!(conversation.reset_revision(), revision + 1);
        assert!(rx.await.is_err(), "reset must revoke the old approval");
        assert!(conversation.pending_approvals.lock().is_empty());
        assert!(
            !sub.frames
                .recv()
                .await
                .unwrap()
                .is_current(conversation.reset_revision()),
            "a publisher already holding the old map guard cannot survive reset"
        );
    }

    #[tokio::test]
    async fn a_message_during_a_turn_steers_it_instead_of_starting_another() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let conv = &a.conversation;
        let Submitted::Start(mut claim) = conv.submit("first".into()) else {
            panic!("idle conversation must start a turn")
        };
        assert!(matches!(conv.submit("more".into()), Submitted::Steered));
        assert_eq!(claim.steering.recv().await.as_deref(), Some("more"));

        conv.finish_turn(claim.generation);
        let Submitted::Start(next) = conv.submit("second".into()) else {
            panic!("finished turn must free the slot")
        };
        assert!(next.generation > claim.generation);
    }

    #[tokio::test]
    async fn an_old_generation_cannot_release_or_cancel_a_newer_turn() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let conv = &a.conversation;
        let Submitted::Start(old) = conv.submit("one".into()) else {
            panic!()
        };
        conv.finish_turn(old.generation);
        let Submitted::Start(new) = conv.submit("two".into()) else {
            panic!()
        };
        conv.finish_turn(old.generation);
        assert!(conv.is_running(), "stale finish must not free the new turn");
        assert!(!old.cancel.is_cancelled());
        assert!(conv.cancel_current());
        assert!(new.cancel.is_cancelled());
        assert!(!old.cancel.is_cancelled());
        conv.finish_turn(new.generation);
        assert!(!conv.cancel_current());
    }

    #[tokio::test]
    async fn explicit_steering_is_scoped_bounded_and_cancel_dominates() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (subscription, _) = attach(&hub, "gw_steering").await;
        let conversation = &subscription.conversation;
        let mut old = conversation.start_if_idle("first".into()).unwrap();
        let evidence = conversation
            .correction_invalidation(old.generation)
            .unwrap();
        assert_eq!(
            conversation.steer_turn("wrong-turn", "wrong".into()),
            "stale"
        );
        assert!(!evidence.load(Ordering::Acquire));
        assert_eq!(
            conversation.steer_turn(&old.turn_id, "accepted".into()),
            "accepted"
        );
        assert!(
            evidence.load(Ordering::Acquire),
            "steering must revoke the initial ingress evidence"
        );
        assert_eq!(old.steering.try_recv().unwrap(), "accepted");
        for _ in 0..STEERING_BUFFER {
            assert_eq!(
                conversation.steer_turn(&old.turn_id, "bounded".into()),
                "accepted"
            );
        }
        assert_eq!(
            conversation.steer_turn(&old.turn_id, "overflow".into()),
            "full"
        );
        assert_eq!(conversation.cancel_turn(&old.turn_id), "requested");
        assert_eq!(
            conversation.steer_turn(&old.turn_id, "after cancel".into()),
            "cancelling"
        );
        conversation.finish_turn(old.generation);
        let mut next = conversation.start_if_idle("next".into()).unwrap();
        assert_eq!(
            conversation.steer_turn(&old.turn_id, "late".into()),
            "stale"
        );
        assert!(next.steering.try_recv().is_err());
        next.steering.close();
        assert_eq!(
            conversation.steer_turn(&next.turn_id, "closed".into()),
            "closed"
        );
        conversation.finish_turn(next.generation);
        assert_eq!(
            conversation.steer_turn(&next.turn_id, "terminal".into()),
            "no_active"
        );
    }

    #[tokio::test]
    async fn cancellation_revokes_detached_approval_before_any_new_decision() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (sub, _) = attach(&hub, "gw_cancel").await;
        let conversation = sub.conversation.clone();
        let Submitted::Start(claim) = conversation.submit("go".into()) else {
            panic!()
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        conversation
            .pending_approvals
            .lock()
            .insert("cancel-me".into(), pending_approval(tx));
        drop(sub);
        assert_eq!(conversation.cancel_turn(&claim.turn_id), "requested");
        assert!(rx.await.is_err());
        assert!(conversation.approval_frames(&Default::default()).is_empty());
        assert!(!conversation.resolve_approval("cancel-me", ChannelApprovalResponse::Approve));
    }

    #[tokio::test]
    async fn an_approval_resolves_once_from_any_subscriber() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let (b, _) = attach(&hub, "gw_s1").await;
        let (tx, rx) = tokio::sync::oneshot::channel();
        a.conversation
            .pending_approvals
            .lock()
            .insert("req-1".into(), pending_approval(tx));
        assert!(
            b.conversation
                .resolve_approval("req-1", ChannelApprovalResponse::Approve)
        );
        assert!(
            !a.conversation
                .resolve_approval("req-1", ChannelApprovalResponse::Deny)
        );
        assert!(
            !a.conversation
                .resolve_approval("unknown", ChannelApprovalResponse::Deny)
        );
        assert!(matches!(rx.await, Ok(ChannelApprovalResponse::Approve)));
    }

    #[tokio::test]
    async fn closing_the_last_socket_keeps_the_turn_and_bounded_approval() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let conv = Arc::clone(&a.conversation);
        let Submitted::Start(claim) = conv.submit("go".into()) else {
            panic!()
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        conv.pending_approvals
            .lock()
            .insert("req-1".into(), pending_approval(tx));

        drop(a);
        assert!(!claim.cancel.is_cancelled(), "detach must not cancel");
        assert_eq!(
            conv.pending_approvals.lock().len(),
            1,
            "detach does not revoke a live prompt"
        );
        assert_eq!(hub.len(), 1, "a running turn keeps its conversation");

        // A socket reconnecting mid-turn finds the same conversation.
        let (again, created) = attach(&hub, "gw_s1").await;
        assert!(!created && Arc::ptr_eq(&again.conversation, &conv));
        assert_eq!(
            again
                .conversation
                .approval_frames(&Default::default())
                .len(),
            1
        );
        assert!(
            again
                .conversation
                .resolve_approval("req-1", ChannelApprovalResponse::Approve)
        );
        assert!(matches!(rx.await, Ok(ChannelApprovalResponse::Approve)));
        drop(again);

        conv.finish_turn(claim.generation);
        hub.release_if_unused(&conv);
        assert_eq!(hub.len(), 0);
    }

    #[tokio::test]
    async fn idle_conversation_is_released_when_its_last_socket_closes() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let (b, _) = attach(&hub, "gw_s1").await;
        drop(a);
        assert_eq!(hub.len(), 1);
        drop(b);
        assert_eq!(hub.len(), 0);
    }

    #[tokio::test]
    async fn failed_creation_leaves_no_entry_and_can_be_retried() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let err = hub
            .attach("gw_s1", |_| async { Err::<((), ()), _>("boom") })
            .await
            .err();
        assert_eq!(err, Some("boom"));
        assert_eq!(hub.len(), 0);
        let (_a, created) = attach(&hub, "gw_s1").await;
        assert!(created);
    }

    #[tokio::test]
    async fn a_failed_build_keeps_the_slot_for_an_attach_waiting_on_it() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<()>();
        let mut fail_rx = Some(fail_rx);
        let failing = hub.attach("gw_s1", |_| {
            let rx = fail_rx.take();
            async move {
                if let Some(rx) = rx {
                    let _ = rx.await;
                }
                Err::<((), ()), _>("boom")
            }
        });
        let waiting = async {
            tokio::task::yield_now().await;
            let _ = fail_tx.send(());
            hub.attach("gw_s1", |_| async { Ok::<_, &str>(((), ())) })
                .await
        };
        let (failed, joined) = tokio::join!(failing, waiting);
        assert!(failed.is_err());
        let (b, built) = joined.unwrap();
        assert!(built.is_some());

        // The waiting attach's conversation is the one the key maps to.
        let (c, created) = attach(&hub, "gw_s1").await;
        assert!(!created && Arc::ptr_eq(&b.conversation, &c.conversation));
        assert_eq!(hub.len(), 1);
    }

    #[tokio::test]
    async fn remembered_requests_report_duplicates_and_stay_bounded() {
        let hub = Arc::new(ConversationHub::<()>::default());
        let (a, _) = attach(&hub, "gw_s1").await;
        let key = a.conversation.key();
        assert_eq!(
            hub.record_request(key, "r1", "accepted").unwrap(),
            RequestReceipt::Recorded
        );
        hub.set_request_state(key, "r1", "done");
        assert_eq!(
            hub.record_request(key, "r1", "accepted").unwrap(),
            RequestReceipt::Duplicate {
                state: "done".into()
            }
        );
        for i in 2..=RECENT_REQUESTS {
            hub.record_request(key, &format!("r{i}"), "accepted")
                .unwrap();
        }
        assert!(hub.record_request(key, "overflow", "accepted").is_err());
        assert_eq!(
            hub.record_request(key, "r1", "accepted").unwrap(),
            RequestReceipt::Duplicate {
                state: "done".into()
            }
        );
    }

    #[test]
    fn receipt_expiry_and_session_capacity_do_not_evict_live_ids() {
        let hub = ConversationHub::<()>::default();
        for i in 0..RECENT_SESSIONS {
            hub.record_request(&format!("s{i}"), "r", "accepted")
                .unwrap();
        }
        assert!(hub.record_request("overflow", "r", "accepted").is_err());
        assert!(matches!(
            hub.record_request("s0", "r", "accepted").unwrap(),
            RequestReceipt::Duplicate { .. }
        ));
        hub.requests.lock().get_mut("s0").unwrap().entries[0].2 =
            std::time::Instant::now() - RECEIPT_TTL;
        // Updating terminal state must not renew the receipt's expiry.
        hub.set_request_state("s0", "r", "done");
        assert_eq!(
            hub.record_request("overflow", "r", "accepted").unwrap(),
            RequestReceipt::Recorded
        );
        assert!(!hub.requests.lock().contains_key("s0"));
    }
}
