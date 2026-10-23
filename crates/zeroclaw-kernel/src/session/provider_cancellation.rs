//! Settle observed local cancellation without claiming a remote provider stopped.
use super::*;
use std::collections::BTreeMap;
use zeroclaw_api::session_events::*;

impl SessionCoordinator {
    /// The caller holds the stream lock and commits this plan with its own
    /// terminal event. All scope, progress and intent come from durable facts.
    pub(super) fn cancelled_provider_events(
        &self,
        boundary: &SessionEvent,
        history: &[SessionEvent],
        error_ref: Option<&StoredPayloadRef>,
    ) -> Result<(Vec<SessionEvent>, Vec<PayloadRecord>), SessionError> {
        let transform = boundary.body.kind() == EventKind::ContextTransformStarted;
        let mut pending: BTreeMap<InvocationId, (&SessionEvent, Option<&SessionEvent>)> =
            BTreeMap::new();
        for event in history {
            let Some(invocation) = &event.invocation_id else {
                continue;
            };
            match &event.body.data {
                EventData::ProviderAttemptStarted {
                    prepared_event_id, ..
                } if event.branch_id == boundary.branch_id && event.turn_id == boundary.turn_id => {
                    if transform {
                        let prepared =
                            self.store.read_event(prepared_event_id)?.ok_or_else(|| {
                                ReadError::Gap("cancelled provider preparation missing".into())
                            })?;
                        if prepared.causation_id.as_ref() != Some(&boundary.event_id) {
                            continue;
                        }
                    }
                    pending.insert(invocation.clone(), (event, None));
                }
                EventData::ProviderResponseProgressObserved { .. } => {
                    if let Some((_, progress)) = pending.get_mut(invocation) {
                        *progress = Some(event);
                    }
                }
                _ if zeroclaw_api::session_events::rules::terminal(&event.body.kind()) => {
                    pending.remove(invocation);
                }
                _ => {}
            }
        }
        let mut events = Vec::new();
        let mut payloads = Vec::new();
        let mut host_request: Option<EventId> = None;
        for (invocation, (started, progress)) in pending {
            let request = history.iter().rev().find(|event| {
                event.sequence >= boundary.sequence
                    && event.stream_id == boundary.stream_id
                    && match &event.body.data {
                        EventData::CancellationRequested { target_ref, .. } => match target_ref {
                            TargetRef::Session { stream_id } => stream_id == &boundary.stream_id,
                            TargetRef::Branch { branch_id } => {
                                boundary.branch_id.as_ref() == Some(branch_id)
                            }
                            TargetRef::Turn { turn_id } => {
                                boundary.turn_id.as_ref() == Some(turn_id)
                            }
                            TargetRef::Invocation { invocation_id } => invocation_id == &invocation,
                            TargetRef::ContextTransform { started_event_id } => {
                                transform && started_event_id == &boundary.event_id
                            }
                        },
                        _ => false,
                    }
            });
            let request_event_id = if let Some(request) = request {
                request.event_id.clone()
            } else if let Some(id) = &host_request {
                id.clone()
            } else {
                // ToolLoopCancelled is an observed host stop, not an inferred
                // user reply. Give that local intent its own durable identity.
                let target_ref = if transform {
                    TargetRef::ContextTransform {
                        started_event_id: boundary.event_id.clone(),
                    }
                } else {
                    TargetRef::Turn {
                        turn_id: boundary
                            .turn_id
                            .clone()
                            .ok_or(SessionError::InvocationMismatch)?,
                    }
                };
                let mut request = SessionEvent::typed(
                    boundary.stream_id.clone(),
                    EventData::CancellationRequested {
                        target_ref,
                        expected_incarnation: self.execution_incarnation.clone(),
                        reason_key: "executor_cooperative_stop_observed".into(),
                    },
                    EventActor::System {
                        component: "execution_settlement".into(),
                    },
                )
                .map_err(AppendError::from)?;
                request.branch_id = boundary.branch_id.clone();
                request.turn_id = boundary.turn_id.clone();
                request.causation_id = Some(boundary.event_id.clone());
                let id = request.event_id.clone();
                host_request = Some(id.clone());
                events.push(request);
                id
            };
            let partial = progress.and_then(|event| match &event.body.data {
                EventData::ProviderResponseProgressObserved { observation_ref } => {
                    Some(observation_ref.clone())
                }
                _ => None,
            });
            let observation = PayloadRecord::json(
                "session.provider-observation/v1",
                &ProviderObservationPayload {
                    operations: vec![ProviderObservation::CancellationObserved {
                        request_event_id: request_event_id.clone(),
                        stage: if progress.is_some() {
                            DiagnosticStage::ConsumeStream
                        } else {
                            DiagnosticStage::AwaitResponse
                        },
                    }],
                },
            )
            .map_err(AppendError::from)?;
            let mut observed = SessionEvent::typed(
                boundary.stream_id.clone(),
                EventData::ProviderResponseProgressObserved {
                    observation_ref: StoredPayloadRef::from(&observation),
                },
                EventActor::System {
                    component: "execution_settlement".into(),
                },
            )
            .map_err(AppendError::from)?;
            observed.branch_id = started.branch_id.clone();
            observed.turn_id = started.turn_id.clone();
            observed.invocation_id = Some(invocation.clone());
            observed.causation_id = Some(request_event_id);
            let mut unknown = SessionEvent::typed(
                boundary.stream_id.clone(),
                EventData::InvocationOutcomeUnknown {
                    last_observed_event_id: observed.event_id.clone(),
                    reason_key: "cancelled_remote_outcome_unconfirmed".into(),
                    observed_partial_ref: partial,
                    error_ref: error_ref.cloned(),
                },
                observed.actor.clone(),
            )
            .map_err(AppendError::from)?;
            unknown.branch_id = started.branch_id.clone();
            unknown.turn_id = started.turn_id.clone();
            unknown.invocation_id = Some(invocation);
            unknown.causation_id = Some(observed.event_id.clone());
            events.extend([observed, unknown]);
            payloads.push(observation);
        }
        Ok((events, payloads))
    }
}
