//! Child execution records form a family of independently serialized streams.
//! The existing child engine and TaskRegistry retain execution and task duties.
use super::engine::{EngineSession, EngineSessionAuthority};
use super::*;
use zeroclaw_api::session_events::capture::{
    ChildDispatchIntent, ChildExecutionPort, EngineTurnFuture, EngineTurnInput,
    SessionExecutionPort,
};
use zeroclaw_api::session_events::*;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(super) struct ChildDispatchCommand {
    pub stream: SessionStreamId,
    pub branch: BranchId,
    pub turn: TurnId,
    pub context: EventId,
    pub intent: ChildDispatchIntent,
}

enum ChildResultTarget {
    Request(EventId),
    Task(String),
}
impl ChildResultTarget {
    fn matches(&self, event: &SessionEvent) -> bool {
        match (self, &event.body.data) {
            (Self::Request(id), EventData::ChildInvocationRequested { .. }) => {
                *id == event.event_id
            }
            (Self::Task(id), EventData::ChildInvocationRequested { task_id, .. }) => {
                task_id.as_ref() == Some(id)
            }
            _ => false,
        }
    }
}

pub(super) struct ChildBinding {
    owner: Arc<SessionCoordinator>,
    parent: SessionEvent,
    session: EngineSession,
    input: EngineTurnInput,
    stream: SessionStreamId,
    authority: Arc<dyn EngineSessionAuthority>,
    parent_resolver: Arc<dyn PermissionResolver>,
}

/// The blocking initializer owns cleanup until its caller takes the handle.
struct ChildHandoff {
    child: Arc<ChildBinding>,
    armed: bool,
}
impl ChildHandoff {
    fn take(mut self) -> Arc<dyn ChildExecutionPort> {
        self.armed = false;
        self.child.clone()
    }
}
impl Drop for ChildHandoff {
    fn drop(&mut self) {
        if !self.armed || tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let child = self.child.clone();
        ::zeroclaw_spawn::spawn!(async move {
            if let Err(error) = child.interrupted("child_initializer_waiter_dropped").await {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_attrs(serde_json::json!({"error":error.to_string()})),
                    "child_handoff_settlement_failed"
                );
            }
        });
    }
}
impl SessionCoordinator {
    /// An exclusive execution-owner proof allows reconstructing a child
    /// identity whose parent intent committed before the child creation batch.
    /// Recovery never polls an engine or dispatches the frozen prompt.
    pub async fn recover_requested_child(
        self: &Arc<Self>,
        request: SessionEvent,
        verifier: Arc<dyn super::recovery::StreamExecutionLossVerifier>,
    ) -> Result<SessionStreamId, SessionError> {
        let owner = self.clone();
        self.blocking(move || {
            let invocation = request
                .invocation_id
                .as_ref()
                .ok_or(SessionError::InvocationMismatch)?;
            let child =
                SessionStreamId::parse(format!("child-{invocation}")).map_err(AppendError::from)?;
            let lock = owner.lock_for(&child);
            let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
            if owner.store.head(&child)?.sequence > 0 {
                return Ok(child);
            }
            let canonical = owner
                .store
                .read_event(&request.event_id)?
                .ok_or_else(|| SessionError::StaleTarget("child request missing".into()))?;
            if canonical != request || request.body.kind() != EventKind::ChildInvocationRequested {
                return Err(SessionError::InvocationMismatch);
            }
            if owner
                .stream_history(&request.stream_id)?
                .iter()
                .any(|event| {
                    event.invocation_id == request.invocation_id
                        && event.body.kind() == EventKind::ChildInvocationRejected
                })
            {
                return Ok(child);
            }
            let proof = verifier
                .verify_stream_loss(&request.stream_id, &owner.store.head(&request.stream_id)?)?;
            if proof.codec != "session.evidence/v1" {
                return Err(SessionError::InvalidAdmission(
                    "child recovery requires ownership proof".into(),
                ));
            }
            let branch = BranchId::parse(format!("branch-{child}")).map_err(AppendError::from)?;
            let mut root = SessionEvent::typed(
                child.clone(),
                EventData::DerivedSessionStarted {
                    execution_turn_id: Some(TurnId::new()),
                },
                request.actor.clone(),
            )
            .map_err(AppendError::from)?;
            root.branch_id = Some(branch.clone());
            root.causation_id = Some(request.event_id.clone());
            let context = PayloadRecord::json(
                "session.context/v1",
                &serde_json::json!({"message_refs":[],"history_has_trim_breadcrumb":false}),
            )
            .map_err(AppendError::from)?;
            let mut projection = SessionEvent::typed(
                child.clone(),
                EventData::ContextProjectionCreated {
                    source_ranges: vec![],
                    summary_ref: None,
                    budget: 0,
                    reason_key: "child_initialization_recovered".into(),
                    algorithm_version: "child-executor/v1".into(),
                    result_ref: StoredPayloadRef::from(&context),
                    summary_source_refs: vec![],
                    transform_started_event_id: None,
                },
                request.actor.clone(),
            )
            .map_err(AppendError::from)?;
            projection.branch_id = Some(branch.clone());
            projection.causation_id = Some(root.event_id.clone());
            let mut evidence = SessionEvent::typed(
                child.clone(),
                EventData::SessionMetadataChanged {
                    field: "initialization_recovery_evidence".into(),
                    new_value_ref: StoredPayloadRef::from(&proof),
                },
                request.actor,
            )
            .map_err(AppendError::from)?;
            evidence.branch_id = Some(branch);
            evidence.causation_id = Some(root.event_id.clone());
            owner.store.append_batch(AppendRequest {
                stream_id: child.clone(),
                expected_head: EventCursor::beginning(child.clone()),
                idempotency_key: format!("child-create:{}", request.event_id),
                events: vec![root, projection, evidence],
                payloads: vec![context, proof],
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(child.clone());
            Ok(child)
        })
        .await
    }
    async fn record_child_rejection(
        self: &Arc<Self>,
        command: ChildDispatchCommand,
        invocation: InvocationId,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<(), SessionError> {
        let owner = self.clone();
        let ChildDispatchCommand {
            stream,
            branch,
            turn,
            context,
            intent,
        } = command;
        self.blocking(move || {
            resolver.authorize_context(&stream,&branch)?;
            let lock=owner.lock_for(&stream);let _guard=lock.lock().unwrap_or_else(|p|p.into_inner());
            owner.require_known_turn(&stream,&branch,Some(&turn))?;
            let target=PayloadRecord::json("session.evidence/v1",&serde_json::json!({"agent_alias":intent.target_agent})).map_err(AppendError::from)?;
            let input=PayloadRecord::json("session.bytes/v1",&serde_json::json!({"prompt":intent.prompt.to_owned(),"parent_projection_event_id":context})).map_err(AppendError::from)?;
            let proof=PayloadRecord::json("session.evidence/v1",&serde_json::json!({"source":"current_child_admission","reason_key":"child_authority_denied"})).map_err(AppendError::from)?;
            let actor=EventActor::System {component:"child_executor".into()};
            let mut requested=SessionEvent::typed(stream.clone(),EventData::ChildInvocationRequested {relation:intent.relation,execution_mode:intent.execution_mode,target_agent_ref:StoredPayloadRef::from(&target),input_projection_ref:StoredPayloadRef::from(&input),policy_evidence_ref:StoredPayloadRef::from(&proof),task_id:intent.task_id},actor.clone()).map_err(AppendError::from)?;
            requested.branch_id=Some(branch.clone());requested.turn_id=Some(turn.clone());requested.invocation_id=Some(invocation.clone());
            let mut rejected=SessionEvent::typed(stream.clone(),EventData::ChildInvocationRejected {requested_event_id:requested.event_id.clone(),reason_key:"child_authority_denied".into(),policy_evidence_ref:Some(StoredPayloadRef::from(&proof))},actor).map_err(AppendError::from)?;
            rejected.branch_id=Some(branch);rejected.turn_id=Some(turn);rejected.invocation_id=Some(invocation);rejected.causation_id=Some(requested.event_id.clone());
            owner.store.append_batch(AppendRequest {stream_id:stream.clone(),expected_head:owner.store.head(&stream)?,idempotency_key:format!("child-rejection:{}",requested.event_id),events:vec![requested,rejected],payloads:vec![target,input,proof],erasure_reference_commitment:None})?;let _=owner.wakeups.send(stream);Ok(())
        }).await
    }
    pub(super) async fn begin_child(
        self: &Arc<Self>,
        command: ChildDispatchCommand,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<Arc<dyn ChildExecutionPort>, SessionError> {
        let invocation = InvocationId::new();
        let child_stream =
            SessionStreamId::parse(format!("child-{invocation}")).map_err(AppendError::from)?;
        let child_branch =
            BranchId::parse(format!("branch-{child_stream}")).map_err(AppendError::from)?;
        let authority = match resolver.child_authority(&command.intent.target_agent, &child_stream)
        {
            Ok(authority) => authority,
            Err(error) => {
                self.record_child_rejection(command, invocation, resolver.clone())
                    .await?;
                return Err(error);
            }
        };
        let rejection_command = command.clone();
        let ChildDispatchCommand {
            stream,
            branch,
            turn,
            context,
            intent,
        } = command;
        let owner = self.clone();
        let execution_turn_id = TurnId::new();
        let child_turn = execution_turn_id.clone();
        let target = intent.target_agent.clone();
        let prompt = intent.prompt.to_owned();
        let child_scope = child_stream.clone();
        let child_initial_branch = child_branch.clone();
        let child_authority = authority.clone();
        let parent_resolver = resolver.clone();
        let rejection_invocation = invocation.clone();
        let rejection_resolver = resolver.clone();
        let source = super::engine::current_engine_session_source();
        let prepared = match self
            .blocking(move || {
                let target_ref = PayloadRecord::json(
                    "session.evidence/v1",
                    &serde_json::json!({"agent_alias":target}),
                )
                .map_err(AppendError::from)?;
                // Freeze the exact parent seed and the child's normalized input.
                let input_ref = PayloadRecord::json(
                    "session.bytes/v1",
                    &serde_json::json!({"prompt":prompt,"parent_projection_event_id":context}),
                )
                .map_err(AppendError::from)?;
                let evidence = PayloadRecord::json(
                    "session.evidence/v1",
                    &serde_json::json!({"source":"current_child_admission","target_agent":target}),
                )
                .map_err(AppendError::from)?;
                let mut requested = SessionEvent::typed(
                    stream.clone(),
                    EventData::ChildInvocationRequested {
                        relation: intent.relation,
                        execution_mode: intent.execution_mode,
                        target_agent_ref: StoredPayloadRef::from(&target_ref),
                        input_projection_ref: StoredPayloadRef::from(&input_ref),
                        policy_evidence_ref: StoredPayloadRef::from(&evidence),
                        task_id: intent.task_id,
                    },
                    EventActor::System {
                        component: "child_executor".into(),
                    },
                )
                .map_err(AppendError::from)?;
                requested.branch_id = Some(branch);
                requested.turn_id = Some(turn.clone());
                requested.invocation_id = Some(invocation);
                let started = owner
                    .store
                    .lookup_receipt(&stream, &format!("turn-start:{turn}"))?
                    .ok_or_else(|| {
                        SessionError::StaleTarget("child parent turn start missing".into())
                    })?;
                let started = owner
                    .store
                    .read_event(started.event_ids.first().ok_or_else(|| {
                        SessionError::StoreUnhealthy("parent start receipt empty".into())
                    })?)?
                    .ok_or_else(|| {
                        SessionError::StoreUnhealthy("parent start event missing".into())
                    })?;
                let EventData::TurnStarted {
                    accepted_event_id, ..
                } = started.body.data
                else {
                    return Err(SessionError::StoreUnhealthy(
                        "parent start kind mismatch".into(),
                    ));
                };
                let accepted = owner.store.read_event(&accepted_event_id)?.ok_or_else(|| {
                    SessionError::StoreUnhealthy("parent admission event missing".into())
                })?;
                let actor = accepted.actor;
                child_authority.authorize_actor(
                    &actor,
                    &child_scope,
                    super::query::SessionAccess::Write,
                )?;
                resolver.authorize_context(
                    &stream,
                    requested
                        .branch_id
                        .as_ref()
                        .ok_or(SessionError::InvocationMismatch)?,
                )?;
                let seed = owner
                    .store
                    .read_event(&context)?
                    .ok_or_else(|| SessionError::StaleTarget("child seed missing".into()))?;
                let history = owner.stream_history(&stream)?;
                let lineage = lineage::frozen_lineage(
                    &history,
                    &stream,
                    requested
                        .branch_id
                        .as_ref()
                        .ok_or(SessionError::InvocationMismatch)?,
                    owner.store.head(&stream)?.sequence,
                )?;
                if seed.stream_id != stream
                    || !lineage::includes(&lineage, &seed)
                    || seed.body.kind() != EventKind::ContextProjectionCreated
                {
                    return Err(SessionError::InvalidAdmission(
                        "child seed scope mismatch".into(),
                    ));
                }
                owner.admit_effect(
                    EffectIntent {
                        stream_id: stream.clone(),
                        expected_head: owner.store.head(&stream)?,
                        idempotency_key: format!("child-request:{}", requested.event_id),
                        event: requested.clone(),
                        payloads: vec![target_ref, input_ref, evidence],
                    },
                    resolver.as_ref(),
                )?;
                let mut derived = SessionEvent::typed(
                    child_scope.clone(),
                    EventData::DerivedSessionStarted {
                        execution_turn_id: Some(child_turn),
                    },
                    requested.actor.clone(),
                )
                .map_err(AppendError::from)?;
                derived.branch_id = Some(child_initial_branch.clone());
                derived.causation_id = Some(requested.event_id.clone());
                let empty = PayloadRecord::json(
                    "session.context/v1",
                    &serde_json::json!({"messages":[],"history_has_trim_breadcrumb":false}),
                )
                .map_err(AppendError::from)?;
                let mut projection = SessionEvent::typed(
                    child_scope.clone(),
                    EventData::ContextProjectionCreated {
                        source_ranges: vec![SourceRange {
                            stream_id: seed.stream_id,
                            from_seq: seed.sequence,
                            to_seq: seed.sequence,
                            disposition: RangeDisposition::Omitted,
                        }],
                        summary_ref: None,
                        budget: 0,
                        reason_key: "child_initial_context".into(),
                        algorithm_version: "child-executor/v1".into(),
                        result_ref: StoredPayloadRef::from(&empty),
                        summary_source_refs: vec![],
                        transform_started_event_id: None,
                    },
                    requested.actor.clone(),
                )
                .map_err(AppendError::from)?;
                projection.branch_id = Some(child_initial_branch);
                if let Err(error) = owner.append(AppendRequest {
                    stream_id: child_scope.clone(),
                    expected_head: EventCursor::beginning(child_scope),
                    idempotency_key: format!("child-create:{}", requested.event_id),
                    events: vec![derived, projection],
                    payloads: vec![empty],
                    erasure_reference_commitment: None,
                }) {
                    let settlement = owner.record_child_initialization_failure(&requested, &error);
                    return Err(match settlement {
                        Ok(()) => error,
                        Err(settlement) => {
                            SessionError::Operation(anyhow::Error::new(error).context(format!(
                                "child initialization settlement failed: {settlement}"
                            )))
                        }
                    });
                }
                let input = EngineTurnInput {
                    execution_workspace: None,
                    turn_id: execution_turn_id.to_string(),
                    agent_alias: Some(intent.target_agent),
                    ingress: zeroclaw_api::ingress::IngressContext::sub_turn(),
                    reply_route: None,
                    parts: zeroclaw_api::session_events::capture::EngineInputPart::text_and_files(
                        intent.prompt,
                        vec![],
                    ),
                    provider_ref: String::new(),
                    model: String::new(),
                };
                let mut session = EngineSession::with_actor(
                    owner.clone(),
                    child_stream.clone(),
                    child_branch,
                    actor,
                    authority.clone(),
                );
                if let Some(source) = source {
                    session = session.with_source(source);
                }
                Ok(ChildHandoff {
                    child: Arc::new(ChildBinding {
                        owner: owner.clone(),
                        parent: requested,
                        session,
                        input,
                        stream: child_stream,
                        authority,
                        parent_resolver,
                    }),
                    armed: true,
                })
            })
            .await
        {
            Ok(binding) => binding,
            Err(error @ SessionError::PermissionDenied(_)) => {
                self.record_child_rejection(
                    rejection_command,
                    rejection_invocation,
                    rejection_resolver,
                )
                .await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        Ok(prepared.take())
    }

    fn record_child_initialization_failure(
        &self,
        requested: &SessionEvent,
        error: &SessionError,
    ) -> Result<(), SessionError> {
        let lock = self.lock_for(&requested.stream_id);
        let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        if self
            .stream_history(&requested.stream_id)?
            .iter()
            .any(|event| {
                event.invocation_id == requested.invocation_id
                    && (zeroclaw_api::session_events::rules::terminal(&event.body.kind())
                        || event.body.kind() == EventKind::OutcomeReconciled)
            })
        {
            return Ok(());
        }
        let detail = PayloadRecord::json(
            "session.error-detail/v1",
            &serde_json::json!({
                "error_key":"child_initialization_write_failed",
                "detail":error.to_string(),
            }),
        )
        .map_err(AppendError::from)?;
        let mut unknown = SessionEvent::typed(
            requested.stream_id.clone(),
            EventData::InvocationOutcomeUnknown {
                last_observed_event_id: requested.event_id.clone(),
                reason_key: "child_initialization_write_failed".into(),
                observed_partial_ref: None,
                error_ref: Some(StoredPayloadRef::from(&detail)),
            },
            requested.actor.clone(),
        )
        .map_err(AppendError::from)?;
        unknown.branch_id = requested.branch_id.clone();
        unknown.turn_id = requested.turn_id.clone();
        unknown.invocation_id = requested.invocation_id.clone();
        unknown.causation_id = Some(requested.event_id.clone());
        self.store.append_batch(AppendRequest {
            stream_id: requested.stream_id.clone(),
            expected_head: self.store.head(&requested.stream_id)?,
            idempotency_key: format!("child-initialization-failed:{}", requested.event_id),
            events: vec![unknown],
            payloads: vec![detail],
            erasure_reference_commitment: None,
        })?;
        let _ = self.wakeups.send(requested.stream_id.clone());
        Ok(())
    }
}

#[async_trait::async_trait]
impl ChildExecutionPort for ChildBinding {
    async fn execute(&self, engine: EngineTurnFuture<'_>) -> anyhow::Result<String> {
        let result = self.session.execute(self.input.clone(), engine).await;
        self.settle_parent().await?;
        if let (
            Ok(value),
            EventData::ChildInvocationRequested {
                execution_mode: ChildExecutionMode::Sync,
                ..
            },
        ) = (&result, &self.parent.body.data)
        {
            self.owner
                .consume_child_request(
                    self.parent.stream_id.clone(),
                    self.parent
                        .branch_id
                        .clone()
                        .ok_or(SessionError::InvocationMismatch)?,
                    self.parent
                        .turn_id
                        .clone()
                        .ok_or(SessionError::InvocationMismatch)?,
                    ChildResultTarget::Request(self.parent.event_id.clone()),
                    serde_json::Value::String(value.clone()),
                    self.parent_resolver.clone(),
                )
                .await?;
        }
        result
    }
    async fn request_cancellation(&self, reason: &str) -> anyhow::Result<()> {
        let owner = self.owner.clone();
        let parent = self.parent.clone();
        let reason = reason.to_owned();
        let resolver = self.parent_resolver.clone();
        self.owner
            .blocking(move || {
                resolver.authorize_context(
                    &parent.stream_id,
                    parent
                        .branch_id
                        .as_ref()
                        .ok_or(SessionError::InvocationMismatch)?,
                )?;
                let lock = owner.lock_for(&parent.stream_id);
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                let mut event = SessionEvent::typed(
                    parent.stream_id.clone(),
                    EventData::ChildCancellationRequested {
                        requested_event_id: parent.event_id.clone(),
                        reason_key: reason,
                        target_scope_ref: TargetRef::Invocation {
                            invocation_id: parent
                                .invocation_id
                                .clone()
                                .ok_or(SessionError::InvocationMismatch)?,
                        },
                    },
                    parent.actor.clone(),
                )
                .map_err(AppendError::from)?;
                event.branch_id = parent.branch_id.clone();
                event.turn_id = parent.turn_id.clone();
                event.invocation_id = parent.invocation_id.clone();
                event.causation_id = Some(parent.event_id.clone());
                let key = format!("child-cancel:{}", parent.event_id);
                if owner.observation_retry(&key, &event)?.is_none() {
                    owner.store.append_batch(AppendRequest {
                        stream_id: parent.stream_id.clone(),
                        expected_head: owner.store.head(&parent.stream_id)?,
                        idempotency_key: key,
                        events: vec![event],
                        payloads: vec![],
                        erasure_reference_commitment: None,
                    })?;
                    let _ = owner.wakeups.send(parent.stream_id);
                }
                Ok(())
            })
            .await?;
        Ok(())
    }
    async fn interrupted(&self, reason_key: &str) -> anyhow::Result<()> {
        if self.cancel_unadmitted(reason_key).await? {
            return Ok(());
        }
        self.owner
            .recover_interrupted_stream(
                self.stream.clone(),
                format!("child-interruption:{}", self.parent.event_id),
                Arc::new(StoppedChildFuture {
                    reason: reason_key.to_owned(),
                }),
            )
            .await?;
        self.settle_parent().await
    }
}

struct StoppedChildFuture {
    reason: String,
}
impl super::recovery::StreamExecutionLossVerifier for StoppedChildFuture {
    fn verify_stream_loss(
        &self,
        stream: &SessionStreamId,
        through: &EventCursor,
    ) -> Result<PayloadRecord, SessionError> {
        PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source":"host_child_future_stopped", "stream":stream, "through":through,
                "reason_key":self.reason, "remote_outcome":"unverified",
            }),
        )
        .map_err(AppendError::from)
        .map_err(Into::into)
    }
}
impl ChildBinding {
    async fn cancel_unadmitted(&self, reason_key: &str) -> Result<bool, SessionError> {
        let owner = self.owner.clone();
        let parent = self.parent.clone();
        let child = self.stream.clone();
        let turn = TurnId::parse(&self.input.turn_id).map_err(AppendError::from)?;
        let reason = reason_key.to_owned();
        self.owner.blocking(move || {
            // Closing an unaccepted stream is the local execution fence. It
            // records no invented turn and races admission under the same lock.
            let (derived, closed) = {
                let lock = owner.lock_for(&child);
                let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
                let history = owner.stream_history(&child)?;
                if history.iter().any(|event| event.body.kind() == EventKind::TurnAccepted) {
                    return Ok(false);
                }
                let derived = history.first().ok_or_else(|| SessionError::StoreUnhealthy("child derivation missing".into()))?;
                if !matches!(&derived.body.data, EventData::DerivedSessionStarted {execution_turn_id:Some(bound)} if bound==&turn)
                    || derived.causation_id.as_ref()!=Some(&parent.event_id)
                    || history.iter().any(|event| !matches!(event.body.kind(), EventKind::DerivedSessionStarted | EventKind::ContextProjectionCreated | EventKind::SessionMetadataChanged | EventKind::SessionClosed))
                {
                    return Err(SessionError::InvalidAdmission("unaccepted child has unexpected history".into()));
                }
                let closed = if let Some(closed) = history.iter().find(|event| event.body.kind()==EventKind::SessionClosed) {
                    closed.clone()
                } else {
                    let mut closed = SessionEvent::typed(child.clone(), EventData::SessionClosed {
                        reason_key:"child_owner_stopped_before_admission".into(),
                    }, EventActor::System {component:"child_executor".into()}).map_err(AppendError::from)?;
                    closed.branch_id=derived.branch_id.clone();
                    closed.causation_id=Some(derived.event_id.clone());
                    let receipt=owner.store.append_batch(AppendRequest {
                        stream_id:child.clone(), expected_head:owner.store.head(&child)?,
                        idempotency_key:format!("child-unadmitted-close:{}",parent.event_id),
                        events:vec![closed], payloads:vec![], erasure_reference_commitment:None,
                    })?;
                    let _=owner.wakeups.send(child.clone());
                    let closed_id=receipt.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("child close receipt empty".into()))?;
                    owner.store.read_event(closed_id)?.ok_or_else(|| SessionError::StoreUnhealthy("child close event missing".into()))?
                };
                (derived.clone(),closed)
            };
            let lock=owner.lock_for(&parent.stream_id);
            let _guard=lock.lock().unwrap_or_else(|error|error.into_inner());
            let history=owner.stream_history(&parent.stream_id)?;
            let mut cancellation=None;
            for event in history.iter().filter(|event|event.invocation_id==parent.invocation_id) {
                match &event.body.data {
                    EventData::InvocationOutcomeUnknown {..}
                    | EventData::ChildInvocationCompleted {..}
                    | EventData::ChildInvocationRejected {..}
                    | EventData::InvocationCancelled {..}
                    | EventData::OutcomeReconciled {..}=> {
                        // Unknown child reconciliation requires a real bound
                        // turn terminal. Stream closure alone cannot replace it.
                        return Ok(true);
                    }
                    EventData::ChildCancellationRequested {requested_event_id,..} if requested_event_id==&parent.event_id => cancellation=Some(event.event_id.clone()),
                    _=>{},
                }
            }
            let build=|data| {
                let mut event=SessionEvent::typed(parent.stream_id.clone(),data,parent.actor.clone()).map_err(AppendError::from)?;
                event.branch_id=parent.branch_id.clone(); event.turn_id=parent.turn_id.clone();
                event.invocation_id=parent.invocation_id.clone(); event.causation_id=Some(parent.event_id.clone());
                Ok::<_,SessionError>(event)
            };
            let mut events=Vec::new();
            let cancellation_event_id=match cancellation {
                Some(id)=>id,
                None=> {
                    let requested=build(EventData::ChildCancellationRequested {
                        requested_event_id:parent.event_id.clone(), reason_key:reason.clone(),
                        target_scope_ref:TargetRef::Invocation {invocation_id:parent.invocation_id.clone().ok_or(SessionError::InvocationMismatch)?},
                    })?;
                    let id=requested.event_id.clone(); events.push(requested); id
                }
            };
            let evidence=super::retry::payload(&parent.event_id,"child-unadmitted-cancellation","session.evidence/v1",&serde_json::json!({
                "source":"host_child_future_stopped_before_admission", "reason_key":reason,
                "derived_event_id":derived.event_id, "closed_event_id":closed.event_id,
                "child_closed_cursor":EventCursor {stream_id:child,sequence:closed.sequence},
            }))?;
            events.push(build(EventData::InvocationCancelled {
                cancellation_event_id,verification_evidence_ref:StoredPayloadRef::from(&evidence),observed_partial_ref:None,
            })?);
            owner.store.append_batch(AppendRequest {
                stream_id:parent.stream_id.clone(),expected_head:owner.store.head(&parent.stream_id)?,
                idempotency_key:format!("child-unadmitted-outcome:{}",parent.event_id),
                events,payloads:vec![evidence],erasure_reference_commitment:None,
            })?;
            let _=owner.wakeups.send(parent.stream_id);
            Ok(true)
        }).await
    }

    async fn settle_parent(&self) -> anyhow::Result<()> {
        // A durable terminal cursor is evidence even when the child failed.
        // If no terminal committed, the parent's effect stays unresolved.
        let owner = self.owner.clone();
        let parent = self.parent.clone();
        let child = self.stream.clone();
        let turn = TurnId::parse(&self.input.turn_id)?;
        let authority = self.authority.clone();
        let branch = self.session.branch.clone();
        self.owner
            .blocking(move || {
                let terminal = if let Some(receipt) = owner
                    .store
                    .lookup_receipt(&child, &format!("turn-outcome:{turn}"))?
                {
                    receipt.last
                } else if let Some(event) = owner
                    .store
                    .read_latest_event(
                        &child,
                        &branch,
                        Some(&turn),
                        &EventKind::TurnOutcomeUnknown,
                    )?
                    .or(owner.store.read_latest_event(
                        &child,
                        &branch,
                        Some(&turn),
                        &EventKind::TurnCancelled,
                    )?)
                {
                    EventCursor {
                        stream_id: child.clone(),
                        sequence: event.sequence,
                    }
                } else {
                    return Ok(());
                };
                let page = owner.store.read_page(
                    &child,
                    &EventCursor {
                        stream_id: child.clone(),
                        sequence: terminal.sequence - 1,
                    },
                    Some(&terminal),
                    1,
                    4 * 1024 * 1024,
                )?;
                let event = page.events.first().ok_or_else(|| {
                    SessionError::StoreUnhealthy("child terminal event missing".into())
                })?;
                authority.authorize_context(
                    &child,
                    event.branch_id.as_ref().ok_or_else(|| {
                        SessionError::StoreUnhealthy("child terminal branch missing".into())
                    })?,
                )?;
                let lock = owner.lock_for(&parent.stream_id);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let committed = owner.store.read_event(&parent.event_id)?.ok_or_else(|| {
                    SessionError::StoreUnhealthy("parent child request missing".into())
                })?;
                let head = owner.store.head(&parent.stream_id)?;
                let mut after = EventCursor {
                    stream_id: parent.stream_id.clone(),
                    sequence: committed.sequence,
                };
                let mut unknown = None;
                loop {
                    let page = owner.store.read_page(
                        &parent.stream_id,
                        &after,
                        Some(&head),
                        256,
                        4 * 1024 * 1024,
                    )?;
                    for observed in &page.events {
                        if observed.invocation_id != parent.invocation_id {
                            continue;
                        }
                        match observed.body.data {
                            EventData::InvocationOutcomeUnknown { .. } => {
                                unknown = Some(observed.event_id.clone())
                            }
                            EventData::ChildInvocationCompleted { .. }
                            | EventData::OutcomeReconciled { .. } => return Ok(()),
                            _ => {}
                        }
                    }
                    if !page.has_more {
                        break;
                    }
                    after.sequence = page
                        .events
                        .last()
                        .ok_or_else(|| {
                            SessionError::StoreUnhealthy("child settlement page empty".into())
                        })?
                        .sequence;
                }
                let child_unknown = event.body.kind() == EventKind::TurnOutcomeUnknown;
                if child_unknown && unknown.is_some() {
                    return Ok(());
                }
                let mut payloads = Vec::new();
                let data = if let Some(unknown_event_id) = unknown {
                    // The verified child terminal is new evidence about the
                    // old uncertainty. Never append a second invocation terminal.
                    let evidence = super::retry::payload(
                        &parent.event_id,
                        "child-terminal-evidence",
                        "session.evidence/v1",
                        &ChildTerminalEvidence {
                            child_terminal_cursor: terminal.clone(),
                            terminal_event_id: event.event_id.clone(),
                        },
                    )?;
                    let resolution = match &event.body.data {
                        EventData::TurnCancelled { .. } => Reconciliation::Cancelled,
                        EventData::TurnCommitted { completion_ref } => {
                            let PayloadRead::Present(completion) =
                                owner.store.read_payload(&completion_ref.payload_id)?
                            else {
                                return Err(SessionError::StaleTarget(
                                    "child completion unavailable".into(),
                                ));
                            };
                            match serde_json::from_slice::<
                                zeroclaw_api::session_events::TurnCompletion,
                            >(&completion.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?
                            {
                                zeroclaw_api::session_events::TurnCompletion::Success {
                                    ..
                                } => Reconciliation::Completed { result_ref: None },
                                zeroclaw_api::session_events::TurnCompletion::Failed {
                                    error_ref,
                                    ..
                                } => Reconciliation::Failed { error_ref },
                            }
                        }
                        _ => {
                            return Err(SessionError::StoreUnhealthy(
                                "unexpected child terminal kind".into(),
                            ));
                        }
                    };
                    let verification_evidence_ref = StoredPayloadRef::from(&evidence);
                    payloads.push(evidence);
                    EventData::OutcomeReconciled {
                        unknown_event_id,
                        resolution,
                        verification_evidence_ref,
                    }
                } else if child_unknown {
                    EventData::InvocationOutcomeUnknown {
                        last_observed_event_id: parent.event_id.clone(),
                        reason_key: "child_execution_continuation_unresolved".into(),
                        observed_partial_ref: None,
                        error_ref: None,
                    }
                } else {
                    EventData::ChildInvocationCompleted {
                        child_terminal_cursor: terminal,
                        consumed_result_ref: None,
                    }
                };
                let mut outcome =
                    SessionEvent::typed(parent.stream_id.clone(), data, parent.actor.clone())
                        .map_err(AppendError::from)?;
                outcome.branch_id = parent.branch_id;
                outcome.turn_id = parent.turn_id;
                outcome.invocation_id = parent.invocation_id;
                outcome.causation_id = Some(parent.event_id.clone());
                outcome.event_id = EventId::parse(format!("child-outcome-{}", parent.event_id))
                    .map_err(AppendError::from)?;
                let key = format!("observation:{}", outcome.event_id);
                if owner.observation_retry(&key, &outcome)?.is_some() {
                    return Ok(());
                }
                owner.store.append_batch(AppendRequest {
                    stream_id: parent.stream_id.clone(),
                    expected_head: head,
                    idempotency_key: key,
                    events: vec![outcome],
                    payloads,
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(parent.stream_id);
                Ok(())
            })
            .await?;
        Ok(())
    }
}

impl SessionCoordinator {
    pub(super) async fn consume_child_result(
        self: &Arc<Self>,
        stream: SessionStreamId,
        branch: BranchId,
        turn: TurnId,
        task_id: String,
        value: serde_json::Value,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<(), SessionError> {
        self.consume_child_request(
            stream,
            branch,
            turn,
            ChildResultTarget::Task(task_id),
            value,
            resolver,
        )
        .await
    }

    async fn consume_child_request(
        self: &Arc<Self>,
        stream: SessionStreamId,
        branch: BranchId,
        turn: TurnId,
        target: ChildResultTarget,
        value: serde_json::Value,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<(), SessionError> {
        let owner = self.clone();
        self.blocking(move || {
            resolver.authorize_context(&stream, &branch)?;
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let head = owner.store.head(&stream)?;
            let mut after = EventCursor::beginning(stream.clone());
            let mut request = None;
            let mut outcome = None;
            let mut consumer_terminated = false;
            loop {
                let page = owner.store.read_page(&stream, &after, Some(&head), 256, 4 * 1024 * 1024)?;
                for event in &page.events {
                    if event.branch_id.as_ref() != Some(&branch) { continue; }
                    if event.turn_id.as_ref() == Some(&turn)
                        && matches!(event.body.kind(), EventKind::TurnCommitted | EventKind::TurnCancelled | EventKind::TurnOutcomeUnknown)
                    {
                        consumer_terminated = true;
                    }
                    if target.matches(event) {
                        if request.is_some() { return Err(SessionError::StoreUnhealthy("ambiguous child task correlation".into())); }
                        request=Some(event.clone());
                    }
                    if request.as_ref().is_some_and(|requested| requested.invocation_id==event.invocation_id) && matches!(event.body.kind(), EventKind::ChildInvocationCompleted | EventKind::OutcomeReconciled) {
                        outcome=Some(event.clone());
                    }
                }
                if !page.has_more { break; }
                after.sequence=page.events.last().ok_or_else(|| SessionError::StoreUnhealthy("child consumption page empty".into()))?.sequence;
            }
            // A TaskRegistry result imported from legacy storage has no native
            // child invocation. Its tool result is still captured normally.
            let Some(request)=request else { return Ok(()); };
            let outcome=outcome.ok_or_else(|| SessionError::StaleTarget("child result has no durable settlement".into()))?;
            let started=owner.store.lookup_receipt(&stream,&format!("turn-start:{turn}"))?.ok_or_else(|| SessionError::StaleTarget("consumer turn not started".into()))?;
            let started=owner.store.read_event(started.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("consumer receipt empty".into()))?)?.ok_or_else(|| SessionError::StoreUnhealthy("consumer turn event missing".into()))?;
            let EventData::TurnStarted {accepted_event_id,..}=started.body.data else {return Err(SessionError::StoreUnhealthy("consumer turn kind mismatch".into()));};
            let key=format!("child-consumed:{}:{turn}",request.event_id);
            let payload=super::retry::payload(&request.event_id,&format!("consumed-{turn}"),"session.bytes/v1",&value)?;
            if let Some(receipt)=owner.store.lookup_receipt(&stream,&key)? {
                let existing=owner.store.read_event(receipt.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("consumption receipt empty".into()))?)?.ok_or_else(|| SessionError::StoreUnhealthy("consumption event missing".into()))?;
                if !matches!(existing.body.data,EventData::ChildResultConsumed {consumed_result_ref,..} if consumed_result_ref==StoredPayloadRef::from(&payload)) {return Err(SessionError::InvalidAdmission("child consumption changed during retry".into()));}
                return Ok(());
            }
            if consumer_terminated {
                // A child may settle after its parent has stopped. Settlement
                // is still real, but no running consumer received this value.
                if matches!(target, ChildResultTarget::Request(_)) { return Ok(()); }
                return Err(SessionError::StaleTarget("consumer turn already terminal".into()));
            }
            let mut event=SessionEvent::typed(stream.clone(),EventData::ChildResultConsumed {child_outcome_event_id:outcome.event_id,consumed_result_ref:StoredPayloadRef::from(&payload),consumer_event_id:accepted_event_id},EventActor::System {component:"child_result_consumer".into()}).map_err(AppendError::from)?;
            event.branch_id=Some(branch.clone());event.turn_id=Some(turn);event.invocation_id=request.invocation_id;event.causation_id=Some(request.event_id);
            resolver.authorize_context(&stream,&branch)?;
            owner.store.append_batch(AppendRequest {stream_id:stream.clone(),expected_head:head,idempotency_key:key,events:vec![event],payloads:vec![payload],erasure_reference_commitment:None})?;
            let _=owner.wakeups.send(stream);
            Ok(())
        }).await
    }
}
