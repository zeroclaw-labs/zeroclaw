//! Narrow session commands. The read facade has no append capability.
use super::engine::EngineSession;
use super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::session_events::capture::{SessionExecutionPort, SessionMetadataObservation};
use zeroclaw_api::session_events::*;

#[cfg(test)]
mod tests;

impl EngineSession {
    pub(super) async fn control_file(
        &self,
        file: zeroclaw_api::session_events::capture::EngineFileInput,
        operation: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !file.file_id.is_empty() && !operation.is_empty(),
            "invalid_file_observation"
        );
        let metadata = PayloadRecord::json("session.locator/v1", &file.registration)?;
        let registration = PayloadRecord::json(
            "session.file-registration/v1",
            &serde_json::json!({"metadata_ref":StoredPayloadRef::from(&metadata),"representation_ref":StoredPayloadRef::from(&file.representation)}),
        )?;
        let evidence = PayloadRecord::json("session.evidence/v1", &file.policy_evidence)?;
        let operation = operation.to_owned();
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let actor = self.actor.clone();
        self.owner.blocking(move || {
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|p|p.into_inner());
            SessionAccessPolicy::authorize_actor(authority.as_ref(), &actor, &stream, SessionAccess::Write)?;
            authority.authorize_context(&stream,&branch)?;
            let key=format!("file-observation:{}:{operation}",file.file_id);
            if let Some(receipt)=owner.store.lookup_receipt(&stream,&key)? {
                let page=owner.store.read_page(&stream,&EventCursor {stream_id:stream.clone(),sequence:receipt.first.sequence-1},Some(&receipt.last),16,4*1024*1024)?;
                let previous=page.events.iter().find_map(|event|match &event.body.data {
                    EventData::FileUsed {representation_ref,..}=>Some(representation_ref),_=>None
                }).ok_or_else(||SessionError::StoreUnhealthy("file receipt missing rendition".into()))?;
                if previous.body_digest!=file.representation.body_digest || previous.codec!=file.representation.codec || previous.byte_len!=file.representation.byte_len {
                    return Err(SessionError::InvalidAdmission("file rendition retry conflict".into()));
                }
                return Ok(());
            }
            let mut events=Vec::new();
            let mut payloads=vec![file.representation.clone(),evidence.clone()];
            let mut cursor=EventCursor::beginning(stream.clone());
            let through=owner.store.head(&stream)?;
            let mut registered=false;
            loop {
                let page=owner.store.read_page(&stream,&cursor,Some(&through),256,4*1024*1024)?;
                registered |= page.events.iter().any(|event|matches!(&event.body.data,EventData::FileRegistered {file_id,..} if file_id==&file.file_id));
                if registered || !page.has_more {break;}
                cursor.sequence=page.events.last().ok_or_else(||SessionError::StoreUnhealthy("file scan empty page".into()))?.sequence;
            }
            if !registered {
                let mut event=SessionEvent::typed(stream.clone(),EventData::FileRegistered {file_id:file.file_id.clone(),registration_ref:StoredPayloadRef::from(&registration)},actor.clone()).map_err(AppendError::from)?;
                event.branch_id=Some(branch.clone());
                events.push(event);
                payloads.extend([registration,metadata]);
            }
            let mut used=SessionEvent::typed(stream.clone(),EventData::FileUsed {file_id:file.file_id,operation,representation_ref:StoredPayloadRef::from(&file.representation),policy_evidence_ref:StoredPayloadRef::from(&evidence)},actor).map_err(AppendError::from)?;
            used.branch_id=Some(branch);
            events.push(used);
            owner.store.append_batch(AppendRequest {stream_id:stream.clone(),expected_head:through,idempotency_key:key,events,payloads,erasure_reference_commitment:None})?;
            let _=owner.wakeups.send(stream);
            Ok(())
        }).await?;
        Ok(())
    }
    pub(super) async fn control_elicitation_requested(
        &self,
        request_id: &str,
        prompt: &str,
        schema: &serde_json::Value,
        expires_at: Timestamp,
    ) -> anyhow::Result<()> {
        let prompt = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            prompt.to_owned().into_bytes(),
        )?;
        let schema = PayloadRecord::json("session.bytes/v1", schema)?;
        let (control, mut payloads) = self.control_input(
            serde_json::json!({"command":"elicitation_request", "request_id":request_id}),
        )?;
        let mut event = SessionEvent::typed(
            self.stream.clone(),
            EventData::ElicitationRequested {
                request_id: request_id.into(),
                prompt_ref: StoredPayloadRef::from(&prompt),
                response_schema_ref: StoredPayloadRef::from(&schema),
                expires_at,
                session_incarnation: self.owner.execution_incarnation.clone(),
            },
            self.actor.clone(),
        )?;
        event.branch_id = Some(self.branch.clone());
        event.turn_id = self.active_turn()?;
        event.causation_id = Some(control.event_id.clone());
        payloads.extend([prompt, schema]);
        self.commit_control(vec![control, event], payloads).await
    }
    pub(super) async fn control_elicitation_resolved(
        &self,
        request_id: &str,
        resolution: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let payload = PayloadRecord::json("session.bytes/v1", resolution)?;
        let (control, mut payloads) = self.control_input(
            serde_json::json!({"command":"elicitation_resolution", "request_id":request_id}),
        )?;
        let mut event = SessionEvent::typed(
            self.stream.clone(),
            EventData::ElicitationResolved {
                request_id: request_id.into(),
                resolution_ref: StoredPayloadRef::from(&payload),
            },
            self.actor.clone(),
        )?;
        event.branch_id = Some(self.branch.clone());
        event.turn_id = self.active_turn()?;
        event.causation_id = Some(control.event_id.clone());
        payloads.push(payload);
        self.commit_control(vec![control, event], payloads).await
    }
    pub(super) async fn control_close(&self, reason_key: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!reason_key.is_empty(), "invalid_close_reason");
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = lane.lock().await;
        let (control, payloads) =
            self.control_input(serde_json::json!({"command":"close", "reason_key":reason_key}))?;
        let mut closed = SessionEvent::typed(
            self.stream.clone(),
            EventData::SessionClosed {
                reason_key: reason_key.into(),
            },
            self.actor.clone(),
        )?;
        closed.causation_id = Some(control.event_id.clone());
        self.commit_control(vec![control, closed], payloads).await
    }
    pub(super) async fn control_evidence(&self, value: serde_json::Value) -> anyhow::Result<()> {
        let (control, payloads) = self.control_input(value)?;
        self.commit_control(vec![control], payloads).await
    }

    /// Keep rejected approval replies in the owning session's append-only
    /// audit history without allowing them to participate in approval state.
    pub(super) async fn control_approval_reply_rejected(
        &self,
        request_id: Option<String>,
        classification: String,
        evidence: serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !classification.is_empty(),
            "approval_rejection_classification_missing"
        );
        let request_id = request_id.map(|value| value.chars().take(256).collect::<String>());
        let evidence = PayloadRecord::json("session.evidence/v1", &evidence)?;
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let actor = self.actor.clone();
        self.owner
            .blocking(move || {
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                SessionAccessPolicy::authorize_actor(
                    authority.as_ref(),
                    &actor,
                    &stream,
                    SessionAccess::Write,
                )?;
                authority.authorize_context(&stream, &branch)?;
                let mut event = SessionEvent::typed(
                    stream.clone(),
                    EventData::ApprovalReplyRejected {
                        request_id,
                        subject_reply_event_id: None,
                        classification,
                        evidence_ref: Some(StoredPayloadRef::from(&evidence)),
                    },
                    actor,
                )
                .map_err(AppendError::from)?;
                event.branch_id = Some(branch);
                let key = format!("approval-reply-rejected:{}", event.event_id);
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: key,
                    events: vec![event],
                    payloads: vec![evidence],
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(())
            })
            .await?;
        Ok(())
    }
    pub(super) async fn control_context_input(
        &self,
        input: zeroclaw_api::session_events::capture::EngineTurnInput,
        context_text: &str,
        reason_key: &str,
    ) -> anyhow::Result<MessageId> {
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = lane.lock().await;
        let command = self.input_command(input).await?;
        let outcome_key = format!("context-input-outcome:{}", command.turn_id);
        let query = self.context_query();
        // Receipt is an implementation detail of the authenticated command;
        // current authorization has already been resolved by input_command.
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let key = outcome_key.clone();
        let complete = self
            .owner
            .blocking(move || Ok(owner.store.lookup_receipt(&stream, &key)?))
            .await?;
        let mut admission = self
            .owner
            .admit_user_input(&command, &self.actor, &self.actor, self.authority.clone())
            .await?;
        let observed_context = context_text.to_owned();
        if let Some(receipt) = complete {
            let page = query
                .read_page(
                    self.stream.clone(),
                    EventCursor {
                        stream_id: self.stream.clone(),
                        sequence: receipt.first.sequence - 1,
                    },
                    Some(receipt.last),
                    8,
                    4 * 1024 * 1024,
                )
                .await?;
            let projection = page
                .events
                .iter()
                .find(|event| event.body.kind() == EventKind::ContextProjectionCreated)
                .ok_or_else(|| anyhow::Error::msg("context_input_projection_missing"))?;
            let PayloadRead::Present(structure) = query
                .read_payload(
                    self.stream.clone(),
                    super::query::PayloadSelector {
                        event_sequence: projection.sequence,
                        reference_path: vec![0],
                    },
                    64 * 1024 * 1024,
                )
                .await?
            else {
                anyhow::bail!("context_input_erased");
            };
            let value: serde_json::Value = serde_json::from_slice(&structure.body)?;
            let owner = self.owner.clone();
            let projection_copy = projection.clone();
            let stream = self.stream.clone();
            let messages = self
                .owner
                .blocking(move || {
                    super::context_messages::context_refs(owner.store.as_ref(), &projection_copy)?
                        .iter()
                        .map(|item| {
                            super::context_messages::resolve_message(
                                owner.store.as_ref(),
                                &stream,
                                item,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .map(|messages| messages.into_iter().flatten().collect::<Vec<_>>())
                })
                .await?;
            if !matches!(&projection.body.data,EventData::ContextProjectionCreated {reason_key:recorded,..} if recorded==reason_key)
                || !matches!(messages.last(), Some(zeroclaw_api::model_provider::ConversationMessage::Chat(message)) if message.role == "user" && message.content == observed_context)
                || value.get("message_refs").is_none()
            {
                return Err(AppendError::IdempotencyConflict(outcome_key).into());
            }
            admission.disarm();
            return Ok(command.message_id);
        }

        let prior = self.latest_context().await?;
        let mut snapshot = self
            .restore_context()
            .await?
            .ok_or_else(|| anyhow::Error::msg("session_context_missing"))?;
        snapshot
            .messages
            .push(zeroclaw_api::model_provider::ConversationMessage::Chat(
                ChatMessage::user(observed_context),
            ));
        let accepted = query
            .latest_event(
                self.stream.clone(),
                self.branch.clone(),
                Some(command.turn_id.clone()),
                EventKind::TurnAccepted,
            )
            .await?
            .ok_or_else(|| anyhow::Error::msg("context input admission missing"))?;
        let binding = command
            .input_payloads
            .iter()
            .find(|payload| payload.codec == "session.execution-binding/v1")
            .ok_or_else(|| anyhow::Error::msg("context input binding missing"))?
            .clone();
        let evidence = PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({"source":"authenticated_context_only_ingress","reason_key":reason_key}),
        )?;
        let spec = PayloadRecord::json(
            "session.transform/v1",
            &serde_json::json!({"reason_key":reason_key,"algorithm_version":"context-only-ingress/v1","budget":0}),
        )?;
        let completion = PayloadRecord::json(
            "session.turn-completion/v1",
            &TurnCompletion::Success {
                result_event_ids: vec![],
            },
        )?;
        let build = |data| -> Result<SessionEvent, ContractError> {
            let mut event = SessionEvent::typed(self.stream.clone(), data, self.actor.clone())?;
            event.branch_id = Some(self.branch.clone());
            event.turn_id = Some(command.turn_id.clone());
            event.causation_id = Some(accepted.event_id.clone());
            Ok(event)
        };
        let started = build(EventData::ContextTransformStarted {
            input_projection_event_id: prior.event_id.clone(),
            transform_spec_ref: StoredPayloadRef::from(&spec),
            execution_binding_ref: StoredPayloadRef::from(&binding),
        })?;
        let owner = self.owner.clone();
        let branch = self.branch.clone();
        let source = prior.clone();
        let creation = started.event_id.clone();
        let messages = snapshot.messages;
        let plan = self
            .owner
            .blocking(move || owner.plan_context(&branch, &source, &creation, &messages, &[], &[]))
            .await?;
        let context = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":plan.refs,"history_has_trim_breadcrumb":snapshot.history_has_trim_breadcrumb}),
        )?;
        let mut payloads = plan.payloads;
        payloads.extend([
            binding.clone(),
            evidence.clone(),
            spec,
            context.clone(),
            completion.clone(),
        ]);
        let mut projection = build(EventData::ContextProjectionCreated {
            source_ranges: plan.ranges,
            summary_ref: plan.summary_ref,
            budget: 0,
            reason_key: reason_key.into(),
            algorithm_version: "context-only-ingress/v1".into(),
            result_ref: StoredPayloadRef::from(&context),
            summary_source_refs: plan.summary_source_refs,
            transform_started_event_id: Some(started.event_id.clone()),
        })?;
        projection.causation_id = Some(started.event_id.clone());
        let events = vec![
            build(EventData::IngressDecisionRecorded {
                subject_event_id: accepted.event_id.clone(),
                decision: "context_only".into(),
                reason_key: reason_key.into(),
                policy_evidence_ref: StoredPayloadRef::from(&evidence),
            })?,
            started,
            projection,
            build(EventData::TurnCommitted {
                completion_ref: StoredPayloadRef::from(&completion),
            })?,
        ];
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let actor = self.actor.clone();
        let stream = self.stream.clone();
        let prior_id = prior.event_id;
        let branch = self.branch.clone();
        self.owner
            .blocking(move || {
                authority.authorize_actor(&actor, &stream, SessionAccess::Write)?;
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
                if owner
                    .branch_context(&stream, &branch)?
                    .is_none_or(|event| event.event_id != prior_id)
                {
                    return Err(SessionError::StaleTarget(
                        "context input projection changed".into(),
                    ));
                }
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: outcome_key,
                    events,
                    payloads,
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(())
            })
            .await?;
        admission.disarm();
        Ok(command.message_id)
    }
    pub(super) fn control_input(
        &self,
        value: serde_json::Value,
    ) -> anyhow::Result<(SessionEvent, Vec<PayloadRecord>)> {
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Write,
        )?;
        let binding = self.authority.maintenance_binding()?;
        let control = PayloadRecord::json("session.control/v1", &value)?;
        let ingress = PayloadRecord::json(
            "session.ingress/v1",
            &serde_json::json!({"source":"authenticated_session_command", "execution_binding_ref":StoredPayloadRef::from(&binding)}),
        )?;
        let mut event = SessionEvent::typed(
            self.stream.clone(),
            EventData::ControlInputRecorded {
                control_ref: StoredPayloadRef::from(&control),
                expected_target_ref: TargetRef::Session {
                    stream_id: self.stream.clone(),
                },
                ingress_ref: StoredPayloadRef::from(&ingress),
            },
            self.actor.clone(),
        )?;
        event.branch_id = Some(self.branch.clone());
        Ok((event, vec![binding, control, ingress]))
    }
    async fn commit_control(
        &self,
        mut events: Vec<SessionEvent>,
        mut payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<()> {
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let principal = self.actor.clone();
        let stream = self.stream.clone();
        self.owner
            .blocking(move || {
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                SessionAccessPolicy::authorize_actor(
                    authority.as_ref(),
                    &principal,
                    &stream,
                    SessionAccess::Write,
                )?;
                let key = events
                    .first()
                    .ok_or_else(|| SessionError::InvalidAdmission("control batch empty".into()))?
                    .event_id
                    .to_string();
                let supplied_binding = payloads
                    .iter()
                    .find(|payload| payload.codec == "session.execution-binding/v1")
                    .cloned();
                let binding_is_referenced = supplied_binding.is_some();
                let binding =
                    supplied_binding.map_or_else(|| authority.maintenance_binding(), Ok)?;
                if let Some(revision) = owner.authority_observation(
                    &stream,
                    &events[0]
                        .branch_id
                        .clone()
                        .ok_or(SessionError::InvocationMismatch)?,
                    &principal,
                    &binding,
                )? {
                    events.push(revision);
                    if !binding_is_referenced {
                        payloads.push(binding);
                    }
                }
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: format!("session-control:{key}"),
                    events,
                    payloads,
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(())
            })
            .await?;
        Ok(())
    }
    pub(super) async fn control_rename(&self, title: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            !title.trim().is_empty() && title.chars().count() <= 200,
            "invalid_session_title"
        );
        let value = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            title.to_owned().into_bytes(),
        )?;
        let (control, mut payloads) = self.control_input(
            serde_json::json!({"command":"rename", "value_ref":StoredPayloadRef::from(&value)}),
        )?;
        let mut changed = SessionEvent::typed(
            self.stream.clone(),
            EventData::SessionMetadataChanged {
                field: "title".into(),
                new_value_ref: StoredPayloadRef::from(&value),
            },
            control.actor.clone(),
        )?;
        changed.branch_id = Some(self.branch.clone());
        changed.causation_id = Some(control.event_id.clone());
        payloads.push(value);
        self.commit_control(vec![control, changed], payloads).await
    }
    pub(super) async fn control_metadata(
        &self,
        value: SessionMetadataObservation,
    ) -> anyhow::Result<()> {
        let (field, codec, value) = match value {
            SessionMetadataObservation::ContextUsage { input_tokens } => (
                "token_count",
                "session.authority/v1",
                serde_json::json!(input_tokens.unwrap_or(0)),
            ),
            SessionMetadataObservation::Plan { entries } => {
                ("plan", "session.bytes/v1", (serde_json::to_value(entries)?))
            }
        };
        let value = PayloadRecord::json(codec, &value)?;
        let mut changed = SessionEvent::typed(
            self.stream.clone(),
            EventData::SessionMetadataChanged {
                field: field.into(),
                new_value_ref: StoredPayloadRef::from(&value),
            },
            self.actor.clone(),
        )?;
        changed.branch_id = Some(self.branch.clone());
        self.commit_control(vec![changed], vec![value]).await
    }
    pub(super) async fn control_append(&self, text: &str) -> anyhow::Result<MessageId> {
        anyhow::ensure!(!text.trim().is_empty(), "empty_assistant_content");
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = lane.lock().await;
        let input = self.latest_context().await?;
        let mut snapshot = self
            .restore_context()
            .await?
            .ok_or_else(|| anyhow::Error::msg("session_context_missing"))?;
        let text = text.to_owned();
        snapshot
            .messages
            .push(zeroclaw_api::model_provider::ConversationMessage::Chat(
                ChatMessage::assistant(&text),
            ));
        let id = MessageId::new();
        let leaf = PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            text.into_bytes(),
        )?;
        let content = PayloadRecord::json(
            "session.content/v1",
            &ContentPayload {
                message_id: id.clone(),
                parts: vec![RecordedContentPart::Text {
                    text_ref: StoredPayloadRef::from(&leaf),
                }],
            },
        )?;
        let (control, mut payloads) = self.control_input(serde_json::json!({"command":"append_assistant", "content_ref":StoredPayloadRef::from(&content)}))?;
        let mut recorded = SessionEvent::typed(
            self.stream.clone(),
            EventData::AssistantContentRecorded {
                content_ref: StoredPayloadRef::from(&content),
                provenance_refs: vec![],
            },
            control.actor.clone(),
        )?;
        recorded.branch_id = Some(self.branch.clone());
        recorded.causation_id = Some(control.event_id.clone());
        let binding_ref = StoredPayloadRef::from(
            payloads
                .iter()
                .find(|payload| payload.codec == "session.execution-binding/v1")
                .ok_or_else(|| anyhow::Error::msg("control execution binding missing"))?,
        );
        let spec = PayloadRecord::json(
            "session.transform/v1",
            &serde_json::json!({"reason_key":"operator_message_append", "algorithm_version":"session-command/v1", "budget":0}),
        )?;
        let mut started = SessionEvent::typed(
            self.stream.clone(),
            EventData::ContextTransformStarted {
                input_projection_event_id: input.event_id.clone(),
                transform_spec_ref: StoredPayloadRef::from(&spec),
                execution_binding_ref: binding_ref,
            },
            control.actor.clone(),
        )?;
        started.branch_id = Some(self.branch.clone());
        started.causation_id = Some(control.event_id.clone());
        let owner = self.owner.clone();
        let branch = self.branch.clone();
        let prior = input.clone();
        let creation = started.event_id.clone();
        let extra = recorded.clone();
        let extra_payloads = vec![content.clone(), leaf.clone()];
        let messages = snapshot.messages;
        let plan = self
            .owner
            .blocking(move || {
                owner.plan_context(
                    &branch,
                    &prior,
                    &creation,
                    &messages,
                    &[extra],
                    &extra_payloads,
                )
            })
            .await?;
        let context = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({
                "message_refs":plan.refs, "history_has_trim_breadcrumb":snapshot.history_has_trim_breadcrumb,
            }),
        )?;
        let mut projection = SessionEvent::typed(
            self.stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![SourceRange {
                    stream_id: self.stream.clone(),
                    from_seq: input.sequence,
                    to_seq: input.sequence,
                    disposition: RangeDisposition::Included,
                }],
                summary_ref: plan.summary_ref,
                budget: 0,
                reason_key: "operator_message_append".into(),
                algorithm_version: "session-command/v1".into(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: plan.summary_source_refs,
                transform_started_event_id: Some(started.event_id.clone()),
            },
            control.actor.clone(),
        )?;
        projection.branch_id = Some(self.branch.clone());
        projection.causation_id = Some(started.event_id.clone());
        payloads.extend([leaf, content, spec, context]);
        payloads.extend(plan.payloads);
        self.commit_control(vec![control, recorded, started, projection], payloads)
            .await?;
        Ok(id)
    }
    pub(super) async fn control_cancellation(
        &self,
        turn: Option<&str>,
        incarnation: &str,
        reason_key: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !incarnation.is_empty() && !reason_key.is_empty(),
            "invalid_cancellation_target"
        );
        let turn = turn.map(TurnId::parse).transpose()?;
        let target = turn.as_ref().map_or_else(
            || TargetRef::Session {
                stream_id: self.stream.clone(),
            },
            |turn| TargetRef::Turn {
                turn_id: turn.clone(),
            },
        );
        let (control, payloads) = self.control_input(serde_json::json!({"command":"cancel", "target_turn_id":turn,"expected_incarnation":incarnation}))?;
        let mut requested = SessionEvent::typed(
            self.stream.clone(),
            EventData::CancellationRequested {
                target_ref: target,
                expected_incarnation: incarnation.into(),
                reason_key: reason_key.into(),
            },
            control.actor.clone(),
        )?;
        requested.branch_id = Some(self.branch.clone());
        requested.turn_id = turn;
        requested.causation_id = Some(control.event_id.clone());
        self.commit_control(vec![control, requested], payloads)
            .await
    }
}
