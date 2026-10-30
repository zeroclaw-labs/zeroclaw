//! Reconstruct restartable work from verified recovery facts. The host owns
//! engine construction and current routing; this module owns no queue cache.
use super::engine::EngineSession;
use super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use zeroclaw_api::session_events::capture::{EngineFileInput, EngineInputPart, EngineTurnInput};
use zeroclaw_api::session_events::*;

pub(super) fn pending_requeues<'a>(
    history: &'a [SessionEvent],
    branch: &BranchId,
) -> Result<Vec<(&'a SessionEvent, &'a SessionEvent)>, SessionError> {
    let mut pending = Vec::new();
    for accepted in history.iter().filter(|event| {
        event.branch_id.as_ref() == Some(branch) && event.body.kind() == EventKind::TurnAccepted
    }) {
        let turn = accepted
            .turn_id
            .as_ref()
            .ok_or(SessionError::InvocationMismatch)?;
        let last = history.iter().rev().find(|event| {
            event.turn_id.as_ref() == Some(turn)
                && matches!(
                    event.body.data,
                    EventData::TurnStarted { .. }
                        | EventData::TurnCommitted { .. }
                        | EventData::TurnCancelled { .. }
                        | EventData::TurnOutcomeUnknown { .. }
                        | EventData::RecoveryObserved {
                            disposition: RecoveryDisposition::Requeued
                                | RecoveryDisposition::Rescheduled { .. },
                            ..
                        }
                )
        });
        if let Some(requeued) = last
            && matches!(
                requeued.body.data,
                EventData::RecoveryObserved {
                    disposition: RecoveryDisposition::Requeued,
                    ..
                }
            )
        {
            if !rules::can_requeue_turn(history, turn) {
                return Err(SessionError::InvalidAdmission(
                    "requeued turn contains effects or output".into(),
                ));
            }
            pending.push((accepted, requeued));
        }
    }
    pending.sort_by_key(|(accepted, _)| accepted.sequence);
    Ok(pending)
}

#[derive(Debug, thiserror::Error)]
#[error("queued input payload was erased")]
struct QueuedInputErased;

fn present(
    store: &dyn SessionEventStore,
    reference: &StoredPayloadRef,
) -> Result<PayloadRecord, SessionError> {
    let record = match store.read_payload(&reference.payload_id)? {
        PayloadRead::Present(record) => record,
        PayloadRead::ErasurePending | PayloadRead::Redacted => {
            return Err(SessionError::Operation(QueuedInputErased.into()));
        }
    };
    if StoredPayloadRef::from(&record) != *reference {
        return Err(ReadError::Corrupt("queued input reference mismatch".into()).into());
    }
    Ok(record)
}

impl SessionCoordinator {
    pub(super) async fn requeue_for_dispatch(
        self: &Arc<Self>,
        stream: SessionStreamId,
        branch: BranchId,
        turn: TurnId,
    ) -> Result<Option<SessionEvent>, SessionError> {
        let owner = self.clone();
        self.blocking(move || {
            if owner
                .store
                .read_latest_event(&stream, &branch, None, &EventKind::RecoveryObserved)?
                .is_none()
            {
                return Ok(None);
            }
            let history = owner.stream_history(&stream)?;
            if history.iter().any(|event| {
                event.turn_id.as_ref() == Some(&turn)
                    && matches!(
                        event.body.kind(),
                        EventKind::TurnCommitted
                            | EventKind::TurnCancelled
                            | EventKind::TurnOutcomeUnknown
                    )
            }) {
                return Ok(None);
            }
            let pending = pending_requeues(&history, &branch)?;
            if let Some((accepted, recovery)) = pending.first() {
                if accepted.turn_id.as_ref() != Some(&turn) {
                    return Err(SessionError::StaleTarget(
                        "earlier recovered input must execute first".into(),
                    ));
                }
                return Ok(Some((*recovery).clone()));
            }
            Ok(None)
        })
        .await
    }

    pub(super) fn turn_execution_binding(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: &TurnId,
    ) -> Result<StoredPayloadRef, SessionError> {
        if let Some(recovery) = self.store.read_latest_event(
            stream,
            branch,
            Some(turn),
            &EventKind::RecoveryObserved,
        )? {
            match recovery.body.data {
                EventData::RecoveryObserved {
                    disposition:
                        RecoveryDisposition::Rescheduled {
                            execution_binding_ref,
                        },
                    ..
                } => return Ok(execution_binding_ref),
                EventData::RecoveryObserved {
                    disposition: RecoveryDisposition::Requeued,
                    ..
                } => {
                    let started = self.store.read_latest_event(
                        stream,
                        branch,
                        Some(turn),
                        &EventKind::TurnStarted,
                    )?;
                    if started
                        .as_ref()
                        .is_none_or(|started| started.sequence < recovery.sequence)
                    {
                        return Err(SessionError::StaleTarget(
                            "requeued turn has not restarted".into(),
                        ));
                    }
                }
                _ => {}
            }
        }
        let started = self
            .store
            .read_latest_event(stream, branch, Some(turn), &EventKind::TurnStarted)?
            .ok_or_else(|| SessionError::StaleTarget("turn execution binding missing".into()))?;
        match started.body.data {
            EventData::TurnStarted {
                execution_binding_ref,
                ..
            } => Ok(execution_binding_ref),
            _ => Err(SessionError::InvocationMismatch),
        }
    }
}

impl EngineSession {
    pub(super) async fn cancel_requeued_input(
        &self,
        turn: &str,
        reason: &str,
        denied_input: Option<EngineTurnInput>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !reason.is_empty() && reason.len() <= 256,
            "invalid_recovery_rejection_reason"
        );
        let turn = TurnId::parse(turn)?;
        let reason = reason.to_owned();
        let session = self.clone();
        self.owner
            .blocking(move || {
                let lock = session.owner.lock_for(&session.stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let authorization = SessionAccessPolicy::authorize_actor(
                    session.authority.as_ref(),
                    &session.actor,
                    &session.stream,
                    SessionAccess::Write,
                )
                .and_then(|()| {
                    session
                        .authority
                        .authorize_context(&session.stream, &session.branch)
                })
                .and_then(|()| match &denied_input {
                    Some(input) => session.authority.execution_binding(input).map(|_| ()),
                    None => Ok(()),
                });
                if denied_input.is_some() {
                    match authorization {
                        Err(SessionError::PermissionDenied(_)) => {}
                        Ok(()) => return Ok(()),
                        Err(error) => return Err(error),
                    }
                } else {
                    authorization?;
                }
                let history = session.owner.stream_history(&session.stream)?;
                let pending = pending_requeues(&history, &session.branch)?;
                let Some((accepted, recovered)) = pending
                    .into_iter()
                    .find(|(accepted, _)| accepted.turn_id.as_ref() == Some(&turn))
                else {
                    if denied_input.is_some()
                        || history.iter().any(|event| {
                            event.turn_id.as_ref() == Some(&turn)
                                && matches!(
                                    event.body.kind(),
                                    EventKind::TurnCommitted
                                        | EventKind::TurnCancelled
                                        | EventKind::TurnOutcomeUnknown
                                )
                        })
                    {
                        return Ok(());
                    }
                    return Err(SessionError::StaleTarget(
                        "input is not awaiting recovery dispatch".into(),
                    ));
                };
                if denied_input.is_some() && accepted.actor != session.actor {
                    return Err(SessionError::PermissionDenied(
                        "denied recovery actor does not own pending input".into(),
                    ));
                }
                let mut cancelled = SessionEvent::typed(
                    session.stream.clone(),
                    EventData::TurnCancelled {
                        reason_key: reason,
                        unresolved_invocation_ids: vec![],
                    },
                    session.actor.clone(),
                )
                .map_err(AppendError::from)?;
                cancelled.branch_id = Some(session.branch.clone());
                cancelled.turn_id = Some(turn.clone());
                cancelled.causation_id = Some(recovered.event_id.clone());
                session.owner.store.append_batch(AppendRequest {
                    stream_id: session.stream.clone(),
                    expected_head: session.owner.store.head(&session.stream)?,
                    idempotency_key: format!("turn-outcome:{turn}"),
                    events: vec![cancelled],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                })?;
                let _ = session.owner.wakeups.send(session.stream);
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub(super) async fn recoverable_inputs(
        &self,
        provider_ref: String,
        model: String,
    ) -> anyhow::Result<Vec<EngineTurnInput>> {
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Read,
        )?;
        self.authority
            .authorize_context(&self.stream, &self.branch)?;
        let session = self.clone();
        let (inputs, erased) = self
            .owner
            .blocking(move || {
                let history = session.owner.stream_history(&session.stream)?;
                let pending = pending_requeues(&history, &session.branch)?;
                if pending.is_empty() {
                    return Ok((Vec::new(), Vec::new()));
                }
                let mut alias = history
                    .iter()
                    .find_map(|event| match &event.body.data {
                        EventData::SessionStarted { owning_agent, .. } => {
                            Some(owning_agent.clone())
                        }
                        _ => None,
                    })
                    .ok_or_else(|| {
                        SessionError::StaleTarget("queued input owning agent unavailable".into())
                    })?;
                if let Some(reference) =
                    history
                        .iter()
                        .rev()
                        .find_map(|event| match &event.body.data {
                            EventData::SessionMetadataChanged {
                                field,
                                new_value_ref,
                            } if field == "owning_agent" => Some(new_value_ref),
                            _ => None,
                        })
                {
                    let body = zeroclaw_api::session_events::context_read::present(
                        session.owner.store.as_ref(),
                        reference,
                    )?;
                    alias = serde_json::from_slice(&body.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                }
                let mut inputs = Vec::with_capacity(pending.len());
                let mut erased = Vec::new();
                for (accepted, _) in pending {
                    SessionAccessPolicy::authorize_actor(
                        session.authority.as_ref(),
                        &session.actor,
                        &session.stream,
                        SessionAccess::Read,
                    )?;
                    session
                        .authority
                        .authorize_context(&session.stream, &session.branch)?;
                    let decoded = (|| -> Result<EngineTurnInput, SessionError> {
                        let EventData::TurnAccepted {
                            input_ref,
                            ingress_ref,
                            reply_route_ref,
                        } = &accepted.body.data
                        else {
                            return Err(SessionError::InvocationMismatch);
                        };
                        let content: ContentPayload = serde_json::from_slice(
                            &present(session.owner.store.as_ref(), input_ref)?.body,
                        )
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                        let ingress: serde_json::Value = serde_json::from_slice(
                            &present(session.owner.store.as_ref(), ingress_ref)?.body,
                        )
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                        let workspace = ingress
                            .get("execution_workspace")
                            .map(|value| {
                                serde_json::from_value::<std::path::PathBuf>(value.clone())
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))
                            })
                            .transpose()?;
                        let ingress = serde_json::from_value(ingress)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                        let reply_route = reply_route_ref
                            .as_ref()
                            .map(|reference| {
                                serde_json::from_slice(
                                    &present(session.owner.store.as_ref(), reference)?.body,
                                )
                                .map_err(|error| {
                                    SessionError::Read(ReadError::Corrupt(error.to_string()))
                                })
                            })
                            .transpose()?;
                        let mut file_uses = history.iter().filter_map(|event| {
                            if event.turn_id != accepted.turn_id
                                || event.branch_id != accepted.branch_id
                            {
                                return None;
                            }
                            match &event.body.data {
                                EventData::FileUsed {
                                    file_id,
                                    operation,
                                    representation_ref,
                                    policy_evidence_ref,
                                } if operation == "ingress_representation_captured" => {
                                    Some((file_id, representation_ref, policy_evidence_ref))
                                }
                                _ => None,
                            }
                        });
                        let mut parts = Vec::with_capacity(content.parts.len());
                        for part in content.parts {
                            parts.push(match part {
                                RecordedContentPart::Text { text_ref } => EngineInputPart::Text(
                                    String::from_utf8(
                                        present(session.owner.store.as_ref(), &text_ref)?.body,
                                    )
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                ),
                                RecordedContentPart::Opaque {
                                    type_name,
                                    body_ref,
                                } => EngineInputPart::Opaque {
                                    type_name,
                                    body: serde_json::from_slice(
                                        &present(session.owner.store.as_ref(), &body_ref)?.body,
                                    )
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                },
                                RecordedContentPart::File { file_id } => {
                                    let (recorded_id, representation, evidence) = file_uses
                                        .next()
                                        .filter(|(recorded_id, _, _)| *recorded_id == &file_id)
                                        .ok_or_else(|| {
                                            ReadError::Corrupt(
                                                "queued file observation missing or reordered"
                                                    .into(),
                                            )
                                        })?;
                                    let policy: serde_json::Value = serde_json::from_slice(
                                        &present(session.owner.store.as_ref(), evidence)?.body,
                                    )
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                                    let metadata: StoredPayloadRef = serde_json::from_value(
                                        policy["observed_file_metadata_ref"].clone(),
                                    )
                                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                                    EngineInputPart::File(EngineFileInput {
                                        file_id: recorded_id.clone(),
                                        registration: serde_json::from_slice(
                                            &present(session.owner.store.as_ref(), &metadata)?.body,
                                        )
                                        .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                                        representation: present(
                                            session.owner.store.as_ref(),
                                            representation,
                                        )?,
                                        policy_evidence: policy.get("policy").cloned().ok_or_else(
                                            || {
                                                ReadError::Corrupt(
                                                    "queued file policy missing".into(),
                                                )
                                            },
                                        )?,
                                    })
                                }
                                RecordedContentPart::Other => {
                                    return Err(SessionError::InvalidAdmission(
                                        "unknown queued input cannot be dispatched".into(),
                                    ));
                                }
                            });
                        }
                        if file_uses.next().is_some() {
                            return Err(ReadError::Corrupt(
                                "queued file observations exceed parts".into(),
                            )
                            .into());
                        }
                        Ok(EngineTurnInput {
                            execution_workspace: workspace,
                            turn_id: accepted
                                .turn_id
                                .as_ref()
                                .ok_or(SessionError::InvocationMismatch)?
                                .to_string(),
                            agent_alias: Some(alias.clone()),
                            ingress,
                            reply_route,
                            parts,
                            provider_ref: provider_ref.clone(),
                            model: model.clone(),
                        })
                    })();
                    match decoded {
                        Ok(input) => inputs.push(input),
                        Err(SessionError::Operation(error)) if error.is::<QueuedInputErased>() => {
                            erased.push(
                                accepted
                                    .turn_id
                                    .as_ref()
                                    .ok_or(SessionError::InvocationMismatch)?
                                    .to_string(),
                            );
                        }
                        Err(error) => return Err(error),
                    }
                }
                Ok((inputs, erased))
            })
            .await?;
        for turn in erased {
            self.cancel_requeued_input(&turn, "recovered_input_payload_erased", None)
                .await?;
        }
        SessionAccessPolicy::authorize_actor(
            self.authority.as_ref(),
            &self.actor,
            &self.stream,
            SessionAccess::Read,
        )?;
        self.authority
            .authorize_context(&self.stream, &self.branch)?;
        Ok(inputs)
    }
}
