//! Context transforms preserve history and install a new derived projection.
//! Handles contain a frozen source cursor and identities, never live policy.
use super::engine::{EngineSession, decode_context_messages};
use super::query::{PayloadSelector, SessionAccess, SessionAccessPolicy};
use super::*;
use futures_util::FutureExt;
use zeroclaw_api::session_events::capture::SessionExecutionPort;

use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::session_events::capture::{
    ContextSnapshot, ContextTransformIntent, ContextTransformPort, ProviderAdmissionPort,
};
use zeroclaw_api::session_events::*;

struct Transform {
    owner: Arc<SessionCoordinator>,
    stream: SessionStreamId,
    branch: BranchId,
    turn: Option<TurnId>,
    input: EventId,
    started: EventId,
    intent: ContextTransformIntent,
    resolver: Arc<dyn PermissionResolver>,
}

pub struct ContextMaintenanceCommand {
    pub stream: SessionStreamId,
    pub branch: BranchId,
    pub input_projection: EventId,
    pub execution_binding: PayloadRecord,
    pub intent: ContextTransformIntent,
}

pub(super) struct ContextTransformCommand {
    pub stream: SessionStreamId,
    pub branch: BranchId,
    pub turn: Option<TurnId>,
    /// None selects the current projection inside the admitted turn. A result
    /// prepared outside that lane must name its exact source projection.
    pub expected_projection: Option<EventId>,
    pub intent: ContextTransformIntent,
    pub execution_binding: Option<PayloadRecord>,
}

impl SessionCoordinator {
    /// Idle local maintenance takes the same foreground lane as a turn. The
    /// automatic in-turn transform path below never reacquires that lane.
    pub async fn replace_context(
        self: &Arc<Self>,
        command: ContextMaintenanceCommand,
        snapshot: ContextSnapshot,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<(), SessionError> {
        let lane = self.foreground_lock(&command.stream);
        let _permit = lane.lock().await;
        let transform = self
            .begin_context_transform(
                ContextTransformCommand {
                    stream: command.stream,
                    branch: command.branch,
                    turn: None,
                    expected_projection: Some(command.input_projection),
                    intent: command.intent,
                    execution_binding: Some(command.execution_binding),
                },
                resolver,
            )
            .await?;
        transform
            .complete_snapshot(snapshot)
            .await
            .map_err(SessionError::Operation)
    }
    pub(super) fn latest_context(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
    ) -> Result<SessionEvent, SessionError> {
        let latest = self
            .branch_context(stream, branch)?
            .ok_or_else(|| SessionError::StaleTarget("context projection missing".into()))?;
        if latest.stream_id != *stream || latest.body.kind() != EventKind::ContextProjectionCreated
        {
            return Err(SessionError::InvalidAdmission(
                "context scope mismatch".into(),
            ));
        }
        Ok(latest)
    }

    pub(super) async fn begin_context_transform(
        self: &Arc<Self>,
        command: ContextTransformCommand,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Result<Arc<dyn ContextTransformPort>, SessionError> {
        let ContextTransformCommand {
            stream,
            branch,
            turn,
            expected_projection,
            intent,
            execution_binding,
        } = command;
        let owner = Arc::clone(self);
        self.blocking(move || {
            resolver.authorize_context(&stream, &branch)?;
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let input = owner.latest_context(&stream, &branch)?;
            if expected_projection.as_ref().is_some_and(|expected| *expected != input.event_id) {
                return Err(SessionError::StaleTarget("context changed before transform admission".into()));
            }
            let through = owner.store.head(&stream)?;
            let spec = PayloadRecord::json("session.transform/v1", &serde_json::json!({
                "reason_key":intent.reason_key, "algorithm_version":intent.algorithm_version, "budget":intent.budget,
            })).map_err(AppendError::from)?;
            let mut payloads = vec![spec.clone()];
            let binding = if let Some(turn) = &turn {
                owner.turn_execution_binding(&stream, &branch, turn)?
            } else if let Some(binding) = execution_binding {
                if binding.codec != "session.execution-binding/v1" {
                    return Err(SessionError::InvalidAdmission("maintenance binding codec mismatch".into()));
                }
                let reference = StoredPayloadRef::from(&binding);
                payloads.push(binding);
                reference
            } else {
                return Err(SessionError::InvalidAdmission("idle maintenance requires an explicit execution binding".into()));
            };
            let mut started = SessionEvent::typed(stream.clone(), EventData::ContextTransformStarted {
                input_projection_event_id:input.event_id.clone(),transform_spec_ref:StoredPayloadRef::from(&spec),execution_binding_ref:binding,
            },EventActor::System {component:"context_transform".into()}).map_err(AppendError::from)?;
            started.branch_id=Some(branch.clone());started.turn_id=turn.clone();
            let started_id=started.event_id.clone();
            owner.store.append_batch(AppendRequest {stream_id:stream.clone(),expected_head:through.clone(),idempotency_key:format!("context-transform:{started_id}"),events:vec![started],payloads,erasure_reference_commitment:None})?;
            let _=owner.wakeups.send(stream.clone());
            Ok(Arc::new(Transform {owner:Arc::clone(&owner),stream,branch,turn,input:input.event_id,started:started_id,intent,resolver}) as Arc<dyn ContextTransformPort>)
        }).await
    }
}

impl Transform {
    async fn settle_installation_result(&self, result: anyhow::Result<()>) -> anyhow::Result<()> {
        let Err(error) = result else {
            return Ok(());
        };
        // An uncertain append or a conflicting prior receipt cannot be replaced
        // with a newly inferred failure. Recovery must verify the original key.
        if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<SessionError>(),
                Some(SessionError::Append(
                    AppendError::CommitOutcomeUnknown(_) | AppendError::IdempotencyConflict(_)
                ))
            ) || matches!(
                cause.downcast_ref::<AppendError>(),
                Some(AppendError::CommitOutcomeUnknown(_) | AppendError::IdempotencyConflict(_))
            )
        }) {
            return Err(error);
        }
        let settlement = if error.chain().any(|cause| {
            matches!(
                cause.downcast_ref::<SessionError>(),
                Some(SessionError::StaleTarget(_))
            )
        }) {
            self.cancelled("context_input_no_longer_current".into())
                .await
        } else {
            self.failed(format!("{error:#}")).await
        };
        if let Err(settlement) = settlement {
            return Err(error.context(format!(
                "context_transform_settlement_failed: {settlement:#}"
            )));
        }
        Err(error)
    }

    async fn commit(
        &self,
        data: EventData,
        mut payloads: Vec<PayloadRecord>,
        install: bool,
        preceding: Vec<SessionEvent>,
    ) -> anyhow::Result<()> {
        let owner = Arc::clone(&self.owner);
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let turn = self.turn.clone();
        let started = self.started.clone();
        let input = self.input.clone();
        let resolver = Arc::clone(&self.resolver);
        self.owner
            .blocking(move || {
                if install {
                    resolver.authorize_context(&stream, &branch)?;
                }
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let mut event = SessionEvent::typed(
                    stream.clone(),
                    data,
                    EventActor::System {
                        component: "context_transform".into(),
                    },
                )
                .map_err(AppendError::from)?;
                event.branch_id = Some(branch);
                event.turn_id = turn;
                event.causation_id = Some(started.clone());
                let key = format!("context-transform-outcome:{started}");
                if let Some(receipt) = owner.store.lookup_receipt(&stream, &key)? {
                    let mut candidates = preceding.clone();
                    candidates.push(event.clone());
                    // Cancelled transforms may atomically settle outstanding
                    // providers before their own outcome. Those observations
                    // are derived once, not regenerated on a command retry.
                    let compared_ids = if preceding.is_empty()
                        && matches!(event.body.data, EventData::ContextTransformCancelled { .. })
                    {
                        receipt
                            .event_ids
                            .last()
                            .map(std::slice::from_ref)
                            .ok_or_else(|| {
                                SessionError::StoreUnhealthy(
                                    "transform cancellation receipt empty".into(),
                                )
                            })?
                    } else {
                        &receipt.event_ids
                    };
                    if compared_ids.len() != candidates.len() {
                        return Err(AppendError::IdempotencyConflict(key).into());
                    }
                    for (id, candidate) in compared_ids.iter().zip(candidates) {
                        let stored = owner.store.read_event(id)?.ok_or_else(|| {
                            SessionError::StaleTarget("transform receipt event missing".into())
                        })?;
                        if stored.branch_id != candidate.branch_id
                            || stored.turn_id != candidate.turn_id
                            || stored.causation_id != candidate.causation_id
                            || stored.actor != candidate.actor
                            || stored.body != candidate.body
                        {
                            return Err(AppendError::IdempotencyConflict(key).into());
                        }
                    }
                    return Ok(());
                }
                if install
                    && owner
                        .latest_context(
                            &stream,
                            event.branch_id.as_ref().ok_or_else(|| {
                                SessionError::InvalidAdmission("context branch missing".into())
                            })?,
                        )?
                        .event_id
                        != input
                {
                    return Err(SessionError::StaleTarget(
                        "context changed while transform was running".into(),
                    ));
                }
                let mut events =
                    if matches!(event.body.data, EventData::ContextTransformCancelled { .. }) {
                        let boundary = owner.store.read_event(&started)?.ok_or_else(|| {
                            SessionError::StoreUnhealthy(
                                "transform cancellation start missing".into(),
                            )
                        })?;
                        let (events, records) = owner.cancelled_provider_events(
                            &boundary,
                            &owner.stream_history(&stream)?,
                            None,
                        )?;
                        payloads.extend(records);
                        events
                    } else {
                        Vec::new()
                    };
                events.extend(preceding);
                events.push(event);
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: key,
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
}
#[async_trait::async_trait]
impl ContextTransformPort for Transform {
    async fn complete(&self, messages: Vec<ChatMessage>) -> anyhow::Result<()> {
        self.complete_snapshot(ContextSnapshot {
            messages: messages
                .into_iter()
                .map(zeroclaw_api::model_provider::ConversationMessage::Chat)
                .collect(),
            history_has_trim_breadcrumb: None,
        })
        .await
    }
    async fn complete_snapshot(&self, snapshot: ContextSnapshot) -> anyhow::Result<()> {
        let result: anyhow::Result<()> = async {
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let input_id = self.input.clone();
        let creation = self.started.clone();
        let turn = self.turn.clone();
        let messages = snapshot.messages;
        let (plan, preceding) = self
            .owner
            .blocking(move || {
                let input = owner
                    .store
                    .read_event(&input_id)?
                    .ok_or_else(|| SessionError::StaleTarget("transform input missing".into()))?;
                let mut plan =
                    owner.plan_context(&branch, &input, &creation, &messages, &[], &[])?;
                let preceding = if let Some(turn) = turn {
                    owner
                        .record_context_assistants(&stream, &branch, &turn, &creation, &mut plan)?
                } else {
                    vec![]
                };
                Ok((plan, preceding))
            })
            .await?;
        let context = retry::payload(
            &self.started,
            "context",
            "session.context/v1",
            &serde_json::json!({
                "message_refs":plan.refs, "history_has_trim_breadcrumb":snapshot.history_has_trim_breadcrumb,
            }),
        )?;
        let mut payloads = plan.payloads;
        payloads.push(context.clone());
        self.commit(
            EventData::ContextProjectionCreated {
                source_ranges: plan.ranges,
                summary_ref: plan.summary_ref,
                budget: self.intent.budget,
                reason_key: self.intent.reason_key.clone(),
                algorithm_version: self.intent.algorithm_version.clone(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: plan.summary_source_refs,
                transform_started_event_id: Some(self.started.clone()),
            },
            payloads,
            true,
            preceding,
        )
        .await
        }.await;
        self.settle_installation_result(result).await
    }
    async fn complete_summary(
        &self,
        snapshot: ContextSnapshot,
        summary: String,
    ) -> anyhow::Result<()> {
        use zeroclaw_api::model_provider::ConversationMessage;
        let result: anyhow::Result<()> = async {
        anyhow::ensure!(!summary.trim().is_empty(), "empty_context_summary");
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let input_id = self.input.clone();
        let started = self.started.clone();
        let (plan, summary_ref, sources) = self.owner.blocking(move || {
            let input = owner.store.read_event(&input_id)?.ok_or_else(|| SessionError::StaleTarget("summary input missing".into()))?;
            let history = owner.stream_history(&stream)?;
            let mut result = None;
            for event in &history {
                let EventData::ProviderAttemptCompleted { response_ref, .. } = &event.body.data else { continue; };
                let attempt = history.iter().find(|e| e.invocation_id == event.invocation_id && e.body.kind() == EventKind::ProviderRequestPrepared);
                if attempt.is_some_and(|e| e.causation_id.as_ref() == Some(&started)) { result = Some((event.clone(), response_ref.clone())); }
            }
            let (event, response_ref) = result.ok_or_else(|| SessionError::InvalidAdmission("summary requires captured provider result".into()))?;
            let PayloadRead::Present(record) = owner.store.read_payload(&response_ref.payload_id)? else { return Err(SessionError::StaleTarget("summary result unavailable".into())); };
            let response: ProviderResponsePayload = serde_json::from_slice(&record.body).map_err(|e| ReadError::Corrupt(e.to_string()))?;
            let mut actual = String::new(); let mut sources = Vec::new();
            for (block_index, block) in response.blocks.iter().enumerate() {
                if let ResponseBlock::Text { text_ref } = block {
                    let PayloadRead::Present(text) = owner.store.read_payload(&text_ref.payload_id)? else { return Err(SessionError::StaleTarget("summary text unavailable".into())); };
                    actual.push_str(std::str::from_utf8(&text.body).map_err(|e| ReadError::Corrupt(e.to_string()))?);
                    sources.push(ProviderBlockRef { result_event_id: event.event_id.clone(), block_index });
                }
            }
            if actual != summary { return Err(SessionError::InvalidAdmission("summary differs from captured output".into())); }
            let summary = ConversationMessage::Chat(ChatMessage::system(format!("[Context summary]\n{actual}")));
            // This normalized item creates a new MessageId, retaining actual
            // provider provenance in the projection rather than rewriting input.
            let mut messages = Vec::new(); let mut inserted = false; let mut summary_index = 0;
            let previous_summary_ref = match &input.body.data {EventData::ContextProjectionCreated {summary_ref,..} => summary_ref.as_ref(),_ => None};
            let previous_summary = if let Some(reference)=previous_summary_ref {
                let item = super::context_messages::context_refs(owner.store.as_ref(),&input)?.into_iter().find(|item|matches!(&item.source,ContextMessageSource::Conversation {payload_ref,..} if payload_ref==reference));
                item.as_ref().map(|item| super::context_messages::resolve_message(owner.store.as_ref(),&stream,item)).transpose()?.flatten()
            } else {None};
            let mut previous_removed=false;
            for message in snapshot.messages {
                if !previous_removed && matches!((&previous_summary, &message), (Some(ConversationMessage::Chat(old)), ConversationMessage::Chat(current)) if old.role == current.role && old.content == current.content) {previous_removed=true;continue;}
                if !inserted && !matches!(&message, ConversationMessage::Chat(chat) if chat.role == "system") {
                    summary_index = messages.len(); messages.push(summary.clone()); inserted = true;
                }
                messages.push(message);
            }
            if !inserted { summary_index = messages.len(); messages.push(summary); }
            let mut plan = owner.plan_context(&branch, &input, &started, &messages, &[], &[])?;
            for range in &mut plan.ranges { if range.disposition == RangeDisposition::Omitted { range.disposition = RangeDisposition::Summarized; } }
            let summary_payload = retry::payload(&started, "summary-message", "session.conversation/v1", &vec![messages[summary_index].clone()])?;
            if let ContextMessageSource::Conversation {payload_ref,..} = &plan.refs[summary_index].source {
                plan.payloads.retain(|payload| payload.payload_id != payload_ref.payload_id);
            }
            plan.refs[summary_index] = MessageRef {
                message_id: MessageId::parse(format!("context-message-{started}-{summary_index}")).map_err(AppendError::from)?,
                source: ContextMessageSource::Conversation {payload_ref:StoredPayloadRef::from(&summary_payload),item_index:0},
            };
            plan.payloads.push(summary_payload);
            let summary_ref = match &plan.refs[summary_index].source {
                ContextMessageSource::Conversation { payload_ref, .. } => payload_ref.clone(),
                ContextMessageSource::Content { content_ref, .. } => content_ref.clone(),
            };
            Ok((plan, summary_ref, sources))
        }).await?;
        let context = retry::payload(
            &self.started,
            "context",
            "session.context/v1",
            &serde_json::json!({
                "message_refs":plan.refs, "history_has_trim_breadcrumb":true,
            }),
        )?;
        let mut payloads = plan.payloads;
        payloads.push(context.clone());
        self.commit(
            EventData::ContextProjectionCreated {
                source_ranges: plan.ranges,
                summary_ref: Some(summary_ref),
                budget: self.intent.budget,
                reason_key: self.intent.reason_key.clone(),
                algorithm_version: self.intent.algorithm_version.clone(),
                result_ref: StoredPayloadRef::from(&context),
                summary_source_refs: sources,
                transform_started_event_id: Some(self.started.clone()),
            },
            payloads,
            true,
            vec![],
        )
        .await
        }.await;
        self.settle_installation_result(result).await
    }
    async fn failed(&self, diagnostic: String) -> anyhow::Result<()> {
        let error = retry::payload(
            &self.started,
            "error",
            "session.error-detail/v1",
            &serde_json::json!({"error_key":"local_context_transform_failed","detail":diagnostic}),
        )?;
        self.commit(
            EventData::ContextTransformFailed {
                started_event_id: self.started.clone(),
                error_ref: StoredPayloadRef::from(&error),
            },
            vec![error],
            false,
            vec![],
        )
        .await
    }
    async fn cancelled(&self, reason_key: String) -> anyhow::Result<()> {
        self.commit(
            EventData::ContextTransformCancelled {
                started_event_id: self.started.clone(),
                reason_key,
            },
            vec![],
            false,
            vec![],
        )
        .await
    }
    fn provider_capture(
        &self,
        purpose: ProviderPurpose,
    ) -> anyhow::Result<Arc<dyn ProviderAdmissionPort>> {
        anyhow::ensure!(
            purpose != ProviderPurpose::Main,
            "maintenance scope requires a maintenance purpose"
        );
        Ok(Arc::new(super::capture::ProviderCaptureScope {
            coordinator: self.owner.clone(),
            stream: self.stream.clone(),
            branch: self.branch.clone(),
            turn: self.turn.clone(),
            context_event_id: self.input.clone(),
            transform_event_id: Some(self.started.clone()),
            purpose,
            resolver: self.resolver.clone(),
        }))
    }
}

impl EngineSession {
    pub(super) async fn read_context_snapshot(
        &self,
        event: SessionEvent,
    ) -> anyhow::Result<Option<ContextSnapshot>> {
        let query = self.context_query();
        let read = |path| {
            query.read_payload(
                self.stream.clone(),
                PayloadSelector {
                    event_sequence: event.sequence,
                    reference_path: path,
                },
            )
        };
        let PayloadRead::Present(context) = read(vec![0]).await? else {
            anyhow::bail!("engine context projection has been erased");
        };
        let value: serde_json::Value = serde_json::from_slice(&context.body)?;
        let messages = if let Some(items) = value.get("message_refs") {
            let refs: Vec<MessageRef> = serde_json::from_value(items.clone())?;
            let owner = self.owner.clone();
            let stream = self.stream.clone();
            let authority = self.authority.clone();
            let branch = self.branch.clone();
            self.owner
                .blocking(move || {
                    authority.authorize_context(&stream, &branch)?;
                    let messages = refs
                        .iter()
                        .map(|item| {
                            super::context_messages::resolve_message(
                                owner.store.as_ref(),
                                &stream,
                                item,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let messages = messages.into_iter().flatten().collect();
                    authority.authorize_context(&stream, &branch)?;
                    Ok(messages)
                })
                .await?
        } else if value.get("messages_ref").is_some() {
            // Registered context references follow canonical object order:
            // an optional imported message view precedes messages_ref.
            let index = value
                .get("legacy_messages")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let PayloadRead::Present(bytes) = read(vec![0, index]).await? else {
                anyhow::bail!("engine historical context bytes have been erased");
            };
            decode_context_messages(&bytes.body)?
        } else if let Some(entries) = value
            .get("legacy_messages")
            .and_then(serde_json::Value::as_array)
        {
            let mut messages = Vec::with_capacity(entries.len());
            for (index, entry) in entries.iter().enumerate() {
                let PayloadRead::Present(content) = read(vec![0, index]).await? else {
                    anyhow::bail!("legacy engine content has been erased");
                };
                let content: ContentPayload = serde_json::from_slice(&content.body)?;
                let mut text = String::new();
                for (part, recorded) in content.parts.iter().enumerate() {
                    if matches!(recorded, RecordedContentPart::Text { .. }) {
                        let PayloadRead::Present(bytes) = read(vec![0, index, part]).await? else {
                            anyhow::bail!("legacy engine text has been erased");
                        };
                        text.push_str(std::str::from_utf8(&bytes.body)?);
                    } else {
                        anyhow::bail!("legacy engine context representation unsupported");
                    }
                }
                // Decode through the canonical message schema: imported
                // metadata and future typed fields must not be discarded.
                let mut message = entry.clone();
                message["content"] = text.into();
                messages.push(serde_json::from_value::<ChatMessage>(message)?.into_conversation());
            }
            messages
        } else {
            decode_context_messages(&serde_json::to_vec(
                value
                    .get("messages")
                    .ok_or_else(|| anyhow::Error::msg("engine context codec unsupported"))?,
            )?)?
        };
        Ok(Some(ContextSnapshot {
            messages,
            history_has_trim_breadcrumb: value
                .get("history_has_trim_breadcrumb")
                .and_then(serde_json::Value::as_bool),
        }))
    }
    pub(super) async fn summarize_frozen_context(
        &self,
        purpose: ProviderPurpose,
        summary: zeroclaw_api::session_events::capture::ContextSummaryExecutor<'_>,
    ) -> anyhow::Result<ContextSnapshot> {
        anyhow::ensure!(
            purpose != ProviderPurpose::Main,
            "summary_requires_maintenance_purpose"
        );
        let active = self.is_active();
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = if active {
            None
        } else {
            Some(lane.lock().await)
        };
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Write,
        )?;
        let input = self.latest_context().await?;
        let source = self
            .read_context_snapshot(input.clone())
            .await?
            .ok_or_else(|| anyhow::Error::msg("native_context_missing"))?;
        let turn = if active { self.active_turn()? } else { None };
        let transform = self
            .owner
            .begin_context_transform(
                super::context::ContextTransformCommand {
                    stream: self.stream.clone(),
                    branch: self.branch.clone(),
                    turn,
                    expected_projection: Some(input.event_id),
                    intent: ContextTransformIntent {
                        reason_key: if purpose == ProviderPurpose::Summary {
                            "model_context_summary"
                        } else {
                            "model_context_repair"
                        }
                        .into(),
                        algorithm_version: "model-summary/v1".into(),
                        budget: 0,
                    },
                    execution_binding: Some(self.authority.maintenance_binding()?),
                },
                self.authority.clone(),
            )
            .await?;
        let capture = transform.provider_capture(purpose)?;
        let (kept, messages) = super::context_summary::prepare(source);
        let result = std::panic::AssertUnwindSafe(
            zeroclaw_api::session_events::capture::scope_provider_capture(capture, async move {
                let text = summary(messages).await?;
                super::context_summary::validate(&text)?;
                Ok::<String, anyhow::Error>(text)
            }),
        )
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::Error::msg("summary_executor_panic")));
        match result {
            Ok(text) => {
                transform.complete_summary(kept, text).await?;
                self.restore_context()
                    .await?
                    .ok_or_else(|| anyhow::Error::msg("native_context_missing"))
            }
            Err(error) => {
                let settlement = if zeroclaw_api::turn_stop::is_tool_loop_cancelled(&error) {
                    transform
                        .cancelled("summary_cooperatively_stopped".into())
                        .await
                } else {
                    transform
                        .failed(
                            self.authority
                                .diagnostic(&error)
                                .unwrap_or_else(|| "summary_generation_failed".into()),
                        )
                        .await
                };
                if let Err(settlement) = settlement {
                    return Err(error.context(format!(
                        "summary_generation_settlement_failed: {settlement:#}"
                    )));
                }
                Err(error)
            }
        }
    }
}
