//! Observe the revision carried by an already-frozen host binding. The
//! binding creates the historical observation; live authority stays external.
use super::*;
use zeroclaw_api::session_events::*;
impl SessionCoordinator {
    pub(super) fn authority_observation(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
        actor: &EventActor,
        binding: &PayloadRecord,
    ) -> Result<Option<SessionEvent>, SessionError> {
        let value: serde_json::Value = serde_json::from_slice(&binding.body)
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let revision = value.get("config_revision");
        let auth_revision = value.get("auth_policy_revision");
        if revision.is_none() && auth_revision.is_none() {
            return Ok(None);
        }
        // Auth counters belong to the existing config authority epoch; a
        // restarted authority cannot equate them with a prior process.
        if revision.is_none_or(|revision| {
            !revision
                .get("epoch")
                .is_some_and(serde_json::Value::is_string)
                || revision
                    .get("sequence")
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
        }) || auth_revision.is_some_and(|revision| {
            revision
                .get("generation")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|generation| generation == 0)
                || revision
                    .get("accepted_revision")
                    .and_then(serde_json::Value::as_u64)
                    .is_none()
        }) {
            return Err(SessionError::InvalidAdmission(
                "authority revision evidence invalid".into(),
            ));
        }
        let previous = self.store.read_latest_event(
            stream,
            branch,
            None,
            &EventKind::AuthorityRevisionObserved,
        )?;
        if let Some(previous) = &previous {
            let EventData::AuthorityRevisionObserved {
                revision_evidence_ref,
                ..
            } = &previous.body.data
            else {
                return Err(SessionError::StoreUnhealthy(
                    "authority observation kind mismatch".into(),
                ));
            };
            let PayloadRead::Present(payload) =
                self.store.read_payload(&revision_evidence_ref.payload_id)?
            else {
                return Err(SessionError::StoreUnhealthy(
                    "authority revision skeleton missing".into(),
                ));
            };
            let prior: serde_json::Value = serde_json::from_slice(&payload.body)
                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
            if prior.get("config_revision") == revision
                && prior.get("auth_policy_revision") == auth_revision
            {
                return Ok(None);
            }
        }
        let mut event = SessionEvent::typed(
            stream.clone(),
            EventData::AuthorityRevisionObserved {
                revision_evidence_ref: StoredPayloadRef::from(binding),
                previous_observation_event_id: previous.map(|event| event.event_id),
                change_record_ref: None,
                affected_scope_ref: TargetRef::Session {
                    stream_id: stream.clone(),
                },
                reason_key: "current_authority_revision_observed".into(),
            },
            actor.clone(),
        )
        .map_err(AppendError::from)?;
        event.branch_id = Some(branch.clone());
        Ok(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn authority_revisions_keep_legacy_bindings_and_scope_auth_counters_to_config_epoch() {
        for backend in ["sqlite", "jsonl"] {
            let directory = tempfile::TempDir::new().expect("directory");
            let store = zeroclaw_infra::session_events::open_event_store(directory.path(), backend)
                .expect("store");
            let owner = SessionCoordinator::new(store);
            let stream = SessionStreamId::new();
            let branch = BranchId::new();
            let mut root = SessionEvent::typed(
                stream.clone(),
                EventData::SessionStarted {
                    session_id: SessionId::new(),
                    owning_agent: "fixture".into(),
                    capture_mode: CaptureMode::Native,
                },
                EventActor::default(),
            )
            .expect("root");
            root.branch_id = Some(branch.clone());
            owner
                .append(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: EventCursor::beginning(stream.clone()),
                    idempotency_key: "root".into(),
                    events: vec![root],
                    payloads: vec![],
                    erasure_reference_commitment: None,
                })
                .expect("create");
            let revision =
                zeroclaw_config::live::LiveConfig::new(Default::default()).published_revision();
            let restarted =
                zeroclaw_config::live::LiveConfig::new(Default::default()).published_revision();
            let legacy = json!({"config_revision":revision});
            let current = json!({"config_revision":revision,"auth_policy_revision":{"generation":1,"accepted_revision":0}});
            for (index, (value, expected)) in [
                (legacy.clone(), true),
                (legacy, false),
                (current.clone(), true),
                (current, false),
                (json!({"config_revision":restarted,"auth_policy_revision":{"generation":1,"accepted_revision":0}}), true),
            ].into_iter().enumerate() {
                let binding = PayloadRecord::json("session.execution-binding/v1", &value).expect("binding");
                let observed = owner.authority_observation(&stream, &branch, &EventActor::default(), &binding).expect("observation");
                assert_eq!(observed.is_some(), expected);
                if let Some(event) = observed {
                    let EventData::AuthorityRevisionObserved {revision_evidence_ref, ..} = &event.body.data else {unreachable!()};
                    assert_eq!(*revision_evidence_ref, StoredPayloadRef::from(&binding));
                    owner.append(AppendRequest {
                        stream_id:stream.clone(), expected_head:owner.store.head(&stream).expect("head"),
                        idempotency_key:format!("revision-{index}"), events:vec![event],
                        payloads:vec![binding], erasure_reference_commitment:None,
                    }).expect("commit observation");
                }
            }
            for malformed in [
                json!({"auth_policy_revision":{"generation":1,"accepted_revision":0}}),
                json!({"config_revision":revision,"auth_policy_revision":{"generation":0,"accepted_revision":0}}),
                json!({"config_revision":revision,"auth_policy_revision":{"generation":1}}),
                json!({"config_revision":revision,"auth_policy_revision":{"generation":1,"accepted_revision":"0"}}),
            ] {
                let binding = PayloadRecord::json("session.execution-binding/v1", &malformed)
                    .expect("binding");
                assert!(matches!(
                    owner.authority_observation(&stream, &branch, &EventActor::default(), &binding),
                    Err(SessionError::InvalidAdmission(_))
                ));
            }
        }
    }
}
