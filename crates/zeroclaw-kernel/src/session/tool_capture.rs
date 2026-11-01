//! Durable tool preparation and approval consumption. Handles retain identity,
//! never a live permission snapshot; the injected resolver runs at dispatch.
use super::*;
use zeroclaw_api::channel::ChannelApprovalResponse;
use zeroclaw_api::session_events::capture::{
    ApprovalRequestIntent, PreparedToolPort, ToolAdmissionPort, ToolObservationPort,
    ToolPreparationIntent,
};
use zeroclaw_api::session_events::*;

mod approval_replies;
#[cfg(test)]
mod tests;

struct ToolScope {
    owner: Arc<SessionCoordinator>,
    stream: SessionStreamId,
    branch: BranchId,
    turn: TurnId,
    resolver: Arc<dyn PermissionResolver>,
}
struct PreparedTool {
    scope: Arc<ToolScope>,
    invocation: InvocationId,
    policy_id: EventId,
    execution_ref: StoredPayloadRef,
    request_id: String,
    prompt: bool,
    route_ref: StoredPayloadRef,
    incarnation: String,
    // Creates the one-use normalized execution input. Kept only until
    // dispatch/drop; credentials in it never enter the historical payload.
    live_name: String,
    live_arguments: serde_json::Value,
}
impl SessionCoordinator {
    pub fn tool_capture(
        self: &Arc<Self>,
        stream: SessionStreamId,
        branch: BranchId,
        turn: TurnId,
        resolver: Arc<dyn PermissionResolver>,
    ) -> Arc<dyn ToolAdmissionPort> {
        Arc::new(ToolScope {
            owner: Arc::clone(self),
            stream,
            branch,
            turn,
            resolver,
        })
    }
}
impl ToolScope {
    fn event(&self, invocation: &InvocationId, data: EventData) -> anyhow::Result<SessionEvent> {
        let mut event = SessionEvent::typed(
            self.stream.clone(),
            data,
            EventActor::System {
                component: "tool_execution".into(),
            },
        )?;
        event.branch_id = Some(self.branch.clone());
        event.turn_id = Some(self.turn.clone());
        event.invocation_id = Some(invocation.clone());
        Ok(event)
    }
    fn payload(&self, reference: &StoredPayloadRef) -> anyhow::Result<Vec<u8>> {
        match self.owner.store.read_payload(&reference.payload_id)? {
            PayloadRead::Present(payload) if StoredPayloadRef::from(&payload) == *reference => {
                Ok(payload.body)
            }
            _ => anyhow::bail!("tool_proposal_content_unavailable"),
        }
    }
    fn append(
        &self,
        key: String,
        events: Vec<SessionEvent>,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        let receipt = self.owner.store.append_batch(AppendRequest {
            stream_id: self.stream.clone(),
            expected_head: self.owner.store.head(&self.stream)?,
            idempotency_key: key,
            events,
            payloads,
            erasure_reference_commitment: None,
        })?;
        let _ = self.owner.wakeups.send(self.stream.clone());
        Ok(receipt)
    }
    /// Associate the proposal with the current committed response. Arguments
    /// may have undergone schema recovery; they are not a call identifier.
    fn proposal(&self, intent: &ToolPreparationIntent) -> anyhow::Result<(EventId, usize)> {
        if let Some(event) = self.owner.store.read_latest_event(
            &self.stream,
            &self.branch,
            Some(&self.turn),
            &EventKind::ProviderAttemptCompleted,
        )? {
            let EventData::ProviderAttemptCompleted { response_ref, .. } = &event.body.data else {
                anyhow::bail!("tool_proposal_lookup_kind_mismatch");
            };
            let response: ProviderResponsePayload =
                serde_json::from_slice(&self.payload(response_ref)?)?;
            let mut text = String::new();
            let mut text_index = None;
            for (index, block) in response.blocks.iter().enumerate() {
                match block {
                    ResponseBlock::ToolCall { tool_call_ref } => {
                        let call: serde_json::Value =
                            serde_json::from_slice(&self.payload(tool_call_ref)?)?;
                        if call["name"].as_str() == Some(&intent.proposed_name)
                            && intent.provider_call_id.as_ref().is_none_or(|id| {
                                call["provider_call_id"].as_str() == Some(id.as_str())
                            })
                        {
                            return Ok((event.event_id.clone(), index));
                        }
                    }
                    ResponseBlock::Text { text_ref } => {
                        let bytes = self.payload(text_ref)?;
                        text.push_str(std::str::from_utf8(&bytes)?);
                        text_index.get_or_insert(index);
                    }
                    _ => {}
                }
            }
            if let Some(index) = text_index {
                let (_, calls) = zeroclaw_tool_call_parser::parse_tool_calls(&text);
                if calls.iter().any(|call| {
                    call.name == intent.proposed_name
                        // Text protocols may omit an ID; runtime creates one
                        // for typed history. Only compare IDs when both the
                        // provider response and preparation carry one. An
                        // explicit provider ID must still match exactly.
                        && call.tool_call_id.as_ref().is_none_or(|provider_id| {
                            intent
                                .provider_call_id
                                .as_ref()
                                .is_none_or(|id| id == provider_id)
                        })
                }) {
                    return Ok((event.event_id.clone(), index));
                }
            }
            // A proposal must come from the immediately preceding response,
            // never an older call with coincidentally identical identifiers.
        }
        anyhow::bail!("tool_proposal_has_no_committed_provider_origin")
    }
}
#[async_trait::async_trait]
impl ToolAdmissionPort for ToolScope {
    async fn prepare(
        &self,
        intent: ToolPreparationIntent,
    ) -> anyhow::Result<Arc<dyn PreparedToolPort>> {
        let scope = Arc::new(ToolScope {
            owner: Arc::clone(&self.owner),
            stream: self.stream.clone(),
            branch: self.branch.clone(),
            turn: self.turn.clone(),
            resolver: Arc::clone(&self.resolver),
        });
        let work_scope = Arc::clone(&scope);
        let prepared = self.owner.blocking(move || {
            let lock = work_scope.owner.lock_for(&work_scope.stream);
            let _guard = lock.lock().unwrap_or_else(|p|p.into_inner());
            let run = || -> anyhow::Result<PreparedTool> {
                let (provider_result_event_id,block_index) = work_scope.proposal(&intent)?;
                let invocation = InvocationId::new();
                let proposal = work_scope.event(&invocation,EventData::ToolCallProposed {provider_result_event_id,block_index})?;
                let arguments = PayloadRecord::json("session.tool-arguments/v1",&intent.arguments)?;
                let action = PayloadRecord::json("session.tool-call/v1",&serde_json::json!({"name":intent.name,"arguments_ref":StoredPayloadRef::from(&arguments)}))?;
                let current=work_scope.resolver.tool_authority_evidence(&work_scope.stream,&intent.name)?;
                let authority = PayloadRecord::json("session.authority/v1",&serde_json::json!({"approval_observation":{"requirement":intent.approval_requirement,"source":"current_approval_manager"},"admission":current}))?;
                let route = PayloadRecord::json("session.route/v1",&intent.trusted_route)?;
                let policy = work_scope.event(&invocation,EventData::PolicyDecisionRecorded {
                    proposed_event_id:proposal.event_id.clone(),final_action_ref:StoredPayloadRef::from(&action),
                    decision:intent.approval_requirement.clone(),action_fingerprint:Digest::from_bytes(&canonical_json(&serde_json::json!({"name":intent.name,"arguments":intent.arguments}))?),
                    matched_rule_refs:vec![],authority_evidence_ref:StoredPayloadRef::from(&authority),
                })?;
                let prepared = PreparedTool {scope:Arc::clone(&work_scope),invocation,
                    policy_id:policy.event_id.clone(), execution_ref:StoredPayloadRef::from(&action),
                    request_id:format!("approval:{}",policy.event_id),prompt:intent.approval_requirement=="prompt",
                    route_ref:StoredPayloadRef::from(&route),incarnation:work_scope.owner.execution_incarnation.clone(),live_name:intent.name,live_arguments:intent.arguments};
                work_scope.append(format!("tool-prepare:{}",policy.event_id),vec![proposal,policy],vec![arguments,action,authority,route])?;
                Ok(prepared)
            };
            run().map_err(SessionError::Operation)
        }).await?;
        Ok(Arc::new(prepared))
    }
}
impl PreparedTool {
    async fn commit(
        &self,
        suffix: &str,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        let event = self.scope.event(&self.invocation, data)?;
        self.commit_observation(suffix, event, payloads).await
    }
    async fn commit_observation(
        &self,
        suffix: &str,
        event: SessionEvent,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        let key = format!("tool-{}:{}", suffix, self.policy_id);
        let scope = Arc::clone(&self.scope);
        Ok(self
            .scope
            .owner
            .blocking(move || {
                let lock = scope.owner.lock_for(&scope.stream);
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                if matches!(event.body.data, EventData::ApprovalInvalidated { .. }) {
                    let policy = key.strip_prefix("tool-invalidate:").ok_or_else(|| {
                        SessionError::InvalidAdmission("approval invalidation key mismatch".into())
                    })?;
                    if let Some(receipt) = scope
                        .owner
                        .store
                        .lookup_receipt(&scope.stream, &format!("tool-start:{policy}"))?
                    {
                        // A consumed approval cannot be labelled unexecuted.
                        // Started calls are settled by their effect observer.
                        return Ok(receipt);
                    }
                }
                if let Some(receipt) = scope.owner.observation_retry(&key, &event)? {
                    return Ok(receipt);
                }
                scope
                    .append(key, vec![event], payloads)
                    .map_err(SessionError::Operation)
            })
            .await?)
    }
}
#[async_trait::async_trait]
impl PreparedToolPort for PreparedTool {
    async fn verify_action(&self, name: &str, arguments: &serde_json::Value) -> anyhow::Result<()> {
        if self.live_name != name || &self.live_arguments != arguments {
            anyhow::bail!("tool_action_changed_after_durable_preparation");
        }
        Ok(())
    }
    async fn request_approval(&self, intent: ApprovalRequestIntent) -> anyhow::Result<()> {
        if !self.prompt {
            anyhow::bail!("approval_not_required");
        }
        self.commit(
            "request",
            EventData::ApprovalRequested {
                request_id: self.request_id.clone(),
                policy_event_id: self.policy_id.clone(),
                display_ref: self.execution_ref.clone(),
                trusted_route_ref: self.route_ref.clone(),
                expires_at: intent.expires_at,
                session_incarnation: self.incarnation.clone(),
            },
            vec![],
        )
        .await?;
        Ok(())
    }
    async fn resolve_approval(
        &self,
        response: ChannelApprovalResponse,
        reason: &str,
        reply_event_id: Option<EventId>,
        evidence: serde_json::Value,
    ) -> anyhow::Result<()> {
        let evidence_decision = evidence
            .get("decision")
            .map(|value| serde_json::from_value::<ApprovalReplyDecision>(value.clone()))
            .transpose()?;
        anyhow::ensure!(
            evidence_decision
                .is_none_or(|decision| decision == ApprovalReplyDecision::from(&response)),
            "approval_resolution_decision_mismatch"
        );
        let approved = matches!(
            response,
            ChannelApprovalResponse::Approve | ChannelApprovalResponse::AlwaysApprove
        );
        let reply = if response != ChannelApprovalResponse::Deny
            || evidence["operator_reply_available"] == true
        {
            Some(
                self.reply_for_resolution(response, reply_event_id, &evidence)
                    .await?,
            )
        } else {
            anyhow::ensure!(
                reply_event_id.is_none(),
                "runtime_approval_resolution_cannot_reference_operator_reply"
            );
            None
        };
        let evidence = retry::payload(
            &self.policy_id,
            "approval-evidence",
            "session.evidence/v1",
            &evidence,
        )?;
        let resolution = if reason == "approval_channel_unsupported" {
            anyhow::ensure!(
                self.live_name == "shell" && !approved && reply.is_none(),
                "invalid_unsupported_channel_fallback"
            );
            ApprovalResolution::ChannelUnsupported {
                confirmation_id: self.request_id.clone(),
                verification_evidence_ref: StoredPayloadRef::from(&evidence),
            }
        } else if approved {
            ApprovalResolution::Approved {
                confirmation_id: self.request_id.clone(),
                validity: "one_execution".into(),
                verification_evidence_ref: StoredPayloadRef::from(&evidence),
            }
        } else {
            ApprovalResolution::Denied {
                reason_key: reason.into(),
                verification_evidence_ref: StoredPayloadRef::from(&evidence),
            }
        };
        let resolution = retry::payload(
            &self.policy_id,
            "approval-resolution",
            "session.approval-resolution/v1",
            &resolution,
        )?;
        let mut event = self.scope.event(
            &self.invocation,
            EventData::ApprovalResolved {
                request_id: self.request_id.clone(),
                resolution_ref: StoredPayloadRef::from(&resolution),
            },
        )?;
        event.causation_id = reply;
        self.commit_observation("resolve", event, vec![evidence, resolution])
            .await?;
        Ok(())
    }
    async fn record_approval_reply(
        &self,
        actor: EventActor,
        response: ChannelApprovalResponse,
        evidence: serde_json::Value,
    ) -> anyhow::Result<EventId> {
        self.record_reply(actor, response, evidence).await
    }
    async fn record_approval_reply_rejected(
        &self,
        classification: String,
        evidence: serde_json::Value,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !classification.is_empty(),
            "approval_rejection_classification_missing"
        );
        let evidence = PayloadRecord::json("session.evidence/v1", &evidence)?;
        let event = self.scope.event(
            &self.invocation,
            EventData::ApprovalReplyRejected {
                request_id: Some(self.request_id.clone()),
                subject_reply_event_id: None,
                classification,
                evidence_ref: Some(StoredPayloadRef::from(&evidence)),
            },
        )?;
        let scope = Arc::clone(&self.scope);
        self.scope
            .owner
            .blocking(move || {
                let lock = scope.owner.lock_for(&scope.stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                scope
                    .resolver
                    .authorize_context(&scope.stream, &scope.branch)?;
                // Late replies remain evidence after cancellation; this append
                // grants no tool execution and never changes approval state.
                scope
                    .append(
                        format!("approval-reply-rejected:{}", event.event_id),
                        vec![event],
                        vec![evidence],
                    )
                    .map_err(SessionError::Operation)?;
                Ok(())
            })
            .await?;
        Ok(())
    }
    async fn invalidate(&self, reason: &str) -> anyhow::Result<()> {
        if self.prompt {
            self.commit(
                "invalidate",
                EventData::ApprovalInvalidated {
                    request_id: self.request_id.clone(),
                    confirmation_id: None,
                    reason_key: reason.into(),
                    evidence_ref: None,
                },
                vec![],
            )
            .await?;
        }
        Ok(())
    }
    async fn start(&self) -> anyhow::Result<Arc<dyn ToolObservationPort>> {
        anyhow::ensure!(
            self.incarnation == self.scope.owner.execution_incarnation,
            "approval_execution_incarnation_changed"
        );
        let scope = Arc::clone(&self.scope);
        let invocation = self.invocation.clone();
        let policy_id = self.policy_id.clone();
        let execution_ref = self.execution_ref.clone();
        let prompt = self.prompt;
        let live_name = self.live_name.clone();
        let live_arguments = self.live_arguments.clone();
        let request_id = self.request_id.clone();
        let observer = self
            .scope
            .owner
            .blocking(move || {
                let lock = scope.owner.lock_for(&scope.stream);
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                scope
                    .owner
                    .require_known_turn(&scope.stream, &scope.branch, Some(&scope.turn))?;
                let run = || -> anyhow::Result<EffectObservationHandle> {
                    let policy = scope
                        .owner
                        .store
                        .read_event(&policy_id)?
                        .ok_or_else(|| anyhow::Error::msg("tool_policy_missing"))?;
                    anyhow::ensure!(
                        matches!(&policy.body.data,
                        EventData::PolicyDecisionRecorded {decision, ..}
                        if matches!(decision.as_str(), "prompt" | "approved" | "not_required")),
                        "tool_policy_denies_execution"
                    );
                    let confirmation = if prompt {
                        let receipt = scope
                            .owner
                            .store
                            .lookup_receipt(&scope.stream, &format!("tool-resolve:{policy_id}"))?
                            .ok_or_else(|| anyhow::Error::msg("approval_resolution_absent"))?;
                        let page = scope.owner.store.read_page(
                            &scope.stream,
                            &EventCursor {
                                stream_id: scope.stream.clone(),
                                sequence: receipt.first.sequence - 1,
                            },
                            Some(&receipt.last),
                            1,
                        )?;
                        Some(
                            page.events
                                .first()
                                .ok_or_else(|| anyhow::Error::msg("approval_resolution_missing"))?
                                .event_id
                                .clone(),
                        )
                    } else {
                        None
                    };
                    let started = scope.event(
                        &invocation,
                        EventData::ToolEffectStarted {
                            policy_event_id: policy_id.clone(),
                            confirmation_event_id: confirmation,
                            execution_ref: execution_ref.clone(),
                        },
                    )?;
                    let head = scope.owner.store.head(&scope.stream)?;
                    let key = format!("tool-start:{policy_id}");
                    scope.resolver.authorize_tool(
                        &EffectIntent {
                            stream_id: scope.stream.clone(),
                            expected_head: head,
                            idempotency_key: key.clone(),
                            event: started.clone(),
                            payloads: vec![],
                        },
                        &live_name,
                        &live_arguments,
                    )?;
                    let current = scope
                        .resolver
                        .tool_authority_evidence(&scope.stream, &live_name)?;
                    let binding = PayloadRecord::json("session.execution-binding/v1", &current)?;
                    let mut events = Vec::new();
                    if let Some(observation) = scope.owner.authority_observation(
                        &scope.stream,
                        &scope.branch,
                        &started.actor,
                        &binding,
                    )? {
                        events.push(observation);
                    }
                    if prompt {
                        events.push(scope.event(
                            &invocation,
                            EventData::ApprovalConsumed {
                                confirmation_id: request_id,
                            },
                        )?);
                    }
                    let started_event_id = started.event_id.clone();
                    events.push(started);
                    scope.append(key, events, vec![binding])?;
                    Ok(EffectObservationHandle {
                        started_event_id,
                        coordinator: Arc::clone(&scope.owner),
                        stream_id: scope.stream.clone(),
                        branch_id: Some(scope.branch.clone()),
                        turn_id: Some(scope.turn.clone()),
                        invocation_id: invocation,
                    })
                };
                run().map_err(SessionError::Operation)
            })
            .await?;
        Ok(Arc::new(observer))
    }
}
#[async_trait::async_trait]
impl ToolObservationPort for EffectObservationHandle {
    fn started_event_id(&self) -> EventId {
        self.started_event_id.clone()
    }
    async fn has_outcome(&self) -> anyhow::Result<bool> {
        let owner = self.coordinator.clone();
        let stream = self.stream_id.clone();
        let invocation = self.invocation_id.clone();
        Ok(self
            .coordinator
            .blocking(move || {
                let history = owner.stream_history(&stream)?;
                let terminal = history.iter().find(|event| {
                    event.invocation_id.as_ref() == Some(&invocation)
                        && matches!(event.body.kind(), EventKind::ToolEffectCompleted
                            | EventKind::ToolEffectFailed | EventKind::InvocationCancelled
                            | EventKind::InvocationOutcomeUnknown)
                });
                match terminal.map(|event| event.body.kind()) {
                    Some(EventKind::InvocationCancelled) => Ok(true),
                    Some(_) => Err(SessionError::StaleTarget("tool outcome already settled; late result requires explicit reconciliation".into())),
                    None => Ok(false),
                }
            })
            .await?)
    }
    async fn cancel_verified(
        &self,
        reason: &str,
        evidence: serde_json::Value,
    ) -> anyhow::Result<AppendReceipt> {
        let owner = self.coordinator.clone();
        let stream = self.stream_id.clone();
        let branch = self.branch_id.clone();
        let turn = self.turn_id.clone();
        let invocation = self.invocation_id.clone();
        let started = self.started_event_id.clone();
        let reason = reason.to_owned();
        Ok(self
            .coordinator
            .blocking(move || {
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                let proof =
                    PayloadRecord::json("session.evidence/v1", &evidence)
                        .map_err(AppendError::from)?;
                let key = format!("tool-verified-cancel:{invocation}");
                if let Some(receipt) = owner.store.lookup_receipt(&stream, &key)? {
                    let prior = owner.store.read_event(receipt.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("empty cancellation receipt".into()))?)?.ok_or_else(|| SessionError::StaleTarget("cancellation receipt missing".into()))?;
                    let matches_reason = matches!(&prior.body.data, EventData::CancellationRequested {reason_key,..} if reason_key == &reason);
                    let terminal = owner.store.read_event(receipt.event_ids.last().ok_or_else(|| SessionError::StoreUnhealthy("empty cancellation receipt".into()))?)?.ok_or_else(|| SessionError::StaleTarget("cancellation terminal missing".into()))?;
                    let same_proof = match &terminal.body.data {
                        EventData::InvocationCancelled {verification_evidence_ref,..} => matches!(owner.store.read_payload(&verification_evidence_ref.payload_id)?, PayloadRead::Present(value) if value.body == proof.body),
                        _ => false,
                    };
                    if !matches_reason || !same_proof {return Err(AppendError::IdempotencyConflict(key).into());}
                    return Ok(receipt);
                }
                let actor = EventActor::System {
                    component: "tool_process_owner".into(),
                };
                let mut requested = SessionEvent::typed(
                    stream.clone(),
                    EventData::CancellationRequested {
                        target_ref: TargetRef::Invocation {
                            invocation_id: invocation.clone(),
                        },
                        expected_incarnation: started.to_string(),
                        reason_key: reason,
                    },
                    actor.clone(),
                )
                .map_err(AppendError::from)?;
                requested.branch_id = branch.clone();
                requested.turn_id = turn.clone();
                requested.invocation_id = Some(invocation.clone());
                requested.causation_id = Some(started);
                let mut cancelled = SessionEvent::typed(
                    stream.clone(),
                    EventData::InvocationCancelled {
                        cancellation_event_id: requested.event_id.clone(),
                        verification_evidence_ref: StoredPayloadRef::from(&proof),
                        observed_partial_ref: None,
                    },
                    actor,
                )
                .map_err(AppendError::from)?;
                cancelled.branch_id = branch;
                cancelled.turn_id = turn;
                cancelled.invocation_id = Some(invocation.clone());
                cancelled.causation_id = Some(requested.event_id.clone());
                let receipt = owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: key,
                    events: vec![requested, cancelled],
                    payloads: vec![proof],
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(receipt)
            })
            .await?)
    }
    async fn observe(
        &self,
        data: EventData,
        payloads: Vec<PayloadRecord>,
    ) -> anyhow::Result<AppendReceipt> {
        if !matches!(
            data.kind(),
            EventKind::ToolEffectProgressObserved
                | EventKind::ToolEffectCompleted
                | EventKind::ToolEffectFailed
                | EventKind::InvocationOutcomeUnknown
        ) {
            anyhow::bail!("tool_observation_kind_mismatch");
        }
        let mut event = SessionEvent::typed(
            self.stream_id.clone(),
            data,
            EventActor::System {
                component: "tool_capture".into(),
            },
        )?;
        event.branch_id = self.branch_id.clone();
        event.turn_id = self.turn_id.clone();
        event.invocation_id = Some(self.invocation_id.clone());
        let owner = Arc::clone(&self.coordinator);
        let stream = self.stream_id.clone();
        let invocation = self.invocation_id.clone();
        Ok(self
            .coordinator
            .blocking(move || owner.append_observation(&stream, &invocation, event, payloads))
            .await?)
    }
}
