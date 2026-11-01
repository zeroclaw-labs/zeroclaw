//! Session coordinator and bound effect ports.
//!
//! The coordinator is the sole writer visible to runtime adapters.  It owns
//! admission and per-stream serialization, while the event store owns only
//! physical durability.  It intentionally has no provider/tool/channel
//! implementation dependencies.

mod approvals;
#[cfg(test)]
mod boundaries_tests;
mod branches;
pub mod capture;
mod children;
mod context;
mod context_messages;
mod context_summary;
pub use context::ContextMaintenanceCommand;
mod authority;
mod controls;
mod delivery;
pub mod engine;
mod erasure;
pub mod execution;
pub mod model_selection;
mod provider_cancellation;
pub mod query;
pub mod recovery;
mod requeue;
mod retry;
pub mod tool_capture;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{Semaphore, broadcast};
use zeroclaw_api::session_events::{
    AppendError, AppendReceipt, AppendRequest, BranchId, EventActor, EventCursor, EventId,
    EventKind, InvocationId, PayloadRecord, ReadError, SessionEvent, SessionEventStore,
    SessionStreamId, TurnId,
};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("invalid effect admission: {0}")]
    InvalidAdmission(String),
    #[error("effect observation handle is already terminal")]
    TerminalHandle,
    #[error("effect observation does not belong to its bound invocation")]
    InvocationMismatch,
    #[error("session queue is full")]
    QueueFull,
    #[error("session operation timed out: {0}")]
    Timeout(String),
    #[error("session target is stale: {0}")]
    StaleTarget(String),
    #[error("session store is unhealthy: {0}")]
    StoreUnhealthy(String),
    #[error("session operation failed: {0}")]
    Operation(#[source] anyhow::Error),
}

/// The current authority resolver is injected for each admission.  A history
/// event can record the decision evidence, but it never becomes a future
/// permission grant.
pub trait PermissionResolver: Send + Sync {
    fn child_authority(
        &self,
        _target: &str,
        _stream: &SessionStreamId,
    ) -> Result<Arc<dyn engine::EngineSessionAuthority>, SessionError> {
        Err(SessionError::PermissionDenied(
            "child scope is not authorized".into(),
        ))
    }
    fn authorize(&self, intent: &EffectIntent) -> Result<(), SessionError>;
    fn authorize_context(
        &self,
        _stream: &SessionStreamId,
        _branch: &BranchId,
    ) -> Result<(), SessionError> {
        Err(SessionError::PermissionDenied(
            "context mutation is not authorized".into(),
        ))
    }
    /// Historical evidence for the current admission check, not an authority
    /// snapshot. Production resolvers read policy and revision under one guard.
    fn tool_authority_evidence(
        &self,
        _stream: &SessionStreamId,
        _name: &str,
    ) -> Result<serde_json::Value, SessionError> {
        Ok(serde_json::json!({"source":"embedding_permission_resolver","revision":"not_reported"}))
    }
    /// Resolve current tool policy against the committed final action. The
    /// arguments are borrowed observations, never a stored authorization grant.
    fn authorize_tool(
        &self,
        intent: &EffectIntent,
        _name: &str,
        _arguments: &serde_json::Value,
    ) -> Result<(), SessionError> {
        self.authorize(intent)
    }
}

#[derive(Clone, Debug)]
pub struct EffectIntent {
    pub stream_id: SessionStreamId,
    pub expected_head: EventCursor,
    pub idempotency_key: String,
    pub event: SessionEvent,
    pub payloads: Vec<zeroclaw_api::session_events::PayloadRecord>,
}

impl EffectIntent {
    pub fn validate(&self) -> Result<(), SessionError> {
        if self.event.stream_id != self.stream_id {
            return Err(SessionError::InvalidAdmission(
                "effect event belongs to another stream".to_owned(),
            ));
        }
        if self.event.invocation_id.is_none() {
            return Err(SessionError::InvalidAdmission(
                "effect admission requires invocation_id".to_owned(),
            ));
        }
        if !matches!(
            self.event.body.kind(),
            EventKind::ProviderAttemptStarted
                | EventKind::ToolEffectStarted
                | EventKind::DeliveryStarted
                | EventKind::ChildInvocationRequested
        ) {
            return Err(SessionError::InvalidAdmission(
                "effect admission requires a prepared or started invocation event".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A coordinator owns the durable writer and only ephemeral serialization
/// guards.  The guard map is not a second history or status source.
pub struct SessionCoordinator {
    store: Arc<dyn SessionEventStore>,
    search: Option<Arc<dyn zeroclaw_api::session_events::search::SessionSearchPort>>,
    stream_locks: Mutex<HashMap<SessionStreamId, std::sync::Weak<Mutex<()>>>>,
    workers: Arc<Semaphore>,
    wakeups: broadcast::Sender<SessionStreamId>,
    foreground: Mutex<HashMap<SessionStreamId, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    /// Creates the identity of this execution owner, independent of turn IDs.
    /// Reopening a coordinator never reuses a historical approval incarnation.
    execution_incarnation: String,
}

impl SessionCoordinator {
    /// Resolve the stop condition from committed history at each effect
    /// admission. An adapter swallowing an Unknown error cannot restart work.
    pub(super) fn require_known_turn(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: Option<&TurnId>,
    ) -> Result<(), SessionError> {
        if let Some(turn) = turn
            && self
                .store
                .read_latest_event(
                    stream,
                    branch,
                    Some(turn),
                    &EventKind::InvocationOutcomeUnknown,
                )?
                .is_some()
        {
            return Err(SessionError::StaleTarget(
                "effect_after_unknown_outcome".into(),
            ));
        }
        Ok(())
    }
    #[must_use]
    pub fn new(store: Arc<dyn SessionEventStore>) -> Self {
        Self {
            store,
            search: None,
            stream_locks: Mutex::new(HashMap::new()),
            workers: Arc::new(Semaphore::new(4)),
            wakeups: broadcast::channel(256).0,
            foreground: Mutex::new(HashMap::new()),
            execution_incarnation: EventId::new().to_string(),
        }
    }

    /// Inject a read-only index materializer; it receives authorized content
    /// and cannot append events, resolve identities or retain permission.
    pub fn with_search(
        mut self,
        search: Arc<dyn zeroclaw_api::session_events::search::SessionSearchPort>,
    ) -> Self {
        self.search = Some(search);
        self
    }

    /// Blocking store operations are bounded. Once spawned, cancellation of
    /// the waiter does not cancel a commit; retry the frozen request key.
    pub async fn append_async(
        self: &Arc<Self>,
        request: AppendRequest,
    ) -> Result<AppendReceipt, SessionError> {
        let coordinator = Arc::clone(self);
        self.blocking(move || coordinator.append(request)).await
    }

    async fn blocking<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce() -> Result<T, SessionError> + Send + 'static,
    ) -> Result<T, SessionError> {
        let permit = Arc::clone(&self.workers)
            .acquire_owned()
            .await
            .map_err(|error| SessionError::StoreUnhealthy(error.to_string()))?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| SessionError::StoreUnhealthy(error.to_string()))?
    }

    fn lock_for(&self, stream_id: &SessionStreamId) -> Arc<Mutex<()>> {
        let mut locks = self
            .stream_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, value| value.strong_count() > 0);
        if let Some(lock) = locks.get(stream_id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(stream_id.clone(), Arc::downgrade(&lock));
        lock
    }

    /// Append an already authenticated command batch.  The coordinator does
    /// not expose the backend object to callers, so every caller shares the
    /// same per-stream admission boundary.
    pub fn append(&self, request: AppendRequest) -> Result<AppendReceipt, SessionError> {
        let lock = self.lock_for(&request.stream_id);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let stream = request.stream_id.clone();
        let receipt = self.store.append_batch(request)?;
        let _ = self.wakeups.send(stream);
        Ok(receipt)
    }

    pub fn head(&self, stream: &SessionStreamId) -> Result<EventCursor, SessionError> {
        Ok(self.store.head(stream)?)
    }

    #[cfg(test)]
    fn read_page(
        &self,
        stream_id: &SessionStreamId,
        after: &EventCursor,
        through: Option<&EventCursor>,
        max_events: usize,
    ) -> Result<zeroclaw_api::session_events::EventPage, SessionError> {
        Ok(self
            .store
            .read_page(stream_id, after, through, max_events)?)
    }

    /// Resolve current permission and durably append the start boundary before
    /// returning an observation handle.  The resolver is called while no
    /// storage lock is held, so policy implementations may perform their own
    /// bounded lookup without deadlocking the event writer.
    pub fn admit_effect(
        self: &Arc<Self>,
        intent: EffectIntent,
        resolver: &dyn PermissionResolver,
    ) -> Result<EffectObservationHandle, SessionError> {
        intent.validate()?;
        let lock = self.lock_for(&intent.stream_id);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let (Some(branch), turn) = (
            intent.event.branch_id.as_ref(),
            intent.event.turn_id.as_ref(),
        ) {
            self.require_known_turn(&intent.stream_id, branch, turn)?;
        }
        resolver.authorize(&intent)?;
        let event = intent.event;
        let invocation_id = event
            .invocation_id
            .clone()
            .ok_or_else(|| SessionError::InvalidAdmission("missing invocation_id".to_owned()))?;
        let request = AppendRequest {
            stream_id: intent.stream_id.clone(),
            expected_head: intent.expected_head,
            idempotency_key: intent.idempotency_key,
            events: vec![event],
            payloads: intent.payloads,
            erasure_reference_commitment: None,
        };
        let branch_id = request.events[0].branch_id.clone();
        let turn_id = request.events[0].turn_id.clone();
        let started_event_id = request.events[0].event_id.clone();
        self.store.append_batch(request)?;
        let _ = self.wakeups.send(intent.stream_id.clone());
        Ok(EffectObservationHandle {
            started_event_id,
            coordinator: Arc::clone(self),
            stream_id: intent.stream_id,
            invocation_id,
            branch_id,
            turn_id,
        })
    }

    fn append_observation(
        &self,
        stream_id: &SessionStreamId,
        invocation_id: &InvocationId,
        event: SessionEvent,
        payloads: Vec<PayloadRecord>,
    ) -> Result<AppendReceipt, SessionError> {
        if event.stream_id != *stream_id || event.invocation_id.as_ref() != Some(invocation_id) {
            return Err(SessionError::InvocationMismatch);
        }
        if !matches!(
            event.body.kind(),
            EventKind::ProviderResponseProgressObserved
                | EventKind::ProviderAttemptCompleted
                | EventKind::ProviderAttemptFailed
                | EventKind::ToolEffectProgressObserved
                | EventKind::ToolEffectCompleted
                | EventKind::ToolEffectFailed
                | EventKind::InvocationOutcomeUnknown
                | EventKind::InvocationCancelled
                | EventKind::DeliveryCompleted
                | EventKind::DeliveryFailed
                | EventKind::ChildInvocationCompleted
        ) {
            return Err(SessionError::InvalidAdmission(
                "event kind is not an observation or outcome".to_owned(),
            ));
        }
        let lock = self.lock_for(stream_id);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let head = self.store.head(stream_id)?;
        let key = format!("observation:{}", event.event_id);
        let receipt = self.store.append_batch(AppendRequest {
            stream_id: stream_id.clone(),
            expected_head: head,
            idempotency_key: key,
            events: vec![event],
            payloads,
            erasure_reference_commitment: None,
        })?;
        let _ = self.wakeups.send(stream_id.clone());
        Ok(receipt)
    }
}

/// Bound to one invocation after its Started/Prepared event is durable.
/// Adapters receive this handle instead of a concrete event store or a
/// caller-controlled stream/actor identity.
pub struct EffectObservationHandle {
    started_event_id: EventId,
    coordinator: Arc<SessionCoordinator>,
    stream_id: SessionStreamId,
    invocation_id: InvocationId,
    branch_id: Option<BranchId>,
    turn_id: Option<TurnId>,
}

#[cfg(test)]
impl EffectObservationHandle {
    pub fn observe(
        &self,
        kind: EventKind,
        fields: serde_json::Value,
    ) -> Result<AppendReceipt, SessionError> {
        self.observe_with_payloads(kind, fields, Vec::new())
    }

    pub fn observe_with_payloads(
        &self,
        kind: EventKind,
        fields: serde_json::Value,
        payloads: Vec<PayloadRecord>,
    ) -> Result<AppendReceipt, SessionError> {
        let mut event = SessionEvent::new(
            self.stream_id.clone(),
            kind,
            fields,
            EventActor::System {
                component: "effect_observer".to_owned(),
            },
        )
        .map_err(|error| SessionError::InvalidAdmission(error.to_string()))?;
        event.invocation_id = Some(self.invocation_id.clone());
        event.branch_id = self.branch_id.clone();
        event.turn_id = self.turn_id.clone();
        let receipt = self.coordinator.append_observation(
            &self.stream_id,
            &self.invocation_id,
            event,
            payloads,
        )?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_api::session_events::{EventKind, PayloadId, SessionEvent};
    use zeroclaw_infra::session_events::SqliteEventStore;

    struct Allow;
    impl PermissionResolver for Allow {
        fn authorize(&self, _intent: &EffectIntent) -> Result<(), SessionError> {
            Ok(())
        }
    }

    #[test]
    fn admission_durably_binds_observations() {
        use zeroclaw_api::session_events::{CaptureMode, EventData, SessionId, StoredPayloadRef};
        let dir = TempDir::new().expect("temp");
        let store = Arc::new(SqliteEventStore::open(dir.path()).expect("store"));
        let coordinator = Arc::new(SessionCoordinator::new(store));
        let stream = SessionStreamId::new();
        let branch = BranchId::new();
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "test".into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::default(),
        )
        .expect("root");
        root.branch_id = Some(branch.clone());
        let receipt = coordinator
            .append(AppendRequest {
                stream_id: stream.clone(),
                expected_head: EventCursor::beginning(stream.clone()),
                idempotency_key: "root".into(),
                events: vec![root],
                payloads: vec![],
                erasure_reference_commitment: None,
            })
            .expect("create");
        let payload = PayloadRecord::new(
            PayloadId::new(),
            "session.evidence/v1".into(),
            b"{}".to_vec(),
        )
        .expect("payload");
        let reference = StoredPayloadRef::from(&payload);
        let invocation = InvocationId::new();
        let mut event = SessionEvent::typed(
            stream.clone(),
            EventData::DeliveryStarted {
                authorized_intent_ref: reference.clone(),
            },
            EventActor::default(),
        )
        .expect("start");
        event.invocation_id = Some(invocation);
        event.branch_id = Some(branch);
        let start_id = event.event_id.clone();
        let handle = coordinator
            .admit_effect(
                EffectIntent {
                    stream_id: stream.clone(),
                    expected_head: receipt.last,
                    idempotency_key: "start".into(),
                    event,
                    payloads: vec![payload],
                },
                &Allow,
            )
            .expect("admit");
        let fields =
            serde_json::json!({"started_event_id":start_id,"verified_receipt_ref":reference});
        handle
            .observe(EventKind::DeliveryCompleted, fields.clone())
            .expect("complete");
        assert!(matches!(
            handle.observe(EventKind::DeliveryCompleted, fields),
            Err(SessionError::Append(AppendError::InvalidTransition(_)))
        ));
        let page = coordinator
            .read_page(&stream, &EventCursor::beginning(stream.clone()), None, 10)
            .expect("read");
        assert_eq!(page.events.len(), 3);
    }
}
