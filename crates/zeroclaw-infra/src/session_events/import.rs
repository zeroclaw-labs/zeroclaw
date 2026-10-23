//! Bounded, resumable import into the installation's selected event format.
//! The caller holds the installation or CLI source lease until publication.
use super::installation::{NativeBackend, family_identity};
use super::*;
use zeroclaw_api::model_provider::ChatMessage;
use zeroclaw_api::session_events::*;

pub struct LegacyImport<'a> {
    pub family: &'a str,
    pub owning_agent: &'a str,
    pub target: NativeBackend,
    pub stream: SessionStreamId,
    pub branch: BranchId,
    pub session: SessionId,
    pub source_kind: &'a str,
}

pub struct LegacySnapshot {
    pub messages: Vec<ChatMessage>,
    pub history_has_trim_breadcrumb: Option<bool>,
    pub metadata: Vec<(String, serde_json::Value)>,
}

/// CLI paths hold their exclusive path lease until the old file is archived.
pub fn import_legacy_snapshot(
    data_dir: &Path,
    command: LegacyImport<'_>,
    read_source: impl FnOnce() -> Result<LegacySnapshot, AppendError>,
) -> Result<SessionStreamId, AppendError> {
    let guard = installation::InstallationGuard::acquire(&data_dir.join("sessions"))?;
    if guard.active()?.0 != command.target {
        return Err(AppendError::InvalidTransition(
            "import backend differs from active format".into(),
        ));
    }
    let store = open_event_store(data_dir, command.target.name())?;
    import_snapshot(store.as_ref(), command, read_source()?)
}

pub(super) fn import_snapshot(
    store: &dyn SessionEventStore,
    command: LegacyImport<'_>,
    snapshot: LegacySnapshot,
) -> Result<SessionStreamId, AppendError> {
    let completed = format!("legacy-family-import:{}", command.stream);
    if store
        .lookup_receipt(&command.stream, &completed)
        .map_err(corrupt)?
        .is_some()
    {
        return Ok(command.stream);
    }
    let messages = snapshot.messages;
    let metadata = snapshot.metadata;
    let source_digest = Digest::from_bytes(&canonical_json(&serde_json::json!({
        "messages": messages, "history_has_trim_breadcrumb": snapshot.history_has_trim_breadcrumb,
        "metadata": metadata,
    }))?);
    let root_key = format!("legacy-import-root:{}", command.stream);
    if let Some(receipt) = store
        .lookup_receipt(&command.stream, &root_key)
        .map_err(corrupt)?
    {
        let page = store
            .read_page(
                &command.stream,
                &EventCursor::beginning(command.stream.clone()),
                Some(&receipt.last),
                32,
                MAX_PAGE_BYTES,
            )
            .map_err(corrupt)?;
        let evidence = page
            .events
            .iter()
            .find_map(|event| match &event.body.data {
                EventData::LegacyHistoryImported {
                    import_receipt_ref, ..
                } => Some(import_receipt_ref),
                _ => None,
            })
            .ok_or_else(|| AppendError::Corrupt("import source binding missing".into()))?;
        let PayloadRead::Present(record) =
            store.read_payload(&evidence.payload_id).map_err(corrupt)?
        else {
            return Err(AppendError::Corrupt("import evidence unavailable".into()));
        };
        let value: serde_json::Value = serde_json::from_slice(&record.body).map_err(corrupt)?;
        let root_matches = matches!(page.events.first().map(|event| &event.body.data),
            Some(EventData::SessionStarted { owning_agent, session_id, .. }) if owning_agent == command.owning_agent && session_id == &command.session);
        if !root_matches
            || page
                .events
                .first()
                .and_then(|event| event.branch_id.as_ref())
                != Some(&command.branch)
            || value["source_digest"] != serde_json::json!(source_digest)
            || value["source_kind"] != command.source_kind
        {
            return Err(AppendError::Corrupt(
                "frozen legacy source or import scope changed".into(),
            ));
        }
    } else {
        let mut root = SessionEvent::typed(
            command.stream.clone(),
            EventData::SessionStarted {
                session_id: command.session.clone(),
                owning_agent: command.owning_agent.into(),
                capture_mode: CaptureMode::LegacyMessageOnly,
            },
            EventActor::default(),
        )?;
        root.branch_id = Some(command.branch.clone());
        let evidence = PayloadRecord::json(
            "session.evidence/v1",
            &serde_json::json!({
                "source_kind": command.source_kind, "source_digest": source_digest,
                "source_message_count": messages.len(), "capture_mode": "legacy_message_only",
            }),
        )?;
        let mut imported = SessionEvent::typed(
            command.stream.clone(),
            EventData::LegacyHistoryImported {
                source_kind: command.source_kind.into(),
                import_receipt_ref: StoredPayloadRef::from(&evidence),
                completeness: "legacy_message_only".into(),
                known_gaps: vec![
                    "provider_reasoning_and_attempts".into(),
                    "tool_and_approval_lifecycle".into(),
                    "child_and_control_lifecycle".into(),
                ],
            },
            EventActor::default(),
        )?;
        imported.branch_id = Some(command.branch.clone());
        let mut events = vec![root, imported];
        let mut payloads = vec![evidence];
        for (field, value) in metadata {
            let codec = if matches!(
                field.as_str(),
                "principal_id" | "created_at" | "last_activity"
            ) {
                "session.authority/v1"
            } else {
                "session.bytes/v1"
            };
            let value = PayloadRecord::json(codec, &value)?;
            let mut event = SessionEvent::typed(
                command.stream.clone(),
                EventData::SessionMetadataChanged {
                    field,
                    new_value_ref: StoredPayloadRef::from(&value),
                },
                EventActor::default(),
            )?;
            event.branch_id = Some(command.branch.clone());
            events.push(event);
            payloads.push(value);
        }
        append(store, &command.stream, root_key, events, payloads)?;
        #[cfg(test)]
        super::crash_tests::crash_boundary("legacy_import_root");
    }

    let mut entries = Vec::with_capacity(messages.len());
    let mut chunk_entries = Vec::new();
    let mut payloads = Vec::new();
    let mut bytes = 0usize;
    let mut chunk = 0usize;
    for (index, message) in messages.into_iter().enumerate() {
        let mut parts = Vec::new();
        let mut start = 0;
        while start < message.content.len() || (start == 0 && message.content.is_empty()) {
            let mut end = (start + 8 * 1024 * 1024).min(message.content.len());
            while !message.content.is_char_boundary(end) {
                end -= 1;
            }
            let body = PayloadRecord::new(
                stable_payload(&command.stream, &format!("text-{index}-{}", parts.len()))?,
                "session.text/v1".into(),
                message.content.as_bytes()[start..end].to_vec(),
            )?;
            if bytes + body.body.len() > 16 * 1024 * 1024 || payloads.len() >= 4000 {
                flush_chunk(store, &command, chunk, &mut chunk_entries, &mut payloads)?;
                chunk += 1;
                bytes = 0;
            }
            bytes += body.body.len();
            parts.push(RecordedContentPart::Text {
                text_ref: StoredPayloadRef::from(&body),
            });
            payloads.push(body);
            if end == message.content.len() {
                break;
            }
            start = end;
        }
        let content = PayloadRecord::new(
            stable_payload(&command.stream, &format!("content-{index}"))?,
            "session.content/v1".into(),
            canonical_json(&ContentPayload {
                message_id: MessageId::parse(format!("legacy-message-{}-{index}", command.stream))?,
                parts,
            })?,
        )?;
        bytes += content.body.len();
        let entry =
            serde_json::json!({"role":message.role,"content_ref":StoredPayloadRef::from(&content)});
        entries.push(entry.clone());
        chunk_entries.push(entry);
        payloads.push(content);
    }
    if !payloads.is_empty() {
        flush_chunk(store, &command, chunk, &mut chunk_entries, &mut payloads)?;
    }
    let context = PayloadRecord::new(
        stable_payload(&command.stream, "context")?,
        "session.context/v1".into(),
        canonical_json(&serde_json::json!({
            "legacy_messages": entries, "history_has_trim_breadcrumb": snapshot.history_has_trim_breadcrumb,
        }))?,
    )?;
    let mut projection = SessionEvent::typed(
        command.stream.clone(),
        EventData::ContextProjectionCreated {
            source_ranges: vec![],
            summary_ref: None,
            budget: 0,
            reason_key: "legacy_import".into(),
            algorithm_version: "legacy-message-import/v2".into(),
            result_ref: StoredPayloadRef::from(&context),
            summary_source_refs: vec![],
            transform_started_event_id: None,
        },
        EventActor::default(),
    )?;
    projection.branch_id = Some(command.branch.clone());
    let (identity, identity_payload) = family_identity(command.stream.clone(), command.family)?;
    append(
        store,
        &command.stream,
        completed,
        vec![projection, identity],
        vec![context, identity_payload],
    )?;
    Ok(command.stream)
}

fn corrupt(error: impl std::fmt::Display) -> AppendError {
    AppendError::Corrupt(error.to_string())
}
fn stable_payload(stream: &SessionStreamId, kind: &str) -> Result<PayloadId, AppendError> {
    Ok(PayloadId::parse(format!("legacy-{stream}-{kind}"))?)
}
fn append(
    store: &dyn SessionEventStore,
    stream: &SessionStreamId,
    key: String,
    events: Vec<SessionEvent>,
    payloads: Vec<PayloadRecord>,
) -> Result<(), AppendError> {
    store.append_batch(AppendRequest {
        stream_id: stream.clone(),
        expected_head: store.head(stream).map_err(corrupt)?,
        idempotency_key: key,
        events,
        payloads,
        erasure_reference_commitment: None,
    })?;
    Ok(())
}
fn flush_chunk(
    store: &dyn SessionEventStore,
    command: &LegacyImport<'_>,
    index: usize,
    entries: &mut Vec<serde_json::Value>,
    payloads: &mut Vec<PayloadRecord>,
) -> Result<(), AppendError> {
    let key = format!("legacy-import-chunk:{}:{index}", command.stream);
    if store
        .lookup_receipt(&command.stream, &key)
        .map_err(corrupt)?
        .is_some()
    {
        entries.clear();
        payloads.clear();
        return Ok(());
    }
    let mut referenced_text = std::collections::HashSet::new();
    for payload in payloads
        .iter()
        .filter(|payload| payload.codec == "session.content/v1")
    {
        let content: ContentPayload = serde_json::from_slice(&payload.body).map_err(corrupt)?;
        for part in content.parts {
            if let RecordedContentPart::Text { text_ref } = part {
                referenced_text.insert(text_ref.payload_id);
            }
        }
    }
    let evidence = PayloadRecord::new(
        stable_payload(&command.stream, &format!("chunk-{index}"))?,
        "session.evidence/v1".into(),
        canonical_json(&serde_json::json!({
            "chunk_index": index, "legacy_messages": entries,
            "text_fragments": payloads.iter().filter(|payload| payload.codec == "session.text/v1" && !referenced_text.contains(&payload.payload_id)).map(StoredPayloadRef::from).collect::<Vec<_>>(),
        }))?,
    )?;
    let event = SessionEvent::typed(
        command.stream.clone(),
        EventData::LegacyHistoryImported {
            source_kind: command.source_kind.into(),
            import_receipt_ref: StoredPayloadRef::from(&evidence),
            completeness: "legacy_message_only".into(),
            known_gaps: vec![],
        },
        EventActor::default(),
    )?;
    let mut records = std::mem::take(payloads);
    records.push(evidence);
    entries.clear();
    append(store, &command.stream, key, vec![event], records)?;
    #[cfg(test)]
    super::crash_tests::crash_boundary("legacy_import_chunk");
    Ok(())
}
