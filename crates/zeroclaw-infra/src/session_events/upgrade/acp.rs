//! Legacy ACP execution is imported as observed history, never replayed effects.
use super::super::installation::creation_source;
use super::*;
use zeroclaw_api::model_provider::ConversationMessage;
use zeroclaw_api::session_events::*;

pub(super) fn import_all(
    guard: &InstallationGuard,
    store: &dyn SessionEventStore,
) -> Result<(), AppendError> {
    let path = guard.directory().join("acp-sessions.db");
    match filesystem::reader(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(jsonl::storage(error)),
    }
    let source = super::legacy_acp::LegacyAcpStore::open_import(guard).map_err(storage)?;
    for id in source.list_session_ids().map_err(storage)? {
        let family = id.clone();
        let stream = session_id(&family)?;
        let key = format!("legacy-acp-complete:{stream}");
        if store
            .lookup_receipt(&stream, &key)
            .map_err(storage)?
            .is_some()
        {
            continue;
        }
        if store.head(&stream).map_err(storage)?.sequence > 0
            && store
                .lookup_receipt(&stream, &format!("legacy-acp-root:{stream}"))
                .map_err(storage)?
                .is_none()
        {
            return Err(AppendError::InvalidReference(
                "ACP session ID collides with another imported session".into(),
            ));
        }
        let mut data = source
            .load_session(&id)
            .map_err(storage)?
            .ok_or_else(|| AppendError::Corrupt("ACP source disappeared during upgrade".into()))?;
        for (index, message) in data.messages.iter_mut().enumerate() {
            super::legacy_media::upgrade_conversation(message, index);
        }
        if let Some(context) = &mut data.retained_context {
            for (index, message) in context.iter_mut().enumerate() {
                super::legacy_media::upgrade_conversation(message, index);
            }
        }
        let checkpoint = source.legacy_checkpoint(&id).map_err(storage)?;
        let closed = source.is_session_killed(&id).map_err(storage)?;
        let metadata = source.legacy_metadata(&id).map_err(storage)?;
        let imported = AcpImport {
            data,
            checkpoint,
            closed,
            metadata,
        };
        import_one(store, &family, imported)?;
    }
    Ok(())
}

struct AcpImport {
    data: crate::session_repository::SessionTranscript,
    checkpoint: Option<serde_json::Value>,
    closed: bool,
    metadata: Vec<(String, serde_json::Value)>,
}

fn upgrade_actor() -> EventActor {
    EventActor::System {
        component: "session_upgrade".into(),
    }
}

fn import_one(
    store: &dyn SessionEventStore,
    family: &str,
    imported: AcpImport,
) -> Result<(), AppendError> {
    let AcpImport {
        mut data,
        checkpoint,
        closed,
        metadata,
    } = imported;
    let stream = session_id(family)?;
    let branch = BranchId::parse(format!("branch-{stream}"))?;
    let history = data.messages.clone();
    let context = data.retained_context.take().unwrap_or_else(|| {
        zeroclaw_api::session_events::transcript::provider_safe_history(&data.messages)
    });
    let source_digest = Digest::from_bytes(&canonical_json(&serde_json::json!({
        "history":history,"context":context,"checkpoint":checkpoint,"closed":closed,"metadata":metadata,
        "principal_id":data.principal_id,"agent":data.agent_alias,
        "created_at":data.created_at.timestamp_millis(),"last_activity":data.last_activity.timestamp_millis(),
        "history_has_trim_breadcrumb":data.trim_breadcrumb,
    }))?);
    let root_key = format!("legacy-acp-root:{stream}");
    let evidence = PayloadRecord::new(
        PayloadId::parse(format!("legacy-acp-evidence-{stream}"))?,
        "session.evidence/v1".into(),
        canonical_json(
            &serde_json::json!({"source_kind":"legacy-acp","source_digest":source_digest}),
        )?,
    )?;
    if store
        .lookup_receipt(&stream, &root_key)
        .map_err(storage)?
        .is_some()
    {
        let PayloadRead::Present(stored) =
            store.read_payload(&evidence.payload_id).map_err(storage)?
        else {
            return Err(AppendError::Corrupt("ACP import evidence missing".into()));
        };
        if stored != evidence {
            return Err(AppendError::Corrupt(
                "frozen ACP import source changed".into(),
            ));
        }
    } else {
        let mut root = SessionEvent::typed(
            stream.clone(),
            EventData::SessionStarted {
                owning_agent: data.agent_alias,
                capture_mode: zeroclaw_api::session_events::CaptureMode::LegacyMessageOnly,
            },
            EventActor::default(),
        )?;
        root.branch_id = Some(branch.clone());
        let imported = SessionEvent::typed(
            stream.clone(),
            EventData::LegacyHistoryImported {
                source_kind: "legacy-acp".into(),
                import_receipt_ref: StoredPayloadRef::from(&evidence),
                completeness: "typed_history_and_context".into(),
                known_gaps: vec![
                    "prior_execution_not_observed".into(),
                    "legacy_checkpoint_not_reexecuted".into(),
                ],
            },
            EventActor::default(),
        )?;
        let mut events = vec![root, imported];
        let mut payloads = vec![evidence];
        for (field, value) in [
            ("principal_id".into(), serde_json::json!(data.principal_id)),
            (
                "created_at".into(),
                serde_json::json!(data.created_at.timestamp_millis()),
            ),
            (
                "last_activity".into(),
                serde_json::json!(data.last_activity.timestamp_millis()),
            ),
        ]
        .into_iter()
        {
            let payload = PayloadRecord::json("session.authority/v1", &value)?;
            events.push(SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field,
                    new_value_ref: StoredPayloadRef::from(&payload),
                },
                EventActor::default(),
            )?);
            payloads.push(payload);
        }
        append(store, &stream, root_key, events, payloads)?;
    }
    import_metadata(store, &stream, &metadata)?;
    let mut writer = ImportWriter {
        store,
        stream: &stream,
        branch: &branch,
        refs: vec![],
        trim: data.trim_breadcrumb,
    };
    let history_refs = writer.rows(&history, "history")?;
    let history_digests = history
        .iter()
        .map(|message| canonical_json(message).map(|bytes| Digest::from_bytes(&bytes)))
        .collect::<Result<Vec<_>, _>>()?;
    let mut cursor = history.len();
    let mut selected = vec![None; context.len()];
    let mut new_messages = Vec::new();
    let mut new_positions = Vec::new();
    // Match retained suffixes from the end, preserving repeated message identity.
    for (position, message) in context.iter().enumerate().rev() {
        let digest = Digest::from_bytes(&canonical_json(message)?);
        if let Some(index) = history_digests[..cursor]
            .iter()
            .rposition(|known| known == &digest)
        {
            selected[position] = Some(history_refs[index].clone());
            cursor = index;
        } else {
            new_positions.push(position);
            new_messages.push(message.clone());
        }
    }
    let new_refs = writer.rows(&new_messages, "context")?;
    for (position, reference) in new_positions.into_iter().zip(new_refs) {
        selected[position] = Some(reference);
    }
    let checkpoint = if let Some(mut checkpoint) = checkpoint {
        let fragments: Vec<ConversationMessage> =
            serde_json::from_value(checkpoint["fragments"].take()).map_err(storage)?;
        let references = writer.rows(&fragments, "checkpoint")?;
        checkpoint["message_refs"] = serde_json::json!(references);
        Some(PayloadRecord::json("session.evidence/v1", &checkpoint)?)
    } else {
        None
    };
    let context_refs = selected
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| AppendError::Corrupt("ACP context identity missing".into()))?;
    let body = PayloadRecord::json(
        "session.context/v1",
        &serde_json::json!({"message_refs":context_refs,"history_has_trim_breadcrumb":data.trim_breadcrumb}),
    )?;
    let mut final_context = projection(&stream, &branch, &body, "legacy_acp_context")?;
    final_context.event_id = EventId::parse(format!("legacy-acp-final-{stream}"))?;
    let (source, source_payload) = creation_source(stream.clone(), "acp")?;
    let mut events = Vec::new();
    let mut payloads = vec![body, source_payload];
    if let Some(checkpoint) = checkpoint {
        events.push(SessionEvent::typed(
            stream.clone(),
            EventData::LegacyHistoryImported {
                source_kind: "legacy-acp-checkpoint".into(),
                import_receipt_ref: StoredPayloadRef::from(&checkpoint),
                completeness: "partial_observation".into(),
                known_gaps: vec!["legacy_execution_outcome_unknown".into()],
            },
            upgrade_actor(),
        )?);
        payloads.push(checkpoint);
    }
    if closed {
        events.push(SessionEvent::typed(
            stream.clone(),
            EventData::SessionClosed {
                reason_key: "legacy_acp_session_killed".into(),
            },
            EventActor::default(),
        )?);
    }
    events.extend([final_context, source]);
    append(
        store,
        &stream,
        format!("legacy-acp-complete:{stream}"),
        events,
        payloads,
    )
}

fn import_metadata(
    store: &dyn SessionEventStore,
    stream: &SessionId,
    fields: &[(String, serde_json::Value)],
) -> Result<(), AppendError> {
    let mut offset = 0;
    let mut chunk = 0;
    while offset < fields.len() {
        let mut events = Vec::new();
        let mut payloads = Vec::new();
        let mut bytes = 0;
        while offset < fields.len() && events.len() < 256 {
            let (field, value) = &fields[offset];
            let body = super::super::catalog::metadata_payload(field, value.clone())?;
            if !events.is_empty() && bytes + body.body.len() > 8 * 1024 * 1024 {
                break;
            }
            bytes += body.body.len();
            events.push(SessionEvent::typed(
                stream.clone(),
                EventData::SessionMetadataChanged {
                    field: field.clone(),
                    new_value_ref: StoredPayloadRef::from(&body),
                },
                upgrade_actor(),
            )?);
            payloads.push(body);
            offset += 1;
        }
        let key = format!("legacy-acp-metadata:{stream}:{chunk}");
        if store
            .lookup_receipt(stream, &key)
            .map_err(storage)?
            .is_none()
        {
            append(store, stream, key, events, payloads)?;
        }
        #[cfg(test)]
        super::super::crash_tests::crash_boundary("legacy_acp_metadata");
        chunk += 1;
    }
    Ok(())
}

struct ImportWriter<'a> {
    store: &'a dyn SessionEventStore,
    stream: &'a SessionId,
    branch: &'a BranchId,
    refs: Vec<MessageRef>,
    trim: bool,
}

fn chunk_end(rows: &[ConversationMessage], offset: usize) -> Result<usize, AppendError> {
    let mut end = offset;
    let mut bytes = 0;
    while end < rows.len() && end - offset < 64 {
        let size = canonical_json(&rows[end])?.len();
        if end > offset && bytes + size > 8 * 1024 * 1024 {
            break;
        }
        bytes += size;
        end += 1;
    }
    Ok(end)
}

impl ImportWriter<'_> {
    fn rows(
        &mut self,
        rows: &[ConversationMessage],
        phase: &str,
    ) -> Result<Vec<MessageRef>, AppendError> {
        let mut imported = Vec::with_capacity(rows.len());
        let mut offset = 0;
        let mut chunk = 0;
        while offset < rows.len() {
            let end = chunk_end(rows, offset)?;
            let event_id = EventId::parse(format!("legacy-acp-{}-{phase}-{chunk}", self.stream))?;
            let batch = &rows[offset..end];
            let conversation = PayloadRecord::new(
                PayloadId::parse(format!("{event_id}-messages"))?,
                "session.conversation/v1".into(),
                canonical_json(&batch)?,
            )?;
            let mut seeds = Vec::new();
            for (item_index, message) in rows[offset..end].iter().enumerate() {
                let message_id =
                    MessageId::parse(format!("context-message-{event_id}-{}", self.refs.len()))?;
                if phase == "history"
                    && let ConversationMessage::Chat(chat) = message
                    && chat.role != "system"
                {
                    seeds.push(serde_json::json!({"source_index":item_index,"message_id":message_id,"role":chat.role}));
                }
                let reference = MessageRef {
                    message_id,
                    source: ContextMessageSource::Conversation {
                        payload_ref: StoredPayloadRef::from(&conversation),
                        item_index,
                    },
                };
                self.refs.push(reference.clone());
                imported.push(reference);
            }
            let context = PayloadRecord::new(
                PayloadId::parse(format!("{event_id}-context"))?,
                "session.context/v1".into(),
                canonical_json(&serde_json::json!({
                    "message_refs":self.refs,"messages_ref":StoredPayloadRef::from(&conversation),"seed_messages":seeds,"history_has_trim_breadcrumb":self.trim,
                }))?,
            )?;
            let reason = if phase == "history" {
                "host_seed"
            } else {
                "legacy_acp_seed"
            };
            let mut event = projection(self.stream, self.branch, &context, reason)?;
            event.event_id = event_id;
            let key = format!("legacy-acp-{}-{phase}-{chunk}", self.stream);
            if self
                .store
                .lookup_receipt(self.stream, &key)
                .map_err(storage)?
                .is_none()
            {
                append(
                    self.store,
                    self.stream,
                    key,
                    vec![event],
                    vec![conversation, context],
                )?;
            }
            #[cfg(test)]
            super::super::crash_tests::crash_boundary("legacy_acp_chunk");
            offset = end;
            chunk += 1;
        }
        Ok(imported)
    }
}

fn projection(
    stream: &SessionId,
    branch: &BranchId,
    body: &PayloadRecord,
    reason: &str,
) -> Result<SessionEvent, AppendError> {
    let mut event = SessionEvent::typed(
        stream.clone(),
        EventData::ContextProjectionCreated {
            source_ranges: vec![],
            summary_ref: None,
            budget: 0,
            reason_key: reason.into(),
            algorithm_version: "legacy-acp/v1".into(),
            result_ref: StoredPayloadRef::from(body),
            summary_source_refs: vec![],
            transform_started_event_id: None,
        },
        EventActor::default(),
    )?;
    event.branch_id = Some(branch.clone());
    Ok(event)
}

fn append(
    store: &dyn SessionEventStore,
    stream: &SessionId,
    key: String,
    events: Vec<SessionEvent>,
    payloads: Vec<PayloadRecord>,
) -> Result<(), AppendError> {
    store.append_batch(AppendRequest {
        session_id: stream.clone(),
        expected_head: store.head(stream).map_err(storage)?,
        idempotency_key: key,
        events,
        payloads,
        erasure_reference_commitment: None,
    })?;
    Ok(())
}
