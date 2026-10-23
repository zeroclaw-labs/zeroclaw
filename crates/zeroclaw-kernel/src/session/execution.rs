//! Turn admission and execution sequencing. The supplied executor is the
//! existing engine; this module creates no alternate model/tool loop.
use super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use futures_util::FutureExt;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::capture::{
    EngineInputPart, ProviderAdmissionPort, scope_provider_capture,
};
use zeroclaw_api::session_events::projection::{HistoryProjection, InvocationStatus};
use zeroclaw_api::session_events::{
    ContentPayload, ContextMessageSource, Digest, EventData, EventId, MessageId, PayloadId,
    PayloadRead, RecordedContentPart, StoredPayloadRef, TurnCompletion, canonical_json,
};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct UserTurnCommand {
    pub stream_id: SessionStreamId,
    pub branch_id: BranchId,
    pub turn_id: TurnId,
    pub message_id: MessageId,
    pub ingress_key: String,
    pub parts: Vec<EngineInputPart>,
    pub ingress_ref: StoredPayloadRef,
    pub reply_route_ref: Option<StoredPayloadRef>,
    pub execution_binding_ref: StoredPayloadRef,
    pub context_event_id: EventId,
    /// Newly created ingress/binding payloads travel in the same atomic input
    /// batch. Existing references are resolved by the store at admission.
    pub input_payloads: Vec<PayloadRecord>,
}

/// An adapter implements this with the existing Agent/turn executor. Adapters
/// resolve their own capabilities; kernel never constructs providers or tools.
#[async_trait::async_trait]
pub trait TurnExecutorPort: Send {
    /// The host owns explicit error-chain formatting. Kernel
    /// persists only this explicit diagnostic, never arbitrary panic data.
    fn diagnostic(&self, _error: &anyhow::Error) -> Option<String> {
        None
    }
    async fn execute(
        &mut self,
        text: &str,
        admission: Arc<dyn ProviderAdmissionPort>,
    ) -> anyhow::Result<String>;
}

/// A dropped queued waiter owns cancellation of its accepted, unstarted turn.
/// The existing coordinator worker completes the short settlement; it never
/// polls the executor and checks canonical start/outcome before appending.
pub(super) struct QueuedTurnGuard {
    owner: Arc<SessionCoordinator>,
    command: UserTurnCommand,
    accepted: EventId,
    armed: bool,
}
impl QueuedTurnGuard {
    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
    #[cfg(test)]
    pub(super) fn detach_for_recovery(mut self) -> EventId {
        self.disarm();
        self.accepted.clone()
    }
}
impl Drop for QueuedTurnGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let owner = self.owner.clone();
            let command = self.command.clone();
            let accepted = self.accepted.clone();
            runtime.spawn(async move {
                if let Err(error) = owner
                    .cancel_unstarted_turn(&command, &accepted, "turn_waiter_dropped_before_start")
                    .await
                {
                    ::zeroclaw_log::record!(
                        ERROR,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                            .with_attrs(serde_json::json!({"error":error.to_string()})),
                        "queued_turn_settlement_failed"
                    );
                }
            });
        }
    }
}

impl SessionCoordinator {
    pub(super) fn foreground_lock(&self, stream: &SessionStreamId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .foreground
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, value| value.strong_count() > 0);
        if let Some(lock) = locks.get(stream).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        locks.insert(stream.clone(), Arc::downgrade(&lock));
        lock
    }

    /// One admitted user turn. Control/approval commands never take this
    /// foreground permit; commit locks are released before engine awaits.
    pub async fn execute_user_turn(
        self: &Arc<Self>,
        command: UserTurnCommand,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
        resolver: Arc<dyn PermissionResolver>,
        executor: &mut dyn TurnExecutorPort,
    ) -> Result<AppendReceipt, SessionError> {
        self.execute_turn(
            command,
            EventActor::Principal {
                principal_id: principal.0,
            },
            policy,
            resolver,
            executor,
        )
        .await
    }
    pub async fn execute_turn(
        self: &Arc<Self>,
        command: UserTurnCommand,
        actor: EventActor,
        policy: Arc<dyn SessionAccessPolicy>,
        resolver: Arc<dyn PermissionResolver>,
        executor: &mut dyn TurnExecutorPort,
    ) -> Result<AppendReceipt, SessionError> {
        policy.authorize_actor(&actor, &command.stream_id, SessionAccess::Write)?;
        // An ordering refusal must not cancel an already recovered waiter or
        // admit newer work ahead of the persisted recovery queue.
        self.requeue_for_dispatch(
            command.stream_id.clone(),
            command.branch_id.clone(),
            command.turn_id.clone(),
        )
        .await?;
        let principal = actor.clone();
        let mut queued = self
            .admit_user_input(&command, &actor, &actor, Arc::clone(&policy))
            .await?;
        let accepted_id = queued.accepted.clone();
        let lock = self.foreground_lock(&command.stream_id);
        let _permit = lock.lock().await;
        if let Err(error) =
            policy.authorize_actor(&principal, &command.stream_id, SessionAccess::Write)
        {
            self.cancel_unstarted_turn(
                &command,
                &accepted_id,
                "turn_permission_revoked_before_start",
            )
            .await?;
            return Err(error);
        }
        let store = Arc::clone(&self.store);
        let stream = command.stream_id.clone();
        let turn = command.turn_id.clone();
        let (outcome, started, context_only) = self
            .blocking(move || {
                Ok((
                    store.lookup_receipt(&stream, &format!("turn-outcome:{turn}"))?,
                    store.lookup_receipt(&stream, &format!("turn-start:{turn}"))?,
                    store.lookup_receipt(&stream, &format!("context-input-outcome:{turn}"))?,
                ))
            })
            .await?;
        if context_only.is_some() {
            return Err(SessionError::StaleTarget(
                "context-only input cannot become an engine turn".into(),
            ));
        }
        if let Some(receipt) = outcome {
            return Ok(receipt);
        }
        let requeued = self
            .requeue_for_dispatch(
                command.stream_id.clone(),
                command.branch_id.clone(),
                command.turn_id.clone(),
            )
            .await?;
        if started.is_some() && requeued.is_none() {
            return Err(SessionError::StaleTarget(
                "started turn requires verified recovery; retry cannot re-execute effects".into(),
            ));
        }
        let (data, start_key) = if started.is_some() {
            let recovered = requeued.as_ref().ok_or(SessionError::InvocationMismatch)?;
            let EventData::RecoveryObserved {
                prior_execution_ref,
                evidence_ref,
                ..
            } = &recovered.body.data
            else {
                return Err(SessionError::InvocationMismatch);
            };
            (
                EventData::RecoveryObserved {
                    prior_execution_ref: prior_execution_ref.clone(),
                    through_cursor: EventCursor {
                        stream_id: command.stream_id.clone(),
                        sequence: recovered.sequence,
                    },
                    disposition: zeroclaw_api::session_events::RecoveryDisposition::Rescheduled {
                        execution_binding_ref: command.execution_binding_ref.clone(),
                    },
                    evidence_ref: evidence_ref.clone(),
                },
                format!("turn-reschedule:{}", recovered.event_id),
            )
        } else {
            (
                EventData::TurnStarted {
                    accepted_event_id: accepted_id.clone(),
                    execution_binding_ref: command.execution_binding_ref.clone(),
                },
                format!("turn-start:{}", command.turn_id),
            )
        };
        let mut started = SessionEvent::typed(command.stream_id.clone(), data, actor.clone())
            .map_err(AppendError::from)?;
        started.branch_id = Some(command.branch_id.clone());
        started.turn_id = Some(command.turn_id.clone());
        started.causation_id = requeued.as_ref().map(|event| event.event_id.clone());
        let context_id = command.context_event_id.clone();
        let start_result = self
            .append_current(
                command.stream_id.clone(),
                start_key,
                vec![started],
                command
                    .input_payloads
                    .iter()
                    .filter(|payload| {
                        StoredPayloadRef::from(*payload) == command.execution_binding_ref
                    })
                    .cloned()
                    .collect(),
                principal.clone(),
                Arc::clone(&policy),
            )
            .await;
        if let Err(error) = start_result {
            if matches!(error, SessionError::PermissionDenied(_)) {
                self.cancel_unstarted_turn(
                    &command,
                    &accepted_id,
                    "turn_permission_revoked_before_start",
                )
                .await?;
            }
            return Err(error);
        }
        queued.armed = false;
        let admission = self.provider_capture(
            command.stream_id.clone(),
            command.branch_id.clone(),
            command.turn_id.clone(),
            context_id,
            Arc::clone(&resolver),
        );
        let tool_admission = self.tool_capture(
            command.stream_id.clone(),
            command.branch_id.clone(),
            command.turn_id.clone(),
            resolver,
        );
        let text = command
            .parts
            .iter()
            .filter_map(|part| match part {
                EngineInputPart::Text(text) => Some(text.as_str()),
                EngineInputPart::File(_) | EngineInputPart::Opaque { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let result = std::panic::AssertUnwindSafe(scope_provider_capture(
            Arc::clone(&admission),
            zeroclaw_api::session_events::capture::scope_tool_capture(
                tool_admission,
                executor.execute(&text, admission),
            ),
        ))
        .catch_unwind()
        .await
        .unwrap_or_else(|_payload| Err(anyhow::Error::msg("engine_executor_panic")));
        let diagnostic = result
            .as_ref()
            .err()
            .and_then(|error| executor.diagnostic(error));
        self.finish_execution(command, actor, result, diagnostic)
            .await
    }

    async fn cancel_unstarted_turn(
        self: &Arc<Self>,
        command: &UserTurnCommand,
        accepted: &EventId,
        reason: &str,
    ) -> Result<(), SessionError> {
        let owner = self.clone();
        let command = command.clone();
        let accepted = accepted.clone();
        let reason = reason.to_owned();
        self.blocking(move || {
            let lock = owner.lock_for(&command.stream_id);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if owner
                .store
                .lookup_receipt(
                    &command.stream_id,
                    &format!("turn-outcome:{}", command.turn_id),
                )?
                .is_some()
                || owner
                    .store
                    .lookup_receipt(
                        &command.stream_id,
                        &format!("context-input-outcome:{}", command.turn_id),
                    )?
                    .is_some()
            {
                return Ok(());
            }
            if owner
                .store
                .lookup_receipt(
                    &command.stream_id,
                    &format!("turn-start:{}", command.turn_id),
                )?
                .is_some()
            {
                return Ok(());
            }
            let admission = owner
                .store
                .read_event(&accepted)?
                .ok_or_else(|| SessionError::StoreUnhealthy("accepted turn missing".into()))?;
            if admission.stream_id != command.stream_id
                || admission.turn_id.as_ref() != Some(&command.turn_id)
                || admission.body.kind() != EventKind::TurnAccepted
            {
                return Err(SessionError::InvalidAdmission(
                    "unstarted settlement admission mismatch".into(),
                ));
            }
            let mut terminal = SessionEvent::typed(
                command.stream_id.clone(),
                EventData::TurnCancelled {
                    reason_key: reason,
                    unresolved_invocation_ids: vec![],
                },
                EventActor::System {
                    component: "turn_admission".into(),
                },
            )
            .map_err(AppendError::from)?;
            terminal.branch_id = Some(command.branch_id);
            terminal.turn_id = Some(command.turn_id.clone());
            terminal.causation_id = Some(accepted);
            owner.store.append_batch(AppendRequest {
                stream_id: command.stream_id.clone(),
                expected_head: owner.store.head(&command.stream_id)?,
                idempotency_key: format!("turn-outcome:{}", command.turn_id),
                events: vec![terminal],
                payloads: vec![],
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(command.stream_id);
            Ok(())
        })
        .await
    }

    pub(super) async fn admit_user_input(
        self: &Arc<Self>,
        command: &UserTurnCommand,
        actor: &EventActor,
        principal: &EventActor,
        policy: Arc<dyn SessionAccessPolicy>,
    ) -> Result<QueuedTurnGuard, SessionError> {
        let owner = Arc::clone(self);
        let command = command.clone();
        let actor = actor.clone();
        let principal = principal.clone();
        self.blocking(move || {
            policy.authorize_actor(&principal, &command.stream_id, SessionAccess::Write)?;
            let lock = owner.lock_for(&command.stream_id);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(receipt) = owner
                .store
                .lookup_receipt(&command.stream_id, &command.ingress_key)?
            {
                let page = owner.store.read_page(
                    &command.stream_id,
                    &EventCursor {
                        stream_id: command.stream_id.clone(),
                        sequence: receipt.first.sequence - 1,
                    },
                    Some(&receipt.first),
                    1,
                    4 * 1024 * 1024,
                )?;
                let accepted = page
                    .events
                    .first()
                    .ok_or_else(|| ReadError::Gap("accepted input absent".into()))?;
                let EventData::TurnAccepted {
                    input_ref,
                    ingress_ref,
                    reply_route_ref,
                } = &accepted.body.data
                else {
                    return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                };
                if accepted.turn_id.as_ref() != Some(&command.turn_id)
                    || accepted.branch_id.as_ref() != Some(&command.branch_id)
                    || accepted.actor != actor
                    || *ingress_ref != command.ingress_ref
                    || *reply_route_ref != command.reply_route_ref
                {
                    return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                }
                let PayloadRead::Present(content) =
                    owner.store.read_payload(&input_ref.payload_id)?
                else {
                    return Err(SessionError::StaleTarget(
                        "accepted input has been erased".into(),
                    ));
                };
                let content: ContentPayload = serde_json::from_slice(&content.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                if content.message_id != command.message_id || content.parts.len() != command.parts.len() {
                    return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                }
                for (recorded, supplied) in content.parts.iter().zip(&command.parts) {
                    match (recorded, supplied) {
                        (RecordedContentPart::Text { text_ref }, EngineInputPart::Text(text)) => {
                            let PayloadRead::Present(leaf) = owner.store.read_payload(&text_ref.payload_id)? else {
                                return Err(SessionError::StaleTarget("accepted input has been erased".into()));
                            };
                            if leaf.body != text.as_bytes() {
                                return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                            }
                        }
                        (RecordedContentPart::File { file_id }, EngineInputPart::File(file)) if file_id == &file.file_id => {}
                        (RecordedContentPart::Opaque {type_name,body_ref}, EngineInputPart::Opaque {type_name:expected,body}) if type_name==expected => {
                            let PayloadRead::Present(leaf)=owner.store.read_payload(&body_ref.payload_id)? else {return Err(SessionError::StaleTarget("accepted opaque part unavailable".into()));};
                            if leaf.body!=canonical_json(&body.clone()).map_err(AppendError::from)? {return Err(AppendError::IdempotencyConflict(command.ingress_key).into());}
                        }
                        _ => return Err(AppendError::IdempotencyConflict(command.ingress_key).into()),
                    }
                }
                let files: Vec<_> = command.parts.iter().filter_map(|part| match part {
                    EngineInputPart::File(file) => Some(file),
                    EngineInputPart::Text(_) | EngineInputPart::Opaque {..} => None,
                }).collect();
                if !files.is_empty() {
                    let recorded = owner.store.read_page(&command.stream_id,
                        &EventCursor {stream_id:command.stream_id.clone(),sequence:receipt.first.sequence-1},
                        Some(&receipt.last), 10_000, 64*1024*1024)?;
                    if recorded.has_more { return Err(ReadError::TooLarge.into()); }
                    let uses: Vec<_> = recorded.events.iter().filter_map(|event| {
                        if let EventData::FileUsed { file_id, representation_ref, policy_evidence_ref, .. } = &event.body.data {
                            Some((file_id, representation_ref, policy_evidence_ref))
                        } else { None }
                    }).collect();
                    if uses.len() != files.len() {
                        return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                    }
                    for (file, (file_id, representation, evidence)) in files.iter().zip(uses) {
                        file.representation.validate().map_err(AppendError::from)?;
                        let PayloadRead::Present(policy) = owner.store.read_payload(&evidence.payload_id)? else {
                            return Err(SessionError::StaleTarget("accepted file policy unavailable".into()));
                        };
                        let policy: serde_json::Value = serde_json::from_slice(&policy.body)
                            .map_err(|error|ReadError::Corrupt(error.to_string()))?;
                        let metadata: StoredPayloadRef = serde_json::from_value(policy["observed_file_metadata_ref"].clone())
                            .map_err(|error|ReadError::Corrupt(error.to_string()))?;
                        let expected_metadata = canonical_json(&file.registration.clone())
                            .map_err(AppendError::from)?;
                        if file_id != &file.file_id || representation != &StoredPayloadRef::from(&file.representation)
                            || metadata.body_digest != Digest::from_bytes(&expected_metadata)
                            || policy.get("policy") != Some(&file.policy_evidence.clone()) {
                            return Err(AppendError::IdempotencyConflict(command.ingress_key).into());
                        }
                    }
                }
                return Ok(QueuedTurnGuard {
                    owner: owner.clone(),
                    command,
                    accepted: accepted.event_id.clone(),
                    // An idempotent waiter does not own the original input.
                    armed: false,
                });
            }
            let mut leaves = Vec::new();
            let mut parts = Vec::with_capacity(command.parts.len());
            let mut files = Vec::new();
            for (index, part) in command.parts.iter().enumerate() {
                match part {
                    EngineInputPart::Text(text) => {
                        let leaf = PayloadRecord::new(PayloadId::new(), "session.text/v1".into(), text.as_bytes().to_vec())
                            .map_err(AppendError::from)?;
                        parts.push(RecordedContentPart::Text { text_ref: StoredPayloadRef::from(&leaf) });
                        leaves.push(leaf);
                    }
                    EngineInputPart::File(file) => {
                        parts.push(RecordedContentPart::File { file_id: file.file_id.clone() });
                        files.push((index, file.clone()));
                    }
                    EngineInputPart::Opaque {type_name,body} => {
                        let leaf=PayloadRecord::json("session.bytes/v1",&body.clone()).map_err(AppendError::from)?;
                        parts.push(RecordedContentPart::Opaque {type_name:type_name.clone(),body_ref:StoredPayloadRef::from(&leaf)});
                        leaves.push(leaf);
                    }
                }
            }
            let content = PayloadRecord::json(
                "session.content/v1",
                &ContentPayload {
                    message_id: command.message_id.clone(),
                    parts,
                },
            )
            .map_err(AppendError::from)?;
            let mut accepted = SessionEvent::typed(
                command.stream_id.clone(),
                EventData::TurnAccepted {
                    input_ref: StoredPayloadRef::from(&content),
                    ingress_ref: command.ingress_ref.clone(),
                    reply_route_ref: command.reply_route_ref.clone(),
                },
                actor.clone(),
            )
            .map_err(AppendError::from)?;
            accepted.branch_id = Some(command.branch_id.clone());
            accepted.turn_id = Some(command.turn_id.clone());
            let accepted_id = accepted.event_id.clone();
            let mut recorded = SessionEvent::typed(
                command.stream_id.clone(),
                EventData::UserContentRecorded {
                    accepted_event_id: accepted_id.clone(),
                    content_ref: StoredPayloadRef::from(&content),
                },
                actor.clone(),
            )
            .map_err(AppendError::from)?;
            recorded.branch_id = accepted.branch_id.clone();
            recorded.turn_id = accepted.turn_id.clone();
            recorded.causation_id = Some(accepted_id.clone());
            let mut events=vec![accepted,recorded.clone()];
            let binding = match command.input_payloads.iter().find(|payload|payload.payload_id==command.execution_binding_ref.payload_id) {
                Some(payload)=>payload.clone(),
                None=>match owner.store.read_payload(&command.execution_binding_ref.payload_id)? {
                    PayloadRead::Present(payload)=>payload,
                    _=>return Err(SessionError::StaleTarget("execution binding unavailable".into())),
                },
            };
            if let Some(mut revision)=owner.authority_observation(&command.stream_id,&command.branch_id,&actor,&binding)? {
                revision.causation_id=Some(accepted_id.clone());events.push(revision);
            }
            let guard_command = command.clone();
            let mut payloads = command.input_payloads;
            payloads.extend(leaves);
            payloads.push(content);
            let mut registered=std::collections::HashSet::new();
            if !files.is_empty() {
                let wanted:std::collections::HashSet<_>=files.iter().map(|(_,file)|file.file_id.clone()).collect();
                let mut through=owner.store.head(&command.stream_id)?;
                while through.sequence>0 && !wanted.is_subset(&registered) {
                    let after=EventCursor {stream_id:command.stream_id.clone(),sequence:through.sequence.saturating_sub(256)};
                    let page=owner.store.read_page(&command.stream_id,&after,Some(&through),256,64*1024*1024)?;
                    if page.has_more {return Err(ReadError::TooLarge.into());}
                    for event in page.events {
                        if let EventData::FileRegistered {file_id,..}=event.body.data {registered.insert(file_id);}
                    }
                    through=after;
                }
            }
            for (index,file) in files {
                let metadata=PayloadRecord::json("session.locator/v1",&file.registration)
                    .map_err(AppendError::from)?;
                let policy=PayloadRecord::json("session.evidence/v1",&serde_json::json!({
                    "policy":(file.policy_evidence),
                    "observed_file_metadata_ref":StoredPayloadRef::from(&metadata),
                })).map_err(AppendError::from)?;
                let mut data=Vec::new();
                if registered.insert(file.file_id.clone()) {
                    let registration=PayloadRecord::json("session.file-registration/v1",&serde_json::json!({
                        "metadata_ref":StoredPayloadRef::from(&metadata),"representation_ref":StoredPayloadRef::from(&file.representation),
                    })).map_err(AppendError::from)?;
                    data.push(EventData::FileRegistered {file_id:file.file_id.clone(),registration_ref:StoredPayloadRef::from(&registration)});
                    payloads.push(registration);
                }
                data.extend([
                    EventData::FileAttached {file_id:file.file_id.clone(),content_event_id:recorded.event_id.clone(),part_index:index,attach_role:"input".into()},
                    EventData::FileUsed {file_id:file.file_id,operation:"ingress_representation_captured".into(),representation_ref:StoredPayloadRef::from(&file.representation),policy_evidence_ref:StoredPayloadRef::from(&policy)},
                ]);
                for data in data {
                    let mut event=SessionEvent::typed(command.stream_id.clone(),data,actor.clone()).map_err(AppendError::from)?;
                    event.branch_id=recorded.branch_id.clone();event.turn_id=recorded.turn_id.clone();event.causation_id=Some(recorded.event_id.clone());
                    events.push(event);
                }
                payloads.extend([file.representation,metadata,policy]);
            }
            // Repeated File parts may share the exact frozen payload. Publish
            // it once while preserving every attachment position and usage.
            let mut unique=std::collections::HashMap::new();
            for payload in payloads {
                if let Some(prior)=unique.insert(payload.payload_id.clone(),payload.clone())
                    && prior!=payload {return Err(AppendError::Corrupt("file payload identity conflicts".into()).into());}
            }
            let mut payloads: Vec<_> = unique.into_values().collect();
            payloads.sort_by(|left, right| left.payload_id.as_str().cmp(right.payload_id.as_str()));
            owner.store.append_batch(AppendRequest {
                stream_id: command.stream_id.clone(),
                expected_head: owner.store.head(&command.stream_id)?,
                idempotency_key: command.ingress_key.clone(),
                events,
                payloads,
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(command.stream_id.clone());
            // Ownership travels with the worker result. If the async caller
            // was dropped during the commit, dropping this result settles it.
            Ok(QueuedTurnGuard {
                owner: owner.clone(),
                command: guard_command,
                accepted: accepted_id,
                armed: true,
            })
        })
        .await
    }

    async fn append_current(
        self: &Arc<Self>,
        stream: SessionStreamId,
        key: String,
        events: Vec<SessionEvent>,
        payloads: Vec<PayloadRecord>,
        principal: EventActor,
        policy: Arc<dyn SessionAccessPolicy>,
    ) -> Result<AppendReceipt, SessionError> {
        let owner = Arc::clone(self);
        self.blocking(move || {
            policy.authorize_actor(&principal, &stream, SessionAccess::Write)?;
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let head = owner.store.head(&stream)?;
            let receipt = owner.store.append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: head,
                idempotency_key: key,
                events,
                payloads,
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(stream);
            Ok(receipt)
        })
        .await
    }

    async fn finish_execution(
        self: &Arc<Self>,
        command: UserTurnCommand,
        actor: EventActor,
        result: anyhow::Result<String>,
        diagnostic: Option<String>,
    ) -> Result<AppendReceipt, SessionError> {
        let owner = Arc::clone(self);
        self.blocking(move || {
            // Host settlement records an already admitted execution. Revoking
            // effect permission must not suppress its terminal observations.
            let lock = owner.lock_for(&command.stream_id);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let head = owner.store.head(&command.stream_id)?;
            let input_receipt=owner.store.lookup_receipt(&command.stream_id,&command.ingress_key)?
                .ok_or_else(||SessionError::StoreUnhealthy("accepted input receipt missing at settlement".into()))?;
            let mut projection = HistoryProjection::empty(command.stream_id.clone());
            projection.source_cursor.sequence=input_receipt.first.sequence.checked_sub(1)
                .ok_or_else(||SessionError::StoreUnhealthy("accepted input receipt sequence invalid".into()))?;
            let mut last_provider_result = None;
            let mut approvals = super::approvals::UnconsumedApprovals::default();
            let mut elicitations = std::collections::BTreeMap::new();
            let cancelled = result.as_ref().err().is_some_and(zeroclaw_api::turn_stop::is_tool_loop_cancelled);
            let mut cancellation_history = Vec::new();
            loop {
                let page = owner.store.read_page(
                    &command.stream_id,
                    &projection.source_cursor,
                    Some(&head),
                    256,
                    4 * 1024 * 1024,
                )?;
                for event in &page.events {
                    match &event.body.data {
                        EventData::ElicitationRequested { request_id, .. }
                            if event.turn_id.as_ref() == Some(&command.turn_id) => {
                                elicitations.insert(request_id.clone(), event.clone());
                            }
                        EventData::ElicitationResolved { request_id, .. } => {
                            elicitations.remove(request_id);
                        }
                        _ => {}
                    }
                    if event.turn_id.as_ref() == Some(&command.turn_id) {
                        approvals.observe(event, owner.store.as_ref())?;
                    }
                    if event.turn_id.as_ref() == Some(&command.turn_id)
                        && matches!(event.body.data, EventData::ProviderAttemptCompleted {..})
                    {
                        last_provider_result = Some(event.clone());
                    }
                }
                projection.apply_page_for_turn(&page,&command.turn_id)?;
                if cancelled {
                    cancellation_history.extend(page.events.iter().cloned());
                }
                if !page.has_more {
                    break;
                }
            }
            let unresolved: Vec<_> = projection
                .invocations
                .iter()
                .filter(|(_, state)| {
                    state.turn_id.as_ref() == Some(&command.turn_id)
                        && !state.background
                        && matches!(
                            state.status,
                            InvocationStatus::Unknown | InvocationStatus::Running
                        )
                })
                .map(|(id, _)| id.clone())
                .collect();
            let mut payloads = Vec::new();
            let mut events = Vec::new();
            let error_ref = if result.is_err() {
                let error = PayloadRecord::json("session.error-detail/v1", &serde_json::json!({
                    "error_key":"engine_execution_error","detail":diagnostic,
                    "detail_unavailable_reason":if diagnostic.is_none() {Some("host_diagnostic_unavailable")} else {None},
                })).map_err(AppendError::from)?;
                let reference = StoredPayloadRef::from(&error);
                payloads.push(error);
                Some(reference)
            } else {None};
            if cancelled {
                let accepted = cancellation_history.iter().find(|event| {
                    event.turn_id.as_ref() == Some(&command.turn_id)
                        && event.body.kind() == EventKind::TurnAccepted
                }).ok_or_else(|| SessionError::StoreUnhealthy("cancelled turn admission missing".into()))?;
                let (observations, records) = owner.cancelled_provider_events(accepted, &cancellation_history, error_ref.as_ref())?;
                events.extend(observations);
                payloads.extend(records);
            }
            // An engine return/drop cannot settle an outstanding external
            // invocation. Close its observation window as Unknown first, so
            // the terminal turn names exactly the unresolved invocations.
            for id in &unresolved {
                let state = &projection.invocations[id];
                if state.status == InvocationStatus::Running
                    && !events.iter().any(|event| event.invocation_id.as_ref() == Some(id)
                        && event.body.kind() == EventKind::InvocationOutcomeUnknown)
                {
                    let mut unknown = SessionEvent::typed(command.stream_id.clone(),
                        EventData::InvocationOutcomeUnknown {
                            last_observed_event_id:state.last_event_id.clone(),reason_key:"engine_returned_without_effect_settlement".into(),
                            observed_partial_ref:None,error_ref:error_ref.clone(),
                        },actor.clone()).map_err(AppendError::from)?;
                    unknown.branch_id=state.branch_id.clone();unknown.turn_id=state.turn_id.clone();unknown.invocation_id=Some(id.clone());
                    events.push(unknown);
                }
            }
            // Finishing a turn releases its ephemeral preparation scope. A
            // pending reply or unused confirmation cannot authorize later work.
            for (request_id, requested) in approvals.into_requests() {
                let mut invalidated = SessionEvent::typed(command.stream_id.clone(),
                    EventData::ApprovalInvalidated {
                        request_id, confirmation_id: None,
                        reason_key: "approval_turn_execution_finished".into(), evidence_ref: None,
                    }, actor.clone()).map_err(AppendError::from)?;
                invalidated.branch_id = requested.branch_id;
                invalidated.turn_id = requested.turn_id;
                invalidated.invocation_id = requested.invocation_id;
                events.push(invalidated);
            }
            for (request_id, requested) in elicitations {
                let resolution = PayloadRecord::json("session.evidence/v1", &serde_json::json!({
                    "state": if cancelled { "cancelled" } else { "interrupted" },
                    "reason_key": "elicitation_turn_execution_finished",
                })).map_err(AppendError::from)?;
                let mut resolved = SessionEvent::typed(command.stream_id.clone(),
                    EventData::ElicitationResolved {
                        request_id, resolution_ref: StoredPayloadRef::from(&resolution),
                    }, actor.clone()).map_err(AppendError::from)?;
                resolved.branch_id = requested.branch_id;
                resolved.turn_id = requested.turn_id;
                resolved.causation_id = Some(requested.event_id);
                events.push(resolved);
                payloads.push(resolution);
            }
            // No inference of success from only a subset of completed effects.
            let data = if cancelled {
                EventData::TurnCancelled {
                    reason_key: "executor_cooperatively_stopped".into(),
                    unresolved_invocation_ids: unresolved,
                }
            } else if !unresolved.is_empty() {
                EventData::TurnOutcomeUnknown {
                    invocation_ids: unresolved,
                    reason_key: "execution_continuation_unresolved".into(),
                    observed_partial_ref: None,
                }
            } else {
                match result {
                    Ok(text) => {
                        let mut existing = None;
                        if let Some(context) = owner.branch_context(&command.stream_id, &command.branch_id)?
                            && context.turn_id.as_ref() == Some(&command.turn_id)
                            && let Some(item) = super::context_messages::context_refs(owner.store.as_ref(), &context)?.last()
                            && let ContextMessageSource::Content { content_event_id, .. } = &item.source
                        {
                                        let source = owner.store.read_event(content_event_id)?.ok_or_else(|| SessionError::StaleTarget("terminal content missing".into()))?;
                                        if source.turn_id.as_ref() == Some(&command.turn_id)
                                            && super::context_messages::resolve_message(owner.store.as_ref(), &command.stream_id, item)?.is_some_and(|message| matches!(message, zeroclaw_api::model_provider::ConversationMessage::Chat(chat) if chat.role == "assistant" && chat.content == text)) {
                                            existing = Some(content_event_id.clone());
                                        }
                        }
                        let result_event_id = if let Some(existing) = existing {
                            existing
                        } else {
                        let mut parts = Vec::new();
                        let mut provenance_refs = Vec::new();
                        if let Some(source) = last_provider_result {
                            let EventData::ProviderAttemptCompleted {response_ref, ..} = source.body.data else {
                                return Err(SessionError::StoreUnhealthy("provider result kind mismatch".into()));
                            };
                            let PayloadRead::Present(response) = owner.store.read_payload(&response_ref.payload_id)? else {
                                return Err(SessionError::StaleTarget("provider response unavailable at turn commit".into()));
                            };
                            let response: zeroclaw_api::session_events::ProviderResponsePayload = serde_json::from_slice(&response.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                            for (block_index, block) in response.blocks.into_iter().enumerate() {
                                if let zeroclaw_api::session_events::ResponseBlock::Text {text_ref} = &block {
                                    parts.push(RecordedContentPart::Text {text_ref: text_ref.clone()});
                                }
                                provenance_refs.push(zeroclaw_api::session_events::ProviderBlockRef {
                                    result_event_id: source.event_id.clone(), block_index,
                                });
                            }
                        } else {
                            // Host-only completion has no provider provenance.
                            // With a model response present, reuse its canonical
                            // leaves; display receipts/notices are not raw output.
                            let text = PayloadRecord::new(zeroclaw_api::session_events::PayloadId::new(),
                                "session.text/v1".into(), text.to_owned().into_bytes()).map_err(AppendError::from)?;
                            parts.push(RecordedContentPart::Text {text_ref: StoredPayloadRef::from(&text)});
                            payloads.push(text);
                        }
                        let content = PayloadRecord::json(
                            "session.content/v1",
                            &ContentPayload {
                                message_id: MessageId::new(),
                                parts,
                            },
                        )
                        .map_err(AppendError::from)?;
                        let mut recorded = SessionEvent::typed(
                            command.stream_id.clone(),
                            EventData::AssistantContentRecorded {
                                content_ref: StoredPayloadRef::from(&content),
                                provenance_refs,
                            },
                            actor.clone(),
                        )
                        .map_err(AppendError::from)?;
                        recorded.branch_id = Some(command.branch_id.clone());
                        recorded.turn_id = Some(command.turn_id.clone());
                        payloads.push(content);
                        let id = recorded.event_id.clone();
                        events.push(recorded);
                        id
                        };
                        let completion = PayloadRecord::json(
                            "session.turn-completion/v1",
                            &TurnCompletion::Success {
                                result_event_ids: vec![result_event_id],
                            },
                        )
                        .map_err(AppendError::from)?;
                        let reference = StoredPayloadRef::from(&completion);
                        payloads.push(completion);
                        EventData::TurnCommitted {
                            completion_ref: reference,
                        }
                    }
                    Err(_) => {
                        let error_ref = error_ref.ok_or_else(||SessionError::StoreUnhealthy("failed execution diagnostic absent".into()))?;
                        let completion = PayloadRecord::json("session.turn-completion/v1",&TurnCompletion::Failed {
                            error_ref,observed_result_event_ids:vec![],
                        }).map_err(AppendError::from)?;
                        let reference = StoredPayloadRef::from(&completion);
                        payloads.push(completion);
                        EventData::TurnCommitted {completion_ref:reference}
                    },
                }
            };
            let mut terminal = SessionEvent::typed(command.stream_id.clone(), data, actor)
                .map_err(AppendError::from)?;
            terminal.branch_id = Some(command.branch_id);
            terminal.turn_id = Some(command.turn_id.clone());
            events.push(terminal);
            let receipt = owner.store.append_batch(AppendRequest {
                stream_id: command.stream_id.clone(),
                expected_head: head,
                idempotency_key: format!("turn-outcome:{}", command.turn_id),
                events,
                payloads,
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(command.stream_id);
            Ok(receipt)
        })
        .await
    }
}
