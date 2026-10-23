//! Final transport delivery is an independent effect after execution settles.
use super::engine::EngineSession;
use super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use zeroclaw_api::session_events::capture::{
    DeliveryFuture, DeliveryIntent, is_delivery_definitely_failed,
};
use zeroclaw_api::session_events::*;

impl EngineSession {
    pub(super) async fn deliver_observed(
        &self,
        intent: DeliveryIntent,
        send: DeliveryFuture<'_>,
    ) -> anyhow::Result<serde_json::Value> {
        anyhow::ensure!(
            !intent.transport.is_empty() && !intent.completion_boundary.is_empty(),
            "invalid_delivery_intent"
        );
        let turn = intent.turn_id.as_deref().map(TurnId::parse).transpose()?;
        let payload = PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "transport":intent.transport,"target":intent.target.to_owned(),
                "content":intent.content.to_owned(),"completion_boundary":intent.completion_boundary,
            }),
        )?;
        let mut started = SessionEvent::typed(
            self.stream.clone(),
            EventData::DeliveryStarted {
                authorized_intent_ref: StoredPayloadRef::from(&payload),
            },
            self.actor.clone(),
        )?;
        started.branch_id = Some(self.branch.clone());
        started.turn_id = turn;
        started.invocation_id = Some(InvocationId::new());
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let authority = self.authority.clone();
        let event = started.clone();
        self.owner
            .blocking(move || {
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                SessionAccessPolicy::authorize_actor(
                    authority.as_ref(),
                    &event.actor,
                    &stream,
                    SessionAccess::Write,
                )?;
                authority.authorize_context(
                    &stream,
                    event
                        .branch_id
                        .as_ref()
                        .ok_or(SessionError::InvocationMismatch)?,
                )?;
                // Delivery reports already observed execution, including an
                // uncertain result. It cannot restart a provider/tool effect.
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: format!("delivery-start:{}", event.event_id),
                    events: vec![event],
                    payloads: vec![payload],
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(())
            })
            .await?;
        use futures_util::FutureExt as _;
        let result = std::panic::AssertUnwindSafe(send)
            .catch_unwind()
            .await
            .unwrap_or_else(|_| Err(anyhow::Error::msg("channel_delivery_adapter_panic")));
        let (data, payload) = match &result {
            Ok(receipt) => {
                let payload = PayloadRecord::json("session.evidence/v1", &receipt.clone())?;
                (
                    EventData::DeliveryCompleted {
                        started_event_id: started.event_id.clone(),
                        verified_receipt_ref: StoredPayloadRef::from(&payload),
                    },
                    payload,
                )
            }
            Err(error) => {
                // Most channel clients cannot prove non-delivery from a
                // transport error. Preserve uncertainty rather than asserting
                // the remote side did not accept the message.
                let payload = PayloadRecord::json(
                    "session.error-detail/v1",
                    &serde_json::json!({"diagnostic":self.authority.diagnostic(error),"completion_boundary":intent.completion_boundary}),
                )?;
                if is_delivery_definitely_failed(error) {
                    (
                        EventData::DeliveryFailed {
                            started_event_id: started.event_id.clone(),
                            error_ref: StoredPayloadRef::from(&payload),
                        },
                        payload,
                    )
                } else {
                    (
                        EventData::InvocationOutcomeUnknown {
                            last_observed_event_id: started.event_id.clone(),
                            reason_key: "delivery_acceptance_unverified".into(),
                            observed_partial_ref: None,
                            error_ref: Some(StoredPayloadRef::from(&payload)),
                        },
                        payload,
                    )
                }
            }
        };
        let mut outcome = SessionEvent::typed(self.stream.clone(), data, self.actor.clone())?;
        outcome.branch_id = started.branch_id;
        outcome.turn_id = started.turn_id;
        outcome.invocation_id = started.invocation_id.clone();
        outcome.causation_id = Some(started.event_id);
        let owner = self.owner.clone();
        let stream = self.stream.clone();
        let invocation = started
            .invocation_id
            .ok_or(SessionError::InvocationMismatch)?;
        self.owner
            .blocking(move || {
                owner.append_observation(&stream, &invocation, outcome, vec![payload])?;
                Ok(())
            })
            .await?;
        result.map_err(zeroclaw_api::session_events::capture::capture_stopped)
    }
}
