use super::*;

fn approval_request(
    scope: &ToolScope,
    policy_id: &EventId,
    request_id: &str,
) -> anyhow::Result<SessionEvent> {
    let receipt = scope
        .owner
        .store
        .lookup_receipt(&scope.stream, &format!("tool-request:{policy_id}"))?
        .ok_or_else(|| anyhow::Error::msg("approval_request_not_committed"))?;
    anyhow::ensure!(
        receipt.event_ids.len() == 1,
        "approval_request_receipt_shape_mismatch"
    );
    let event = scope
        .owner
        .store
        .read_event(&receipt.event_ids[0])?
        .ok_or_else(|| anyhow::Error::msg("approval_request_event_missing"))?;
    anyhow::ensure!(
        matches!(&event.body.data, EventData::ApprovalRequested { request_id: actual, .. } if actual == request_id),
        "approval_request_identity_mismatch"
    );
    Ok(event)
}

fn verify_reply(
    scope: &ToolScope,
    requested: &SessionEvent,
    reply_event_id: &EventId,
    response: &ChannelApprovalResponse,
) -> anyhow::Result<()> {
    let event = scope
        .owner
        .store
        .read_event(reply_event_id)?
        .ok_or_else(|| anyhow::Error::msg("approval_reply_event_missing"))?;
    anyhow::ensure!(
        event.stream_id == requested.stream_id
            && event.branch_id == requested.branch_id
            && event.turn_id == requested.turn_id
            && event.invocation_id == requested.invocation_id
            && event.causation_id.as_ref() == Some(&requested.event_id),
        "approval_resolution_reply_scope_mismatch"
    );
    let EventData::ControlInputRecorded { control_ref, .. } = &event.body.data else {
        anyhow::bail!("approval_reply_event_kind_mismatch");
    };
    anyhow::ensure!(
        control_ref.codec == "session.approval-reply/v1",
        "approval_reply_payload_codec_mismatch"
    );
    let reply: ApprovalReplyPayload = serde_json::from_slice(&scope.payload(control_ref)?)?;
    anyhow::ensure!(
        reply.decision == ApprovalReplyDecision::from(response),
        "approval_resolution_disagrees_with_recorded_reply"
    );
    match response {
        ChannelApprovalResponse::DenyWithEdit { replacement } => {
            let reference = reply
                .replacement_ref
                .as_ref()
                .ok_or_else(|| anyhow::Error::msg("approval_replacement_content_unavailable"))?;
            anyhow::ensure!(
                reference.codec == "session.text/v1",
                "approval_replacement_payload_codec_mismatch"
            );
            anyhow::ensure!(
                scope.payload(reference)? == replacement.to_owned().as_bytes(),
                "approval_resolution_disagrees_with_recorded_replacement"
            );
        }
        _ => anyhow::ensure!(
            reply.replacement_ref.is_none(),
            "approval_reply_has_unexpected_replacement"
        ),
    }
    Ok(())
}

fn has_prior_reply(scope: &ToolScope, requested: &SessionEvent) -> anyhow::Result<bool> {
    let through = scope.owner.store.head(&scope.stream)?;
    let mut after = EventCursor {
        stream_id: scope.stream.clone(),
        sequence: requested.sequence,
    };
    while after.sequence < through.sequence {
        let page = scope
            .owner
            .store
            .read_page(&scope.stream, &after, Some(&through), 256)?;
        let last = page
            .events
            .last()
            .ok_or_else(|| anyhow::Error::msg("approval_reply_history_did_not_advance"))?;
        anyhow::ensure!(
            last.sequence > after.sequence,
            "approval_reply_history_did_not_advance"
        );
        let next_sequence = last.sequence;
        if page.events.iter().any(|event| {
            event.causation_id.as_ref() == Some(&requested.event_id)
                && matches!(&event.body.data, EventData::ControlInputRecorded { control_ref, .. } if control_ref.codec == "session.approval-reply/v1")
        }) {
            return Ok(true);
        }
        after.sequence = next_sequence;
    }
    Ok(false)
}

fn append_reply(
    scope: &ToolScope,
    invocation: &InvocationId,
    requested: &EventId,
    actor: EventActor,
    response: ChannelApprovalResponse,
    evidence: serde_json::Value,
) -> anyhow::Result<EventId> {
    scope
        .resolver
        .authorize_context(&scope.stream, &scope.branch)?;
    scope
        .owner
        .require_known_turn(&scope.stream, &scope.branch, Some(&scope.turn))?;
    let replacement = match &response {
        ChannelApprovalResponse::DenyWithEdit { replacement } => Some(PayloadRecord::new(
            PayloadId::new(),
            "session.text/v1".into(),
            replacement.to_owned().into_bytes(),
        )?),
        _ => None,
    };
    let control = PayloadRecord::json(
        "session.approval-reply/v1",
        &ApprovalReplyPayload {
            decision: ApprovalReplyDecision::from(&response),
            replacement_ref: replacement.as_ref().map(StoredPayloadRef::from),
        },
    )?;
    let ingress = PayloadRecord::json("session.evidence/v1", &evidence)?;
    let mut event = scope.event(
        invocation,
        EventData::ControlInputRecorded {
            control_ref: StoredPayloadRef::from(&control),
            expected_target_ref: TargetRef::Invocation {
                invocation_id: invocation.clone(),
            },
            ingress_ref: StoredPayloadRef::from(&ingress),
        },
    )?;
    event.actor = actor;
    event.causation_id = Some(requested.clone());
    // Each claimed transport reply is its own observation. Request identity
    // remains canonical; it is not an idempotency key for all respondents.
    let key = format!("approval-reply:{}", event.event_id);
    let mut payloads = vec![control, ingress];
    payloads.extend(replacement);
    let receipt = scope.append(key, vec![event], payloads)?;
    anyhow::ensure!(
        receipt.event_ids.len() == 1,
        "approval_reply_receipt_shape_mismatch"
    );
    Ok(receipt.event_ids[0].clone())
}

impl PreparedTool {
    pub(super) async fn record_reply(
        &self,
        actor: EventActor,
        response: ChannelApprovalResponse,
        evidence: serde_json::Value,
    ) -> anyhow::Result<EventId> {
        anyhow::ensure!(self.prompt, "approval_not_required");
        let scope = Arc::clone(&self.scope);
        let policy_id = self.policy_id.clone();
        let request_id = self.request_id.clone();
        let invocation = self.invocation.clone();
        Ok(self
            .scope
            .owner
            .blocking(move || {
                let lock = scope.owner.lock_for(&scope.stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                let record = || -> anyhow::Result<EventId> {
                    let requested = approval_request(&scope, &policy_id, &request_id)?;
                    append_reply(
                        &scope,
                        &invocation,
                        &requested.event_id,
                        actor,
                        response,
                        evidence,
                    )
                };
                record().map_err(SessionError::Operation)
            })
            .await?)
    }

    pub(super) async fn reply_for_resolution(
        &self,
        response: ChannelApprovalResponse,
        reply_event_id: Option<EventId>,
        evidence: &serde_json::Value,
    ) -> anyhow::Result<EventId> {
        anyhow::ensure!(
            evidence["operator_reply_available"] != false,
            "approval_resolution_has_no_operator_reply"
        );
        let scope = Arc::clone(&self.scope);
        let policy_id = self.policy_id.clone();
        let request_id = self.request_id.clone();
        let invocation = self.invocation.clone();
        Ok(self.scope.owner.blocking(move || {
            let lock = scope.owner.lock_for(&scope.stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let resolve = || -> anyhow::Result<EventId> {
                let requested = approval_request(&scope, &policy_id, &request_id)?;
                let reply = if let Some(reply) = reply_event_id {
                    reply
                } else if let Some(receipt) = scope.owner.store.lookup_receipt(
                    &scope.stream, &format!("tool-resolve:{policy_id}"),
                )? {
                    anyhow::ensure!(receipt.event_ids.len() == 1, "approval_resolution_receipt_shape_mismatch");
                    // A completed legacy resolution may be retried without a
                    // delivery reference; its existing cause is authoritative.
                    scope.owner.store.read_event(&receipt.event_ids[0])?
                        .and_then(|event| event.causation_id)
                        .ok_or_else(|| anyhow::Error::msg("approval_resolution_reply_reference_missing"))?
                } else {
                    anyhow::ensure!(!has_prior_reply(&scope, &requested)?, "approval_resolution_reply_reference_missing");
                    // Compatibility adapters can report an operator answer
                    // without identity or a durable delivery reference.
                    return append_reply(
                        &scope,
                        &invocation,
                        &requested.event_id,
                        EventActor::System { component: "approval_identity_unavailable".into() },
                        response,
                        serde_json::json!({"identity_unavailable_reason":"reply_adapter_did_not_supply_authenticated_identity"}),
                    );
                };
                verify_reply(&scope, &requested, &reply, &response)?;
                Ok(reply)
            };
            resolve().map_err(SessionError::Operation)
        }).await?)
    }
}
