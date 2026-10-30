//! Binding/admission for provider adapters. Protocol capture stays outside kernel.
use super::*;
use zeroclaw_api::session_events::capture::{
    ProviderAdmissionPort, ProviderDispatchIntent, ProviderObservationPort,
};
use zeroclaw_api::session_events::{
    EventData, EventId, ModelResolution, ProviderPurpose, StoredPayloadRef, canonical_json, lineage,
};

/// Canonical execution scope, resolved by the admitted turn owner. No config,
/// capability snapshot or stored permission grant lives in this handle.
pub(super) struct ProviderCaptureScope {
    pub(super) coordinator: Arc<SessionCoordinator>,
    pub(super) stream: SessionStreamId,
    pub(super) branch: BranchId,
    pub(super) turn: Option<TurnId>,
    pub(super) context_event_id: EventId,
    pub(super) purpose: ProviderPurpose,
    pub(super) transform_event_id: Option<EventId>,
    pub(super) resolver: Arc<dyn PermissionResolver>,
}

impl SessionCoordinator {
    pub fn provider_capture(
        self: &Arc<Self>,
        stream: SessionStreamId,
        branch: BranchId,
        turn: TurnId,
        context_event_id: EventId,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Arc<dyn ProviderAdmissionPort> {
        Arc::new(ProviderCaptureScope {
            coordinator: Arc::clone(self),
            stream,
            branch,
            turn: Some(turn),
            context_event_id,
            purpose: ProviderPurpose::Main,
            transform_event_id: None,
            resolver,
        })
    }
}

#[async_trait::async_trait]
impl ProviderAdmissionPort for ProviderCaptureScope {
    async fn consume_child_result(
        &self,
        task_id: &str,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let turn = self
            .turn
            .clone()
            .ok_or_else(|| anyhow::Error::msg("child consumption requires admitted turn"))?;
        self.coordinator
            .consume_child_result(
                self.stream.clone(),
                self.branch.clone(),
                turn,
                task_id.into(),
                value.clone(),
                self.resolver.clone(),
            )
            .await?;
        Ok(())
    }
    async fn begin_child(
        &self,
        intent: zeroclaw_api::session_events::capture::ChildDispatchIntent,
    ) -> anyhow::Result<Arc<dyn zeroclaw_api::session_events::capture::ChildExecutionPort>> {
        let turn = self
            .turn
            .clone()
            .ok_or_else(|| anyhow::Error::msg("child dispatch requires admitted parent turn"))?;
        anyhow::ensure!(
            self.purpose == ProviderPurpose::Main,
            "maintenance cannot launch child execution"
        );
        Ok(self
            .coordinator
            .begin_child(
                super::children::ChildDispatchCommand {
                    stream: self.stream.clone(),
                    branch: self.branch.clone(),
                    turn,
                    context: self.context_event_id.clone(),
                    intent,
                },
                self.resolver.clone(),
            )
            .await?)
    }
    async fn observe_model_change(
        &self,
        selection: zeroclaw_api::session_events::capture::ModelOverrideSelection,
        outcome: zeroclaw_api::session_events::capture::ModelChangeDisposition,
    ) -> anyhow::Result<()> {
        use zeroclaw_api::session_events::TargetRef;
        anyhow::ensure!(
            self.purpose == ProviderPurpose::Main,
            "maintenance_cannot_change_session_model"
        );
        let turn = self
            .turn
            .clone()
            .ok_or_else(|| anyhow::Error::msg("model_change_requires_bound_turn"))?;
        let owner = self.coordinator.clone();
        let resolver = self.resolver.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let (control, payload) = self.coordinator.blocking(move || {
            resolver.authorize_context(&stream,&branch)?;
            let start=owner.store.lookup_receipt(&stream,&format!("turn-start:{turn}"))?
                .ok_or_else(||SessionError::StaleTarget("model change turn start missing".into()))?;
            let started=owner.store.read_event(start.event_ids.first().ok_or_else(||SessionError::StoreUnhealthy("model change receipt empty".into()))?)?
                .ok_or_else(||SessionError::StaleTarget("model change turn binding missing".into()))?;
            let EventData::TurnStarted {accepted_event_id,..}=started.body.data else {return Err(SessionError::InvalidAdmission("model change start kind mismatch".into()));};
            let accepted=owner.store.read_event(&accepted_event_id)?.ok_or_else(||SessionError::StaleTarget("model change ingress missing".into()))?;
            let EventData::TurnAccepted {ingress_ref,..}=accepted.body.data else {return Err(SessionError::InvalidAdmission("model change ingress kind mismatch".into()));};
            let payload=PayloadRecord::json("session.control/v1",&serde_json::json!({"command":"model_override","selection":selection,"source":"admitted_engine"})).map_err(AppendError::from)?;
            let mut control=SessionEvent::typed(stream.clone(),EventData::ControlInputRecorded {
                control_ref:StoredPayloadRef::from(&payload),expected_target_ref:TargetRef::Session {stream_id:stream},ingress_ref,
            },EventActor::System {component:"model_selection".into()}).map_err(AppendError::from)?;
            control.branch_id=Some(branch);control.turn_id=Some(turn);
            Ok((control,payload))
        }).await?;
        self.coordinator
            .record_model_change(control, vec![payload], outcome, self.resolver.clone())
            .await?;
        Ok(())
    }

    async fn begin_context_transform(
        &self,
        intent: zeroclaw_api::session_events::capture::ContextTransformIntent,
    ) -> anyhow::Result<Option<Arc<dyn zeroclaw_api::session_events::capture::ContextTransformPort>>>
    {
        Ok(Some(
            self.coordinator
                .begin_context_transform(
                    super::context::ContextTransformCommand {
                        stream: self.stream.clone(),
                        branch: self.branch.clone(),
                        turn: self.turn.clone(),
                        expected_projection: None,
                        intent,
                        execution_binding: None,
                    },
                    Arc::clone(&self.resolver),
                )
                .await?,
        ))
    }
    async fn admit(
        &self,
        intent: ProviderDispatchIntent,
    ) -> anyhow::Result<Arc<dyn ProviderObservationPort>> {
        // Requests contain complete replay history, including signed thinking
        // and opaque continuation. Ordinary messages have their own read view.
        let messages = PayloadRecord::json("session.reasoning/v1", &intent.messages)?;
        let tools = PayloadRecord::json("session.bytes/v1", &intent.tools)?;
        let request = PayloadRecord::json(
            "session.request/v1",
            &serde_json::json!({"messages_ref":StoredPayloadRef::from(&messages),"tools_ref":StoredPayloadRef::from(&tools),"temperature":intent.temperature,"thinking":intent.thinking,"tool_round_policy":intent.tool_round_policy}),
        )?;
        let route = PayloadRecord::json(
            "session.route/v1",
            &serde_json::json!({"requested":intent.requested_route,"dispatch_source":zeroclaw_api::session_events::capture::current_provider_route_source(),"actual_dispatch":{"provider_ref":intent.provider_ref,"model":intent.model}}),
        )?;
        let invocation = InvocationId::new();
        let mut prepared = SessionEvent::typed(
            self.stream.clone(),
            EventData::ProviderRequestPrepared {
                context_event_id: self.context_event_id.clone(),
                request_ref: StoredPayloadRef::from(&request),
                route_attribution_ref: StoredPayloadRef::from(&route),
                purpose: self.purpose.clone(),
            },
            EventActor::System {
                component: "provider_dispatch".into(),
            },
        )?;
        prepared.branch_id = Some(self.branch.clone());
        prepared.turn_id = self.turn.clone();
        prepared.causation_id = self.transform_event_id.clone();
        prepared.invocation_id = Some(invocation.clone());
        let mut started = SessionEvent::typed(
            self.stream.clone(),
            EventData::ProviderAttemptStarted {
                prepared_event_id: prepared.event_id.clone(),
                previous_attempt_event_id: intent.previous_attempt_event_id.clone(),
                idempotency_ref: None,
            },
            prepared.actor.clone(),
        )?;
        started.branch_id = prepared.branch_id.clone();
        started.turn_id = prepared.turn_id.clone();
        started.invocation_id = prepared.invocation_id.clone();
        started.causation_id = Some(prepared.event_id.clone());
        let coordinator = Arc::clone(&self.coordinator);
        let resolver = Arc::clone(&self.resolver);
        let mut payloads = vec![messages, tools, request, route];
        let branch = self.branch.clone();
        let turn = self.turn.clone();
        let stream = self.stream.clone();
        let observer = self
            .coordinator
            .blocking(move || {
                let lock = coordinator.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                coordinator.require_known_turn(&stream, &branch, turn.as_ref())?;
                let head = coordinator.store.head(&stream)?;
                if let EventData::ProviderRequestPrepared {
                    context_event_id, ..
                } = &mut prepared.body.data
                {
                    *context_event_id = coordinator.latest_context(&stream, &branch)?.event_id;
                }
                // Bind route origin to its canonical control event at dispatch.
                // Summary purpose is already on Prepared; do not copy it here.
                let history = coordinator.stream_history(&stream)?;
                let lineage = lineage::frozen_lineage(&history, &stream, &branch, head.sequence)?;
                let selection = history
                    .iter()
                    .rev()
                    .find(|event| {
                        lineage::includes(&lineage, event)
                            && match event.body.data {
                                EventData::SessionModelOverrideChanged {
                                    resolution: ModelResolution::Applied { .. },
                                } => true,
                                EventData::SessionModelOverrideChanged {
                                    resolution: ModelResolution::AppliedForTurn,
                                } => event.turn_id == turn,
                                _ => false,
                            }
                    })
                    .map(|event| event.event_id.clone());
                let route = payloads.last_mut().ok_or_else(|| {
                    SessionError::InvalidAdmission("route payload missing".into())
                })?;
                let mut body: serde_json::Value = serde_json::from_slice(&route.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                body["selection_event_id"] = serde_json::to_value(selection)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                *route = PayloadRecord::new(
                    route.payload_id.clone(),
                    route.codec.clone(),
                    canonical_json(&body).map_err(AppendError::from)?,
                )
                .map_err(AppendError::from)?;
                if let EventData::ProviderRequestPrepared {
                    route_attribution_ref,
                    ..
                } = &mut prepared.body.data
                {
                    *route_attribution_ref = StoredPayloadRef::from(&*route);
                }
                let key = format!("provider-start:{}", started.event_id);
                resolver.authorize(&EffectIntent {
                    stream_id: stream.clone(),
                    expected_head: head.clone(),
                    idempotency_key: key.clone(),
                    event: started.clone(),
                    payloads: payloads.clone(),
                })?;
                let started_event_id = started.event_id.clone();
                coordinator.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: head,
                    idempotency_key: key,
                    events: vec![prepared, started],
                    payloads,
                    erasure_reference_commitment: None,
                })?;
                let _ = coordinator.wakeups.send(stream.clone());
                Ok(EffectObservationHandle {
                    started_event_id,
                    coordinator,
                    stream_id: stream,
                    branch_id: Some(branch),
                    turn_id: turn,
                    invocation_id: invocation,
                })
            })
            .await?;
        Ok(Arc::new(observer))
    }
}

#[async_trait::async_trait]
impl ProviderObservationPort for EffectObservationHandle {
    fn started_event_id(&self) -> EventId {
        self.started_event_id.clone()
    }
    async fn observe(
        &self,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        if !matches!(
            data.kind(),
            EventKind::ProviderResponseProgressObserved
                | EventKind::ProviderAttemptCompleted
                | EventKind::ProviderAttemptFailed
                | EventKind::InvocationOutcomeUnknown
        ) {
            return Err(SessionError::InvalidAdmission(
                "provider observer cannot record another effect kind".into(),
            )
            .into());
        }
        let mut event = SessionEvent::typed(
            self.stream_id.clone(),
            data,
            EventActor::System {
                component: "provider_capture".into(),
            },
        )?;
        event.branch_id = self.branch_id.clone();
        event.turn_id = self.turn_id.clone();
        event.invocation_id = Some(self.invocation_id.clone());
        let coordinator = Arc::clone(&self.coordinator);
        let stream = self.stream_id.clone();
        let invocation = self.invocation_id.clone();
        Ok(self
            .coordinator
            .blocking(move || coordinator.append_observation(&stream, &invocation, event, payloads))
            .await?)
    }
}
