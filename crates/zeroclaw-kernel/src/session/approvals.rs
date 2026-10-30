//! Ephemeral projection of approvals that can still authorize an execution.
//! Requests and resolutions in the committed log are the only source of truth.
use super::*;
use std::collections::BTreeMap;
use zeroclaw_api::session_events::{ApprovalResolution, EventData, PayloadRead};

#[derive(Default)]
pub(super) struct UnconsumedApprovals(BTreeMap<String, SessionEvent>);

impl UnconsumedApprovals {
    pub(super) fn observe(
        &mut self,
        event: &SessionEvent,
        store: &dyn SessionEventStore,
    ) -> Result<(), SessionError> {
        match &event.body.data {
            EventData::ApprovalRequested { request_id, .. } => {
                self.0.insert(request_id.clone(), event.clone());
            }
            EventData::ApprovalInvalidated { request_id, .. } => {
                self.0.remove(request_id);
            }
            EventData::ApprovalConsumed { confirmation_id } => {
                self.0.remove(confirmation_id);
            }
            EventData::ApprovalResolved {
                request_id,
                resolution_ref,
            } => {
                let PayloadRead::Present(payload) =
                    store.read_payload(&resolution_ref.payload_id)?
                else {
                    return Err(SessionError::StaleTarget(
                        "approval resolution unavailable".into(),
                    ));
                };
                if zeroclaw_api::session_events::StoredPayloadRef::from(&payload) != *resolution_ref
                {
                    return Err(ReadError::Corrupt(
                        "approval resolution reference mismatch".into(),
                    )
                    .into());
                }
                if matches!(
                    serde_json::from_slice::<ApprovalResolution>(&payload.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                    ApprovalResolution::Denied { .. }
                ) {
                    self.0.remove(request_id);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn into_requests(self) -> impl Iterator<Item = (String, SessionEvent)> {
        self.0.into_iter()
    }
}
