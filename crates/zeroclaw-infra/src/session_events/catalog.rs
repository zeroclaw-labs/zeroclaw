//! Transport-neutral session metadata projected from the active event store.
//! The installation lock serializes conditional metadata changes and cutover.
use super::installation::{InstallationGuard, family_identity};
use super::*;
use std::collections::BTreeMap;
use zeroclaw_api::session_events::{
    BranchId, CaptureMode, EventActor, EventData, SessionId, StoredPayloadRef,
};

pub struct SessionCatalog {
    guard: InstallationGuard,
    store: Arc<dyn SessionEventStore>,
}

use zeroclaw_api::session_events::catalog::SessionReducer;
pub use zeroclaw_api::session_events::catalog::{Session, SessionDetail};

/// Common read surface for all protocol entrances.
pub struct SessionQuery<'a> {
    catalog: &'a SessionCatalog,
}

/// Common metadata/lifecycle command surface for all protocol entrances.
pub struct SessionCommand<'a> {
    catalog: &'a SessionCatalog,
}

impl SessionCatalog {
    pub fn open(
        data_dir: &Path,
        stores: &super::family::EventStorePool,
    ) -> Result<Option<Self>, AppendError> {
        let guard = InstallationGuard::acquire(&data_dir.join("sessions"))?;
        if guard.phase().is_none() {
            guard.require_legacy().map_err(jsonl::storage)?;
            return Ok(None);
        }
        let store = stores.open_installation(&guard)?;
        Ok(Some(Self { guard, store }))
    }

    pub fn store(&self) -> &dyn SessionEventStore {
        self.store.as_ref()
    }

    pub fn query(&self) -> SessionQuery<'_> {
        SessionQuery { catalog: self }
    }

    pub fn commands(&self) -> SessionCommand<'_> {
        SessionCommand { catalog: self }
    }
}

impl SessionQuery<'_> {
    pub fn families(&self) -> Result<BTreeMap<String, SessionStreamId>, AppendError> {
        let (backend, _) = self.catalog.guard.active()?;
        let directory = self
            .catalog
            .guard
            .directory()
            .parent()
            .ok_or_else(|| AppendError::Corrupt("session directory parent missing".into()))?;
        installation::families(directory, backend)
    }

    pub fn detail(&self, stream: &SessionStreamId) -> Result<Option<SessionDetail>, ReadError> {
        self.project(stream, true)
    }

    pub fn summary(&self, stream: &SessionStreamId) -> Result<Option<Session>, ReadError> {
        Ok(self.project(stream, false)?.map(|detail| detail.session))
    }

    fn project(
        &self,
        stream: &SessionStreamId,
        include_context: bool,
    ) -> Result<Option<SessionDetail>, ReadError> {
        let head = self.catalog.store.head(stream)?;
        if head.sequence == 0 {
            return Ok(None);
        }
        let mut reducer = SessionReducer::new(stream.clone(), include_context);
        let mut after = EventCursor::beginning(stream.clone());
        loop {
            let page = self
                .catalog
                .store
                .read_page(stream, &after, Some(&head), 256)?;
            for event in &page.events {
                let seed_count = if let EventData::ContextProjectionCreated {
                    reason_key,
                    result_ref,
                    ..
                } = &event.body.data
                    && (reason_key == "host_seed" || reason_key == "legacy_import")
                {
                    let body: serde_json::Value = serde_json::from_slice(
                        &zeroclaw_api::session_events::context_read::present(
                            self.catalog.store(),
                            result_ref,
                        )?
                        .body,
                    )
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    Some(
                        body.get(if reason_key == "host_seed" {
                            "seed_messages"
                        } else {
                            "legacy_messages"
                        })
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| ReadError::Corrupt("seed identities missing".into()))?
                        .len(),
                    )
                } else {
                    None
                };
                reducer.apply(event, seed_count)?;
                if let EventData::TurnAccepted { ingress_ref, .. } = &event.body.data {
                    let value = self.metadata_value(ingress_ref)?;
                    reducer.observe_ingress(&value)?;
                }
            }
            if !page.has_more {
                break;
            }
            after.sequence = page
                .events
                .last()
                .ok_or_else(|| ReadError::Gap("empty catalog page".into()))?
                .sequence;
        }
        let pending: Vec<_> = reducer
            .pending_metadata()
            .map(|(field, _, reference)| (field.to_owned(), reference.clone()))
            .collect();
        for (field, reference) in pending {
            let value = self.metadata_value(&reference)?;
            reducer.resolve_metadata(&field, value)?;
        }
        reducer.finish()
    }

    fn metadata_value(&self, reference: &StoredPayloadRef) -> Result<serde_json::Value, ReadError> {
        let value = match self.catalog.store.read_payload(&reference.payload_id)? {
            PayloadRead::Present(value) if StoredPayloadRef::from(&value) == *reference => value,
            PayloadRead::Present(_) => {
                return Err(ReadError::Corrupt("metadata reference mismatch".into()));
            }
            PayloadRead::ErasurePending | PayloadRead::Redacted
                if zeroclaw_api::session_events::payloads::content_leaf(&reference.codec) =>
            {
                return Ok(serde_json::Value::Null);
            }
            _ => return Err(ReadError::Corrupt("structural metadata unavailable".into())),
        };
        if value.codec == "session.text/v1" {
            Ok(serde_json::Value::String(
                String::from_utf8(value.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?,
            ))
        } else {
            serde_json::from_slice(&value.body)
                .map_err(|error| ReadError::Corrupt(error.to_string()))
        }
    }
}

pub(crate) fn metadata_payload(
    field: &str,
    value: serde_json::Value,
) -> Result<PayloadRecord, AppendError> {
    let structural = matches!(
        field,
        "family_key"
            | "principal_id"
            | "created_at"
            | "last_activity"
            | "owning_agent"
            | "token_count"
            | "interaction_surface"
    );
    let codec = if structural {
        "session.authority/v1"
    } else {
        "session.bytes/v1"
    };
    Ok(PayloadRecord::json(codec, &value)?)
}

impl SessionCommand<'_> {
    pub fn create(
        &self,
        family: &str,
        alias: &str,
        fields: Vec<(&str, serde_json::Value)>,
    ) -> Result<(), AppendError> {
        let stream = upgrade::stream_id(family)?;
        if self
            .catalog
            .store
            .head(&stream)
            .map_err(upgrade::storage)?
            .sequence
            != 0
        {
            return Err(AppendError::InvalidTransition(
                "session already exists".into(),
            ));
        }
        let branch = BranchId::parse(format!("branch-{stream}"))?;
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                session_id: SessionId::parse(format!("session-{stream}"))?,
                owning_agent: alias.into(),
                capture_mode: CaptureMode::Native,
            },
            EventActor::System {
                component: "session_catalog".into(),
            },
        )?;
        root.branch_id = Some(branch.clone());
        let (identity, identity_payload) = family_identity(stream.clone(), family)?;
        let body = PayloadRecord::json(
            "session.context/v1",
            &serde_json::json!({"message_refs":[],"history_has_trim_breadcrumb":false}),
        )?;
        let mut context = SessionEvent::typed(
            stream.clone(),
            EventData::ContextProjectionCreated {
                source_ranges: vec![],
                summary_ref: None,
                budget: 0,
                reason_key: "initial_context".into(),
                algorithm_version: "native-entrance/v1".into(),
                result_ref: StoredPayloadRef::from(&body),
                summary_source_refs: vec![],
                transform_started_event_id: None,
            },
            EventActor::default(),
        )?;
        context.branch_id = Some(branch);
        let mut events = vec![root, identity];
        let mut payloads = vec![identity_payload, body];
        Self::metadata_events(&stream, fields, &mut events, &mut payloads)?;
        events.push(context);
        self.append(
            &stream,
            EventCursor::beginning(stream.clone()),
            events,
            payloads,
        )
    }

    pub fn change(
        &self,
        record: &Session,
        fields: Vec<(&str, serde_json::Value)>,
    ) -> Result<(), AppendError> {
        let mut events = Vec::new();
        let mut payloads = Vec::new();
        Self::metadata_events(record.stream(), fields, &mut events, &mut payloads)?;
        self.append(record.stream(), record.head.clone(), events, payloads)
    }

    fn metadata_events(
        stream: &SessionStreamId,
        fields: Vec<(&str, serde_json::Value)>,
        events: &mut Vec<SessionEvent>,
        payloads: &mut Vec<PayloadRecord>,
    ) -> Result<(), AppendError> {
        for (field, value) in fields {
            let body = metadata_payload(field, value)?;
            events.push(SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: field.into(),
                    new_value_ref: StoredPayloadRef::from(&body),
                },
                EventActor::System {
                    component: "session_catalog".into(),
                },
            )?);
            payloads.push(body);
        }
        Ok(())
    }

    pub fn close(&self, record: &Session, reason: &str) -> Result<(), AppendError> {
        let event = SessionEvent::typed(
            record.stream().clone(),
            EventData::SessionClosed {
                reason_key: reason.into(),
            },
            EventActor::System {
                component: "session_catalog".into(),
            },
        )?;
        self.append(record.stream(), record.head.clone(), vec![event], vec![])
    }

    fn append(
        &self,
        stream: &SessionStreamId,
        head: EventCursor,
        events: Vec<SessionEvent>,
        payloads: Vec<PayloadRecord>,
    ) -> Result<(), AppendError> {
        if events.is_empty() {
            return Ok(());
        }
        self.catalog.store.append_batch(AppendRequest {
            stream_id: stream.clone(),
            expected_head: head,
            idempotency_key: format!("catalog:{}", EventId::new()),
            events,
            payloads,
            erasure_reference_commitment: None,
        })?;
        Ok(())
    }
}
