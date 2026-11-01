//! Finish an already authorized erasure intent. Canonical events are the only
//! work list; completing their deletion does not create a new authorization.
use super::*;
use std::collections::{BTreeMap, HashSet};
use zeroclaw_api::session_events::{Digest, EventData};

#[cfg(test)]
mod tests;

impl SessionCoordinator {
    /// Explicit maintenance entry point, called after admission and at family
    /// startup. Reads never perform cleanup. The backend owns physical deletion
    /// and durability; failures preserve the intent for the next retry.
    pub async fn resume_pending_erasures(
        self: &Arc<Self>,
        stream: SessionStreamId,
    ) -> Result<(), SessionError> {
        let owner = Arc::clone(self);
        self.blocking(move || {
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            owner.finish_erasures_locked(&stream)
        })
        .await
    }

    pub(super) fn finish_erasures_locked(
        &self,
        stream: &SessionStreamId,
    ) -> Result<(), SessionError> {
        let owner = self;
        let history = owner.stream_history(stream)?;
        let mut intents = BTreeMap::new();
        let mut confirmed = HashSet::new();
        for event in history {
            match &event.body.data {
                EventData::PayloadErasureRequested { .. } => {
                    intents.insert(event.event_id.clone(), event);
                }
                EventData::PayloadRedacted {
                    erasure_requested_event_id,
                } => {
                    confirmed.insert(erasure_requested_event_id.clone());
                }
                _ => {}
            }
        }
        for (id, intent) in intents {
            let key = format!("payload-erasure-confirmation:{id}");
            let receipt = owner.store.lookup_receipt(stream, &key)?;
            if confirmed.contains(&id) && receipt.is_none() {
                // Legacy confirmations already completed their backend
                // deletion. Do not invent a second confirmation for them.
                continue;
            }
            let mut event = SessionEvent::typed(
                stream.clone(),
                EventData::PayloadRedacted {
                    erasure_requested_event_id: id.clone(),
                },
                EventActor::System {
                    component: "payload_erasure_cleanup".into(),
                },
            )
            .map_err(AppendError::from)?;
            event.event_id = EventId::parse(format!(
                "payload-erasure-confirmation-{}",
                Digest::from_bytes(id.as_str().as_bytes()).as_str()
            ))
            .map_err(AppendError::from)?;
            event.branch_id = intent.branch_id;
            event.causation_id = Some(id);
            if let Some(receipt) = receipt {
                if receipt.event_ids.len() != 1 {
                    return Err(SessionError::InvalidAdmission(
                        "erasure confirmation receipt shape mismatch".into(),
                    ));
                }
                let prior = owner
                    .store
                    .read_event(&receipt.event_ids[0])?
                    .ok_or_else(|| ReadError::Gap("erasure confirmation missing".into()))?;
                if prior.event_id != event.event_id
                    || prior.stream_id != event.stream_id
                    || prior.branch_id != event.branch_id
                    || prior.causation_id != event.causation_id
                    || prior.actor != event.actor
                    || prior.body != event.body
                {
                    return Err(AppendError::IdempotencyConflict(key).into());
                }
                event = prior;
                event.sequence = 0;
                event.committed_at = 0;
            }
            // Retrying completed confirmations also resumes a SQLite WAL
            // checkpoint that failed after the redaction transaction.
            owner.store.append_batch(AppendRequest {
                stream_id: stream.clone(),
                expected_head: owner.store.head(stream)?,
                idempotency_key: key,
                events: vec![event],
                payloads: vec![],
                erasure_reference_commitment: None,
            })?;
            let _ = owner.wakeups.send(stream.clone());
        }
        Ok(())
    }
}
