//! Recovery only classifies loss after the execution owner verifies it.
//! Timeout, Drop and a missing outcome are not proof of failure or cancellation.
use super::*;
use zeroclaw_api::session_events::projection::{HistoryProjection, InvocationStatus};
use zeroclaw_api::session_events::{EventData, RecoveryDisposition, StoredPayloadRef};

/// Only the process/task ownership owner can prove that *all* writers of a
/// stream's previous execution have stopped. A clock timeout is insufficient.
/// The returned payload records that proof, not an execution authorization.
pub trait StreamExecutionLossVerifier: Send + Sync {
    fn verify_stream_loss(
        &self,
        stream: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError>;
}

/// TaskRegistry/process ownership remains canonical. A verifier must prove
/// that this exact execution can no longer publish observations. It returns a
/// reference to already recorded, credential-free evidence, never a grant.
pub trait ExecutionLossVerifier: Send + Sync {
    fn verify_loss(
        &self,
        stream: &SessionStreamId,
        invocation: &InvocationId,
    ) -> Result<StoredPayloadRef, SessionError>;
}

impl SessionCoordinator {
    /// Recover a whole session family without replaying any external effect.
    pub async fn recover_family(
        self: &Arc<Self>,
        stream: SessionStreamId,
        verifier: Arc<dyn StreamExecutionLossVerifier>,
    ) -> Result<(), SessionError> {
        let recovery_key = format!("family-owner-{}", EventId::new());
        let mut streams = vec![stream.clone()];
        let mut pending = vec![stream.clone()];
        let mut visited = std::collections::HashSet::from([stream.clone()]);
        while let Some(parent) = pending.pop() {
            let proof = verifier.verify_stream_loss(&parent, &self.head(&parent)?)?;
            if proof.codec != "session.evidence/v1" {
                return Err(SessionError::InvalidAdmission(
                    "family recovery requires ownership proof".into(),
                ));
            }
            self.resume_pending_erasures(parent.clone()).await?;
            let mut after = EventCursor::beginning(parent.clone());
            let mut through = None;
            loop {
                let owner = Arc::clone(self);
                let scope = parent.clone();
                let cursor = after.clone();
                let limit = through.clone();
                let page = self
                    .blocking(move || {
                        Ok(owner
                            .store
                            .read_page(&scope, &cursor, limit.as_ref(), 256)?)
                    })
                    .await?;
                through = Some(page.through.clone());
                for event in &page.events {
                    if matches!(event.body.data, EventData::ChildInvocationRequested { .. }) {
                        let invocation = event
                            .invocation_id
                            .as_ref()
                            .ok_or(SessionError::InvocationMismatch)?;
                        let child = SessionStreamId::parse(format!("child-{invocation}"))
                            .map_err(AppendError::from)?;
                        let child = if self.head(&child)?.sequence == 0 {
                            self.recover_requested_child(event.clone(), Arc::clone(&verifier))
                                .await?
                        } else {
                            child
                        };
                        if self.head(&child)?.sequence > 0 && visited.insert(child.clone()) {
                            streams.push(child.clone());
                            pending.push(child);
                        }
                    }
                }
                if !page.has_more {
                    break;
                }
                after.sequence = page
                    .events
                    .last()
                    .ok_or_else(|| {
                        SessionError::Read(ReadError::Corrupt("empty recovery page".into()))
                    })?
                    .sequence;
            }
        }
        for child in streams.into_iter().rev().filter(|child| child != &stream) {
            self.recover_interrupted_stream(
                child,
                format!("{recovery_key}-child"),
                Arc::clone(&verifier),
            )
            .await?;
        }
        self.recover_interrupted_stream(stream, recovery_key, verifier)
            .await?;
        Ok(())
    }
    /// Settle interrupted local transforms separately from uncertain external
    /// effects. No provider, tool, delivery or child is executed during replay.
    /// `recovery_key` identifies the ownership recovery command across retries.
    pub async fn recover_interrupted_stream(
        self: &Arc<Self>,
        stream: SessionStreamId,
        recovery_key: String,
        verifier: Arc<dyn StreamExecutionLossVerifier>,
    ) -> Result<Option<AppendReceipt>, SessionError> {
        let owner = Arc::clone(self);
        self.blocking(move || {
            use std::collections::BTreeMap;
            use zeroclaw_api::session_events::projection::TurnStatus;
            if recovery_key.is_empty() || recovery_key.len() > 512 {
                return Err(SessionError::InvalidAdmission(
                    "invalid recovery command identity".into(),
                ));
            }
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let key = format!("stream-recovery:{recovery_key}");
            if let Some(receipt) = owner.store.lookup_receipt(&stream, &key)? {
                return Ok(Some(receipt));
            }
            let through = owner.store.head(&stream)?;
            let evidence = verifier.verify_stream_loss(&stream, &through)?;
            if evidence.codec != "session.evidence/v1" {
                return Err(SessionError::InvalidAdmission(
                    "recovery requires typed ownership evidence".into(),
                ));
            }
            let mut projection = HistoryProjection::empty(stream.clone());
            let mut transforms = BTreeMap::new();
            let mut approvals = super::approvals::UnconsumedApprovals::default();
            let mut elicitations = BTreeMap::new();
            let mut history = Vec::new();
            loop {
                let page = owner.store.read_page(
                    &stream,
                    &projection.source_cursor,
                    Some(&through),
                    256)?;
                for event in &page.events {
                    approvals.observe(event, owner.store.as_ref())?;
                    match &event.body.data {
                        EventData::ElicitationRequested {request_id,..}=> {elicitations.insert(request_id.clone(),event.clone());}
                        EventData::ElicitationResolved {request_id,..}=> {elicitations.remove(request_id);}

                        EventData::ContextTransformStarted { .. } => {
                            transforms.insert(event.event_id.clone(), event.clone());
                        }
                        EventData::ContextTransformFailed {
                            started_event_id, ..
                        }
                        | EventData::ContextTransformCancelled {
                            started_event_id, ..
                        } => {
                            transforms.remove(started_event_id);
                        }
                        EventData::ContextProjectionCreated {
                            transform_started_event_id: Some(id),
                            ..
                        } => {
                            transforms.remove(id);
                        }
                        _ => {}
                    }
                }
                projection.apply_page(&page)?;
                history.extend(page.events);
                if !page.has_more {
                    break;
                }
            }
            if !projection.unknown_event_ids.is_empty() {
                return Err(SessionError::InvalidAdmission(
                    "unknown historical semantics prevent recovery".into(),
                ));
            }
            let running = projection
                .invocations
                .values()
                .any(|state| state.status == InvocationStatus::Running);
            let turns = projection
                .turns
                .values()
                .any(|state| matches!(state.status, TurnStatus::Queued | TurnStatus::Running));
            if !running && !turns && transforms.is_empty() && approvals.is_empty() && elicitations.is_empty() {
                return Ok(None);
            }
            let actor = EventActor::System {
                component: "execution_recovery".into(),
            };
            let event = |data: EventData| {
                SessionEvent::typed(stream.clone(), data, actor.clone()).map_err(AppendError::from)
            };
            let mut recovered = event(EventData::RecoveryObserved {
                prior_execution_ref: None,
                through_cursor: through.clone(),
                disposition: RecoveryDisposition::Interrupted,
                evidence_ref: StoredPayloadRef::from(&evidence),
            })?;
            // The root branch anchors this stream ownership observation;
            // each settlement below retains its original branch/turn binding.
            recovered.branch_id = owner
                .store
                .read_page(
                    &stream,
                    &EventCursor::beginning(stream.clone()),
                    Some(&through),
                    1)?
                .events
                .first()
                .and_then(|event| event.branch_id.clone());
            let cause = recovered.event_id.clone();
            let mut events = vec![recovered];
            let mut payloads=vec![evidence.clone()];
            let closed = history.iter().any(|event| matches!(&event.body.data, EventData::SessionClosed { reason_key } if reason_key != "session_ttl_expired"));
            let derived = history.first().is_some_and(|event| event.body.kind() == EventKind::DerivedSessionStarted);
            for (request_id,requested) in elicitations {
                let resolution=PayloadRecord::json("session.evidence/v1",&serde_json::json!({"state":"interrupted", "reason_key":"elicitation_execution_owner_lost", "ownership_evidence_ref":StoredPayloadRef::from(&evidence)})).map_err(AppendError::from)?;
                let mut resolved=event(EventData::ElicitationResolved {request_id,resolution_ref:StoredPayloadRef::from(&resolution)})?;
                resolved.branch_id=requested.branch_id; resolved.turn_id=requested.turn_id; resolved.causation_id=Some(cause.clone());
                events.push(resolved); payloads.push(resolution);
            }
            for (request_id, requested) in approvals.into_requests() {
                let mut invalidated = event(EventData::ApprovalInvalidated {
                    request_id,
                    confirmation_id: None,
                    reason_key: "approval_execution_owner_lost".into(),
                    evidence_ref: Some(StoredPayloadRef::from(&evidence)),
                })?;
                invalidated.branch_id = requested.branch_id;
                invalidated.turn_id = requested.turn_id;
                invalidated.invocation_id = requested.invocation_id;
                invalidated.causation_id = Some(cause.clone());
                events.push(invalidated);
            }
            for (id, state) in &projection.invocations {
                if state.status != InvocationStatus::Running {
                    continue;
                }
                let mut unknown = event(EventData::InvocationOutcomeUnknown {
                    last_observed_event_id: state.last_event_id.clone(),
                    reason_key: "execution_owner_lost".into(),
                    observed_partial_ref: None,
                    error_ref: Some(StoredPayloadRef::from(&evidence)),
                })?;
                unknown.branch_id = state.branch_id.clone();
                unknown.turn_id = state.turn_id.clone();
                unknown.invocation_id = Some(id.clone());
                unknown.causation_id = Some(cause.clone());
                events.push(unknown);
            }
            for (id, started) in transforms {
                let mut cancelled = event(EventData::ContextTransformCancelled {
                    started_event_id: id,
                    reason_key: "local_transform_execution_owner_lost".into(),
                })?;
                cancelled.branch_id = started.branch_id;
                cancelled.turn_id = started.turn_id;
                cancelled.causation_id = Some(cause.clone());
                events.push(cancelled);
            }
            for (id, state) in &projection.turns {
                if !matches!(state.status, TurnStatus::Queued | TurnStatus::Running) {
                    continue;
                }
                let accepted = owner
                    .store
                    .read_event(&state.accepted_event_id)?
                    .ok_or_else(|| ReadError::Gap("recovery turn admission missing".into()))?;
                let restartable = zeroclaw_api::session_events::rules::can_requeue_turn(&history, id);
                if restartable && (closed || derived) {
                    // A closed session cannot admit work. A derived stream's
                    // stopped host future cannot be reconstructed by a root
                    // dispatcher; no effects means cancellation is provable.
                    let mut cancelled = event(EventData::TurnCancelled {
                        reason_key: if closed { "session_closed_before_recovery" } else { "child_execution_owner_lost_before_effect" }.into(),
                        unresolved_invocation_ids: vec![],
                    })?;
                    cancelled.branch_id = accepted.branch_id;
                    cancelled.turn_id = Some(id.clone());
                    cancelled.causation_id = Some(cause.clone());
                    events.push(cancelled);
                    continue;
                }
                if restartable {
                    let prior_execution_ref = history.iter().rev().find_map(|event| {
                        if event.turn_id.as_ref() != Some(id) { return None; }
                        match &event.body.data {
                            EventData::TurnStarted {execution_binding_ref, ..}
                            | EventData::RecoveryObserved {disposition:RecoveryDisposition::Rescheduled {execution_binding_ref}, ..} => Some(execution_binding_ref.clone()),
                            _ => None,
                        }
                    });
                    let mut requeued = event(EventData::RecoveryObserved {
                        prior_execution_ref,
                        through_cursor: through.clone(),
                        disposition: RecoveryDisposition::Requeued,
                        evidence_ref: StoredPayloadRef::from(&evidence),
                    })?;
                    requeued.branch_id = accepted.branch_id;
                    requeued.turn_id = Some(id.clone());
                    requeued.causation_id = Some(cause.clone());
                    events.push(requeued);
                    continue;
                }
                let data = {
                    let invocation_ids = projection
                        .invocations
                        .iter()
                        .filter(|(_, invocation)| {
                            invocation.turn_id.as_ref() == Some(id)
                                && !invocation.background
                                && matches!(
                                    invocation.status,
                                    InvocationStatus::Running | InvocationStatus::Unknown
                                )
                        })
                        .map(|(id, _)| id.clone())
                        .collect();
                    EventData::TurnOutcomeUnknown {
                        invocation_ids,
                        reason_key: "execution_owner_lost_continuation_unverified".into(),
                        observed_partial_ref: None,
                    }
                };
                let mut terminal = event(data)?;
                terminal.branch_id = accepted.branch_id;
                terminal.turn_id = Some(id.clone());
                terminal.causation_id = Some(cause.clone());
                events.push(terminal);
            }
            let receipt = owner.store.append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: through,
                idempotency_key: key,
                events,
                payloads,
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(stream);
            Ok(Some(receipt))
        })
        .await
    }

    /// Append recovery and Unknown atomically without re-executing anything.
    /// Explicit failure/cancellation is recorded by the effect owner through
    /// its bound observer; this path handles only verified worker loss.
    pub async fn recover_interrupted_invocation(
        self: &Arc<Self>,
        stream: SessionStreamId,
        invocation: InvocationId,
        verifier: Arc<dyn ExecutionLossVerifier>,
    ) -> Result<AppendReceipt, SessionError> {
        let coordinator = Arc::clone(self);
        self.blocking(move || {
            let lock = coordinator.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let key = format!("recover-loss:{invocation}");
            if let Some(receipt) = coordinator.store.lookup_receipt(&stream, &key)? {
                return Ok(receipt);
            }
            let evidence = verifier.verify_loss(&stream, &invocation)?;
            let head = coordinator.store.head(&stream)?;
            let mut projection = HistoryProjection::empty(stream.clone());
            loop {
                let page = coordinator.store.read_page(
                    &stream,
                    &projection.source_cursor,
                    Some(&head),
                    256,
                )?;
                projection.apply_page(&page)?;
                if !page.has_more {
                    break;
                }
            }
            if !projection.unknown_event_ids.is_empty() {
                return Err(SessionError::InvalidAdmission(
                    "unknown historical semantics prevent recovery".into(),
                ));
            }
            let state = projection.invocations.get(&invocation).ok_or_else(|| {
                SessionError::InvalidAdmission("recovery invocation absent".into())
            })?;
            if state.status != InvocationStatus::Running {
                return Err(SessionError::StaleTarget(
                    "invocation already settled".into(),
                ));
            }
            let actor = EventActor::System {
                component: "execution_recovery".into(),
            };
            let mut recovered = SessionEvent::typed(
                stream.clone(),
                EventData::RecoveryObserved {
                    prior_execution_ref: None,
                    through_cursor: head.clone(),
                    disposition: RecoveryDisposition::ClassifiedUnknown,
                    evidence_ref: evidence.clone(),
                },
                actor.clone(),
            )
            .map_err(AppendError::from)?;
            recovered.branch_id = state.branch_id.clone();
            recovered.turn_id = state.turn_id.clone();
            recovered.causation_id = Some(state.last_event_id.clone());
            let mut unknown = SessionEvent::typed(
                stream.clone(),
                EventData::InvocationOutcomeUnknown {
                    last_observed_event_id: state.last_event_id.clone(),
                    reason_key: "execution_owner_lost".into(),
                    observed_partial_ref: None,
                    error_ref: Some(evidence),
                },
                actor,
            )
            .map_err(AppendError::from)?;
            unknown.branch_id = state.branch_id.clone();
            unknown.turn_id = state.turn_id.clone();
            unknown.invocation_id = Some(invocation.clone());
            unknown.causation_id = Some(recovered.event_id.clone());
            let receipt = coordinator.store.append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: head,
                idempotency_key: key,
                events: vec![recovered, unknown],
                payloads: vec![],
                erasure_reference_commitment: None,
            })?;
            let _ = coordinator.wakeups.send(stream);
            Ok(receipt)
        })
        .await
    }
}
