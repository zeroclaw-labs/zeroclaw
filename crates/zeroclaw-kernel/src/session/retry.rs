//! Retry identity is derived from a committed operation and its actual body.
//! This creates no second outcome cache: the receipt remains authoritative.
use super::*;
use zeroclaw_api::session_events::{Digest, PayloadId, canonical_json};

pub(super) fn payload(
    operation: &EventId,
    role: &str,
    codec: &str,
    value: &impl serde::Serialize,
) -> Result<PayloadRecord, AppendError> {
    let body = canonical_json(value)?;
    let identity = canonical_json(&(operation, role, codec, Digest::from_bytes(&body)))?;
    let id = PayloadId::parse(format!("retry-{}", Digest::from_bytes(&identity).as_str()))?;
    Ok(PayloadRecord::new(id, codec.to_owned(), body)?)
}

impl SessionCoordinator {
    /// Called under the stream lane. A matching observation retry may reuse
    /// its receipt; a changed outcome must fail, including after recovery.
    pub(super) fn observation_retry(
        &self,
        key: &str,
        candidate: &SessionEvent,
    ) -> Result<Option<AppendReceipt>, SessionError> {
        let Some(receipt) = self.store.lookup_receipt(&candidate.stream_id, key)? else {
            return Ok(None);
        };
        if receipt.event_ids.len() != 1 {
            return Err(SessionError::InvalidAdmission(
                "observation receipt shape mismatch".into(),
            ));
        }
        let event = self
            .store
            .read_event(&receipt.event_ids[0])?
            .ok_or_else(|| SessionError::StaleTarget("observation receipt event missing".into()))?;
        if event.stream_id != candidate.stream_id
            || event.branch_id != candidate.branch_id
            || event.turn_id != candidate.turn_id
            || event.invocation_id != candidate.invocation_id
            || event.causation_id != candidate.causation_id
            || event.actor != candidate.actor
            || event.body != candidate.body
        {
            return Err(AppendError::IdempotencyConflict(key.into()).into());
        }
        Ok(Some(receipt))
    }
}
