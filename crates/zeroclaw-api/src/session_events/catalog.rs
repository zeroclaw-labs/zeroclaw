//! Common session model and replay reducer. Events remain the only authority;
//! this projection is rebuilt on demand and is never persisted independently.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Session {
    /// Derived streams have no local owner until an attribution event exists.
    /// Their execution authority is resolved from the committed parent request.
    pub owning_agent: Option<String>,
    pub fields: BTreeMap<String, Value>,
    pub closed: bool,
    pub deleted: bool,
    pub created_at: i64,
    pub last_activity: i64,
    pub head: EventCursor,
    pub message_count: usize,
    pub running: bool,
}

impl Session {
    pub fn stream(&self) -> &SessionStreamId {
        &self.head.stream_id
    }
    pub fn title(&self) -> Option<&str> {
        self.fields.get("title").and_then(Value::as_str)
    }
    pub fn channel_id(&self) -> Option<&str> {
        self.fields.get("channel_id").and_then(Value::as_str)
    }
    pub fn family_key(&self) -> Option<&str> {
        self.fields.get("family_key").and_then(Value::as_str)
    }

    pub fn field<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
        default: T,
    ) -> Result<T, ReadError> {
        self.fields.get(key).map_or(Ok(default), |value| {
            serde_json::from_value(value.clone())
                .map_err(|error| ReadError::Corrupt(error.to_string()))
        })
    }
}

/// Details attach a context reference without duplicating session metadata.
#[derive(Clone, Debug)]
pub struct SessionDetail {
    pub session: Session,
    pub context: Option<SessionEvent>,
}

impl std::ops::Deref for SessionDetail {
    type Target = Session;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

/// Storage-independent reduction of a contiguous event prefix. Only the last
/// effective value of each metadata field is resolved, avoiding repeated plan/body reads.
pub struct SessionReducer {
    stream: SessionStreamId,
    record: Option<SessionDetail>,
    metadata: BTreeMap<String, (u64, StoredPayloadRef, bool)>,
    importing: bool,
    include_context: bool,
    last_activity_sequence: u64,
    channel_sequence: u64,
    sequence: u64,
    history: projection::HistoryProjection,
    imported_messages: usize,
}

impl SessionReducer {
    pub fn new(stream: SessionStreamId, include_context: bool) -> Self {
        Self {
            history: projection::HistoryProjection::empty(stream.clone()),
            imported_messages: 0,
            stream,
            record: None,
            metadata: BTreeMap::new(),
            importing: false,
            include_context,
            last_activity_sequence: 0,
            channel_sequence: 0,
            sequence: 0,
        }
    }

    /// `seed_count` is the structural identity count of a host seed. No message
    /// text, reasoning or attachment body is needed for the summary projection.
    pub fn apply(
        &mut self,
        event: &SessionEvent,
        seed_count: Option<usize>,
    ) -> Result<(), ReadError> {
        if event.stream_id != self.stream || self.sequence.checked_add(1) != Some(event.sequence) {
            return Err(ReadError::Gap(
                "session projection received noncontiguous events".into(),
            ));
        }
        let root = match &event.body.data {
            EventData::SessionStarted {
                owning_agent,
                capture_mode,
                ..
            } => Some((
                Some(owning_agent.as_str()),
                *capture_mode == CaptureMode::LegacyMessageOnly,
            )),
            EventData::DerivedSessionStarted { .. } => Some((None, false)),
            _ => None,
        };
        if let Some((owning_agent, importing)) = root {
            if self.record.is_some() {
                return Err(ReadError::Corrupt("duplicate session root".into()));
            }
            self.importing = importing;
            let mut fields = BTreeMap::new();
            if let EventActor::Principal { principal_id } = &event.actor {
                fields.insert("principal_id".into(), serde_json::json!(principal_id));
            }
            self.record = Some(SessionDetail {
                session: Session {
                    owning_agent: owning_agent.map(str::to_owned),
                    fields,
                    closed: false,
                    deleted: false,
                    created_at: event.committed_at,
                    last_activity: event.committed_at,
                    head: EventCursor::beginning(self.stream.clone()),
                    message_count: 0,
                    running: false,
                },
                context: None,
            });
        }
        let detail = self
            .record
            .as_mut()
            .ok_or_else(|| ReadError::Corrupt("session root missing".into()))?;
        let state = &mut detail.session;
        if !self.importing
            && !matches!(&event.actor, EventActor::System {component} if component == "session_upgrade")
            && !matches!(&event.body.data, EventData::SessionMetadataChanged {field, ..} if field == "family_key")
        {
            state.last_activity = event.committed_at;
            self.last_activity_sequence = event.sequence;
        }
        match &event.body.data {
            EventData::SessionMetadataChanged {
                field,
                new_value_ref,
            } => {
                let imported = matches!(&event.actor, EventActor::System { component } if component == "session_upgrade");
                // Imported observations precede native facts semantically even
                // when an interrupted upgrade appends them at the stream tail.
                // Keep their events, but never let them replace a native value.
                if !imported
                    || self
                        .metadata
                        .get(field)
                        .is_none_or(|(_, _, was_imported)| *was_imported)
                {
                    self.metadata.insert(
                        field.clone(),
                        (event.sequence, new_value_ref.clone(), imported),
                    );
                }
            }
            EventData::ContextProjectionCreated {
                reason_key,
                algorithm_version,
                ..
            } => {
                if self.include_context {
                    detail.context = Some(event.clone());
                }
                if reason_key == "host_seed" || reason_key == "legacy_import" {
                    let count = seed_count
                        .ok_or_else(|| ReadError::Corrupt("seed identities missing".into()))?;
                    self.imported_messages = if reason_key == "legacy_import" {
                        count
                    } else {
                        self.imported_messages.checked_add(count).ok_or_else(|| {
                            ReadError::Corrupt("session message count overflow".into())
                        })?
                    };
                } else if seed_count.is_some() {
                    return Err(ReadError::Corrupt("unexpected seed identity count".into()));
                }
                if reason_key == "legacy_import"
                    || reason_key == "legacy_acp_context"
                    || (reason_key == "host_seed" && algorithm_version != "legacy-acp/v1")
                {
                    self.importing = false;
                }
            }
            EventData::SessionClosed { reason_key } => {
                // TTL releases an idle execution window, not the logical
                // conversation. Explicit closures and deletion stay terminal.
                state.closed |= reason_key != "session_ttl_expired";
                state.deleted |= reason_key == "session_deleted";
            }
            _ => {}
        }
        self.history.apply_event(event)?;
        self.sequence = event.sequence;
        state.head.sequence = event.sequence;
        Ok(())
    }

    pub fn pending_metadata(&self) -> impl Iterator<Item = (&str, u64, &StoredPayloadRef)> {
        self.metadata
            .iter()
            .map(|(field, (sequence, reference, _))| (field.as_str(), *sequence, reference))
    }

    /// Channel attribution is derived from accepted ingress, without another
    /// persisted metadata record. Unknown transports leave existing facts intact.
    pub fn observe_ingress(&mut self, ingress: &Value) -> Result<(), ReadError> {
        let Some(channel) = ingress.get("transport").and_then(|v| v.get("channel")) else {
            return Ok(());
        };
        let (Some(kind), Some(alias)) = (
            channel.get("kind").and_then(Value::as_str),
            channel.get("alias").and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        let state = &mut self
            .record
            .as_mut()
            .ok_or_else(|| ReadError::Corrupt("session root missing".into()))?
            .session;
        self.channel_sequence = self.sequence;
        state.fields.insert(
            "channel_id".into(),
            Value::String(if alias.is_empty() {
                kind.into()
            } else {
                format!("{kind}.{alias}")
            }),
        );
        state.fields.insert(
            "sender_id".into(),
            ingress.get("sender").cloned().unwrap_or(Value::Null),
        );
        Ok(())
    }

    pub fn resolve_metadata(&mut self, field: &str, value: Value) -> Result<(), ReadError> {
        let (sequence, _, imported) = self
            .metadata
            .remove(field)
            .ok_or_else(|| ReadError::Corrupt("unexpected metadata resolution".into()))?;
        if matches!(field, "channel_id" | "sender_id")
            && self.channel_sequence != 0
            && (imported || sequence < self.channel_sequence)
        {
            return Ok(());
        }
        let state = &mut self
            .record
            .as_mut()
            .ok_or_else(|| ReadError::Corrupt("session root missing".into()))?
            .session;
        match field {
            "created_at" => {
                state.created_at = value
                    .as_i64()
                    .ok_or_else(|| ReadError::Corrupt("created_at is not an integer".into()))?
            }
            "last_activity" => {
                let timestamp = value
                    .as_i64()
                    .ok_or_else(|| ReadError::Corrupt("last_activity is not an integer".into()))?;
                if sequence >= self.last_activity_sequence {
                    state.last_activity = timestamp;
                }
            }
            "owning_agent" => {
                state.owning_agent = Some(
                    value
                        .as_str()
                        .ok_or_else(|| ReadError::Corrupt("owning_agent is not a string".into()))?
                        .into(),
                )
            }
            _ => {
                state.fields.insert(field.into(), value);
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Option<SessionDetail>, ReadError> {
        if !self.metadata.is_empty() {
            return Err(ReadError::Corrupt("session metadata unresolved".into()));
        }
        if let Some(detail) = self.record.as_mut() {
            detail.session.message_count = self
                .imported_messages
                .checked_add(self.history.content.len())
                .ok_or_else(|| ReadError::Corrupt("session message count overflow".into()))?;
            detail.session.running = self.history.turns.values().any(|turn| {
                matches!(
                    turn.status,
                    projection::TurnStatus::Queued | projection::TurnStatus::Running
                )
            });
        }
        Ok(self.record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(stream: &SessionStreamId, sequence: u64, data: EventData) -> SessionEvent {
        let mut event =
            SessionEvent::typed(stream.clone(), data, EventActor::default()).expect("event");
        event.sequence = sequence;
        event.committed_at = sequence as i64 * 100;
        event
    }

    fn root(stream: &SessionStreamId, legacy: bool) -> SessionEvent {
        event(
            stream,
            1,
            EventData::SessionStarted {
                session_id: SessionId::new(),
                owning_agent: "agent".into(),
                capture_mode: if legacy {
                    CaptureMode::LegacyMessageOnly
                } else {
                    CaptureMode::Native
                },
            },
        )
    }

    fn metadata(
        stream: &SessionStreamId,
        sequence: u64,
        field: &str,
        value: Value,
    ) -> (SessionEvent, Value) {
        let payload = PayloadRecord::json("session.authority/v1", &value).expect("metadata");
        (
            event(
                stream,
                sequence,
                EventData::SessionMetadataChanged {
                    field: field.into(),
                    new_value_ref: StoredPayloadRef::from(&payload),
                },
            ),
            value,
        )
    }

    fn finish(mut reducer: SessionReducer, values: BTreeMap<u64, Value>) -> SessionDetail {
        let pending: Vec<_> = reducer
            .pending_metadata()
            .map(|(field, seq, _)| (field.to_owned(), seq))
            .collect();
        for (field, seq) in pending {
            reducer
                .resolve_metadata(&field, values[&seq].clone())
                .expect("resolve");
        }
        reducer.finish().expect("finish").expect("session")
    }

    #[test]
    fn ingress_attribution_preserves_order_and_ignores_late_legacy_metadata() {
        for explicit_override in [false, true] {
            let stream = SessionStreamId::new();
            let mut reducer = SessionReducer::new(stream.clone(), false);
            reducer.apply(&root(&stream, false), None).expect("root");
            let (old, old_value) = metadata(&stream, 2, "channel_id", serde_json::json!("old"));
            reducer.apply(&old, None).expect("old metadata");
            let payload =
                PayloadRecord::json("session.ingress/v1", &serde_json::json!({})).expect("ingress");
            let mut accepted = event(
                &stream,
                3,
                EventData::TurnAccepted {
                    input_ref: StoredPayloadRef::from(&payload),
                    ingress_ref: StoredPayloadRef::from(&payload),
                    reply_route_ref: None,
                },
            );
            accepted.turn_id = Some(TurnId::new());
            reducer.apply(&accepted, None).expect("accepted");
            reducer
                .observe_ingress(&serde_json::json!({
                    "transport":{"channel":{"kind":"discord","alias":"work"}},
                    "sender":"alice"
                }))
                .expect("channel attribution");
            let (mut late, late_value) =
                metadata(&stream, 4, "sender_id", serde_json::json!("legacy"));
            late.actor = EventActor::System {
                component: "session_upgrade".into(),
            };
            reducer.apply(&late, None).expect("late import");
            let mut values = BTreeMap::from([(2, old_value), (4, late_value)]);
            if explicit_override {
                let (later, value) =
                    metadata(&stream, 5, "channel_id", serde_json::json!("explicit"));
                reducer.apply(&later, None).expect("new metadata");
                values.insert(5, value);
            }
            reducer
                .observe_ingress(&serde_json::json!({}))
                .expect("unknown");
            reducer
                .observe_ingress(&serde_json::json!({"transport":"internal"}))
                .expect("nonchannel");
            let summary = finish(reducer, values);
            assert_eq!(
                summary.channel_id(),
                Some(if explicit_override {
                    "explicit"
                } else {
                    "discord.work"
                })
            );
            assert_eq!(summary.fields["sender_id"], "alice");
        }
    }

    #[test]
    fn summary_and_detail_share_metadata_and_resolve_only_latest_fields() {
        let stream = SessionStreamId::new();
        let mut summary = SessionReducer::new(stream.clone(), false);
        let mut detail = SessionReducer::new(stream.clone(), true);
        let payload =
            PayloadRecord::json("session.context/v1", &serde_json::json!({})).expect("context");
        let events = vec![
            root(&stream, false),
            metadata(&stream, 2, "title", serde_json::json!("old")).0,
            metadata(&stream, 3, "title", serde_json::json!("current")).0,
            event(
                &stream,
                4,
                EventData::ContextProjectionCreated {
                    source_ranges: vec![],
                    summary_ref: None,
                    budget: 0,
                    reason_key: "initial_context".into(),
                    algorithm_version: "fixture".into(),
                    result_ref: StoredPayloadRef::from(&payload),
                    summary_source_refs: vec![],
                    transform_started_event_id: None,
                },
            ),
        ];
        for event in &events {
            summary.apply(event, None).expect("summary event");
            detail.apply(event, None).expect("detail event");
        }
        assert_eq!(summary.pending_metadata().count(), 1);
        let values = BTreeMap::from([(3, serde_json::json!("current"))]);
        let summary = finish(summary, values.clone());
        let detail = finish(detail, values);
        assert_eq!(summary.title(), detail.title());
        assert_eq!(summary.head, detail.head);
        assert!(summary.context.is_none());
        assert_eq!(detail.context.expect("detail context").sequence, 4);
    }

    #[test]
    fn imported_metadata_fills_missing_fields_without_replacing_native_values() {
        let stream = SessionStreamId::new();
        let mut reducer = SessionReducer::new(stream.clone(), false);
        reducer.apply(&root(&stream, false), None).expect("root");
        let mut values = BTreeMap::new();
        for (sequence, field, value, imported) in [
            (2, "token_count", serde_json::json!(99), false),
            (3, "token_count", serde_json::json!(2), true),
            (4, "workspace_dir", serde_json::json!("/old"), true),
            (5, "workspace_dir", serde_json::json!("/new"), false),
            (6, "workspace_dir", serde_json::json!("/old"), true),
            (7, "plan", serde_json::json!([]), true),
        ] {
            let (mut item, value) = metadata(&stream, sequence, field, value);
            if imported {
                item.actor = EventActor::System {
                    component: "session_upgrade".into(),
                };
            }
            reducer.apply(&item, None).expect("metadata");
            values.insert(sequence, value);
        }
        let summary = finish(reducer, values);
        assert_eq!(summary.field("token_count", 0u64).expect("tokens"), 99);
        assert_eq!(
            summary
                .field("workspace_dir", String::new())
                .expect("workspace"),
            "/new"
        );
        assert_eq!(summary.fields["plan"], serde_json::json!([]));
    }

    #[test]
    fn summary_matches_paged_history_for_content_and_supersession() {
        let stream = SessionStreamId::new();
        let payload =
            PayloadRecord::json("session.content/v1", &serde_json::json!({})).expect("content");
        let content = event(
            &stream,
            2,
            EventData::AssistantContentRecorded {
                content_ref: StoredPayloadRef::from(&payload),
                provenance_refs: vec![],
            },
        );
        let events = [
            root(&stream, false),
            content.clone(),
            event(
                &stream,
                3,
                EventData::ContentSuperseded {
                    target_event_ids: vec![content.event_id],
                    disposition: Supersession::Hidden {
                        reason_key: "hidden".into(),
                    },
                },
            ),
        ];
        let mut history = projection::HistoryProjection::empty(stream.clone());
        for (index, item) in events.iter().enumerate() {
            let after = history.source_cursor.clone();
            history
                .apply_page(&EventPage {
                    stream_id: stream.clone(),
                    after,
                    through: EventCursor {
                        stream_id: stream.clone(),
                        sequence: item.sequence,
                    },
                    events: vec![item.clone()],
                    raw_events: vec![],
                    has_more: false,
                })
                .expect("page");
            if history.source_cursor.sequence == 2 {
                assert_eq!(history.content.len(), 1);
            }
            let mut reducer = SessionReducer::new(stream.clone(), false);
            for item in &events[..=index] {
                reducer.apply(item, None).expect("reduce");
            }
            let summary = finish(reducer, BTreeMap::new());
            assert_eq!(summary.message_count, history.content.len());
        }
        assert_eq!(history.content.len(), 0);
    }

    #[test]
    fn derived_session_does_not_invent_an_owner() {
        let stream = SessionStreamId::new();
        let mut reducer = SessionReducer::new(stream.clone(), false);
        reducer
            .apply(
                &event(
                    &stream,
                    1,
                    EventData::DerivedSessionStarted {
                        execution_turn_id: Some(TurnId::new()),
                    },
                ),
                None,
            )
            .expect("derived root");
        let derived = finish(reducer, BTreeMap::new());
        assert!(derived.owning_agent.is_none());
        assert!(!derived.fields.contains_key("owning_agent"));
    }

    #[test]
    fn imported_timestamps_do_not_override_later_native_activity() {
        let stream = SessionStreamId::new();
        let mut reducer = SessionReducer::new(stream.clone(), true);
        reducer.apply(&root(&stream, true), None).expect("root");
        let (created, _) = metadata(&stream, 2, "created_at", serde_json::json!(7));
        let (activity, _) = metadata(&stream, 3, "last_activity", serde_json::json!(9));
        reducer.apply(&created, None).expect("created");
        reducer.apply(&activity, None).expect("activity");
        let payload =
            PayloadRecord::json("session.context/v1", &serde_json::json!({})).expect("context");
        reducer
            .apply(
                &event(
                    &stream,
                    4,
                    EventData::ContextProjectionCreated {
                        source_ranges: vec![],
                        summary_ref: None,
                        budget: 0,
                        reason_key: "legacy_import".into(),
                        algorithm_version: "fixture".into(),
                        result_ref: StoredPayloadRef::from(&payload),
                        summary_source_refs: vec![],
                        transform_started_event_id: None,
                    },
                ),
                Some(2),
            )
            .expect("import");
        reducer
            .apply(
                &event(
                    &stream,
                    5,
                    EventData::SessionClosed {
                        reason_key: "session_deleted".into(),
                    },
                ),
                None,
            )
            .expect("close");
        let result = finish(
            reducer,
            BTreeMap::from([(2, serde_json::json!(7)), (3, serde_json::json!(9))]),
        );
        assert_eq!(result.created_at, 7);
        assert_eq!(result.last_activity, 500);
        assert_eq!(result.message_count, 2);
        assert!(result.closed && result.deleted);
        assert!(!result.fields.contains_key("created_at"));
        assert!(!result.fields.contains_key("last_activity"));
    }

    #[test]
    fn ttl_observations_keep_family_open_and_do_not_undo_explicit_closure() {
        for explicit in [false, true] {
            let stream = SessionStreamId::new();
            let mut reducer = SessionReducer::new(stream.clone(), false);
            reducer.apply(&root(&stream, false), None).expect("root");
            reducer
                .apply(
                    &event(
                        &stream,
                        2,
                        EventData::SessionClosed {
                            reason_key: if explicit {
                                "explicit_close"
                            } else {
                                "session_ttl_expired"
                            }
                            .into(),
                        },
                    ),
                    None,
                )
                .expect("close observation");
            reducer
                .apply(
                    &event(
                        &stream,
                        3,
                        EventData::SessionClosed {
                            reason_key: "session_ttl_expired".into(),
                        },
                    ),
                    None,
                )
                .expect("later TTL observation");
            assert_eq!(finish(reducer, BTreeMap::new()).closed, explicit);
        }
    }

    #[test]
    fn missing_root_and_noncontiguous_prefix_are_errors() {
        let stream = SessionStreamId::new();
        let mut reducer = SessionReducer::new(stream.clone(), false);
        assert!(matches!(
            reducer.apply(
                &event(
                    &stream,
                    1,
                    EventData::SessionClosed {
                        reason_key: "closed".into()
                    }
                ),
                None
            ),
            Err(ReadError::Corrupt(_))
        ));
        let mut reducer = SessionReducer::new(stream.clone(), false);
        reducer.apply(&root(&stream, false), None).expect("root");
        assert!(matches!(
            reducer.apply(
                &event(
                    &stream,
                    3,
                    EventData::SessionClosed {
                        reason_key: "closed".into()
                    }
                ),
                None
            ),
            Err(ReadError::Gap(_))
        ));
    }
}
