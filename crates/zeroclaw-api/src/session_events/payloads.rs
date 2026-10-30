//! Payload graph and erasure rules derived from the canonical history.
use super::*;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct StoredPayload {
    pub reference: StoredPayloadRef,
    pub body: Option<Vec<u8>>,
}

mod codecs;

/// Registered content codecs shared by backend projections and graph validation.
pub fn content_leaf_codecs() -> impl Iterator<Item = &'static str> {
    codecs::content_leaf_names()
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadDisposition {
    Present,
    ErasurePending,
    Redacted,
}

pub fn disposition(history: &[SessionEvent], id: &PayloadId) -> PayloadDisposition {
    let intents: HashSet<_> = history
        .iter()
        .filter_map(|event| match &event.body.data {
            EventData::PayloadErasureRequested { targets, .. }
                if targets.iter().any(|target| target.payload_id == *id) =>
            {
                Some(event.event_id.clone())
            }
            _ => None,
        })
        .collect();
    if intents.is_empty() {
        return PayloadDisposition::Present;
    }
    if history.iter().any(|event| matches!(&event.body.data, EventData::PayloadRedacted { erasure_requested_event_id } if intents.contains(erasure_requested_event_id))) {
        PayloadDisposition::Redacted
    } else { PayloadDisposition::ErasurePending }
}

/// Only registered content leaves may be physically erased; references and
/// message/response identities remain immutable. Unknown codecs block graph
/// mutation while remaining readable.
pub fn content_leaf(codec: &str) -> bool {
    matches!(codecs::lookup(codec), Some(codecs::Codec::ContentLeaf))
}

pub fn nested_refs(payload: &StoredPayload) -> Result<Vec<StoredPayloadRef>, AppendError> {
    let validator = match codecs::lookup(&payload.reference.codec) {
        Some(codecs::Codec::ContentLeaf) => return Ok(Vec::new()),
        Some(codecs::Codec::Structure(validator)) => validator,
        None => {
            return Err(AppendError::UnresolvedReachability(
                payload.reference.codec.clone(),
            ));
        }
    };
    let bytes = payload
        .body
        .as_ref()
        .ok_or_else(|| AppendError::Corrupt("structural payload body missing".to_owned()))?;
    if let Some(validate) = validator {
        validate(bytes)?;
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| AppendError::Corrupt(error.to_string()))?;
    refs_in_value(&value)
}

/// Read-side response references retain raw traversal positions. Unknown
/// blocks never authorize their apparent references; writers and erasure keep
/// using the strict structural decoder in `nested_refs`.
pub enum ResponseReadReference {
    Content(StoredPayloadRef),
    Reasoning(StoredPayloadRef),
    Unknown,
}

pub struct ResponseReadReferences {
    pub references: Vec<ResponseReadReference>,
    pub unknown_block_indices: Vec<usize>,
}

pub fn response_references_for_read(bytes: &[u8]) -> Result<ResponseReadReferences, AppendError> {
    fn invalid(error: impl std::fmt::Display) -> AppendError {
        AppendError::InvalidReference(format!("invalid structural payload: {error}"))
    }
    fn slots(
        value: &Value,
        known: &[StoredPayloadRef],
        reasoning: bool,
        out: &mut Vec<ResponseReadReference>,
    ) {
        match value {
            Value::Object(object) if object.contains_key("payload_id") => {
                let reference = serde_json::from_value::<StoredPayloadRef>(value.clone())
                    .ok()
                    .filter(|reference| known.contains(reference));
                out.push(match reference {
                    Some(reference) if reasoning => ResponseReadReference::Reasoning(reference),
                    Some(reference) => ResponseReadReference::Content(reference),
                    None => ResponseReadReference::Unknown,
                });
            }
            Value::Object(object) => {
                for value in object.values() {
                    slots(value, known, reasoning, out);
                }
            }
            Value::Array(values) => {
                for value in values {
                    slots(value, known, reasoning, out);
                }
            }
            _ => {}
        }
    }
    let value: Value = serde_json::from_slice(bytes).map_err(invalid)?;
    let mut outer = value.clone();
    let object = outer
        .as_object_mut()
        .ok_or_else(|| invalid("response is not an object"))?;
    let blocks = object
        .insert("blocks".into(), Value::Array(Vec::new()))
        .and_then(|blocks| blocks.as_array().cloned())
        .ok_or_else(|| invalid("response blocks are not an array"))?;
    let response: ProviderResponsePayload = serde_json::from_value(outer).map_err(invalid)?;
    let outer_refs = refs_in_value(&serde_json::to_value(response).map_err(invalid)?)?;
    let mut references = Vec::new();
    let mut unknown_block_indices = Vec::new();
    for (key, field) in value
        .as_object()
        .ok_or_else(|| invalid("response object"))?
    {
        if key != "blocks" {
            slots(field, &outer_refs, false, &mut references);
            continue;
        }
        for (index, block) in blocks.iter().enumerate() {
            let kind = block
                .get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("response block kind missing"))?;
            if matches!(kind, "text" | "reasoning" | "tool_call" | "media" | "other") {
                let known: ResponseBlock =
                    serde_json::from_value(block.clone()).map_err(invalid)?;
                let protected = matches!(known, ResponseBlock::Reasoning { .. });
                let refs = refs_in_value(&serde_json::to_value(known).map_err(invalid)?)?;
                slots(block, &refs, protected, &mut references);
            } else {
                unknown_block_indices.push(index);
                slots(block, &[], false, &mut references);
            }
        }
    }
    Ok(ResponseReadReferences {
        references,
        unknown_block_indices,
    })
}

/// Canonical reference traversal, including forward-compatible unknown parts.
pub fn refs_in_value(value: &Value) -> Result<Vec<StoredPayloadRef>, AppendError> {
    fn refs(value: &Value, out: &mut Vec<StoredPayloadRef>) -> Result<(), AppendError> {
        match value {
            Value::Object(object) if object.contains_key("payload_id") => out.push(
                serde_json::from_value(value.clone())
                    .map_err(|error| AppendError::InvalidReference(error.to_string()))?,
            ),
            Value::Object(object) => {
                for child in object.values() {
                    refs(child, out)?;
                }
            }
            Value::Array(items) => {
                for child in items {
                    refs(child, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut out = Vec::new();
    refs(value, &mut out)?;
    Ok(out)
}

/// Scope and immutable events reaching a target, including nested references.
/// Sorted to freeze the authorization precondition without a mutable refcount.
pub fn erasure_scope(
    history: &[SessionEvent],
    payloads: &[StoredPayload],
    targets: &[StoredPayloadRef],
) -> Result<(Vec<SessionStreamId>, Digest), AppendError> {
    let target_ids: HashSet<_> = targets
        .iter()
        .map(|target| target.payload_id.clone())
        .collect();
    let mut edges = HashMap::new();
    for payload in payloads {
        edges.insert(
            payload.reference.payload_id.clone(),
            nested_refs(payload)?
                .into_iter()
                .map(|reference| reference.payload_id)
                .collect::<Vec<_>>(),
        );
    }
    fn reaches(
        id: &PayloadId,
        targets: &HashSet<PayloadId>,
        edges: &HashMap<PayloadId, Vec<PayloadId>>,
        seen: &mut HashSet<PayloadId>,
    ) -> bool {
        if targets.contains(id) {
            return true;
        }
        if !seen.insert(id.clone()) {
            return false;
        }
        edges.get(id).is_some_and(|children| {
            children
                .iter()
                .any(|child| reaches(child, targets, edges, seen))
        })
    }
    let mut references = Vec::new();
    for event in history {
        if matches!(
            event.body.kind(),
            EventKind::PayloadErasureRequested | EventKind::PayloadRedacted
        ) {
            continue;
        }
        if event.body.kind().is_unknown() {
            return Err(AppendError::UnresolvedReachability(
                "unknown event may reference target".into(),
            ));
        }
        if event.body.data.stored_refs()?.iter().any(|reference| {
            reaches(
                &reference.payload_id,
                &target_ids,
                &edges,
                &mut HashSet::new(),
            )
        }) {
            references.push((event.stream_id.clone(), event.event_id.clone()));
        }
    }
    references.sort();
    let mut scopes: Vec<_> = references
        .iter()
        .map(|(stream, _)| stream.clone())
        .collect();
    scopes.dedup();
    Ok((scopes, Digest::from_bytes(&canonical_json(&references)?)))
}

/// Validate relationships which require registered structure, never content leaves.

#[derive(Clone, Debug)]
struct ApprovalState {
    requested: EventId,
    resolved: bool,
    invalidated: bool,
}
/// Disposable payload/reference admission index, reconstructed from committed
/// records and events. Content leaves need metadata only during validation.
#[derive(Default)]
pub struct PayloadIndex {
    records: HashMap<PayloadId, StoredPayload>,
    message_ids: HashSet<MessageId>,
    approvals: HashMap<(SessionStreamId, String), ApprovalState>,
    dispositions: HashMap<PayloadId, PayloadDisposition>,
    provider_observations: super::provider_observation::ObservationIndex,
}
pub struct PayloadDelta {
    records: HashMap<PayloadId, StoredPayload>,
    message_ids: HashSet<MessageId>,
    approvals: HashMap<(SessionStreamId, String), ApprovalState>,
    dispositions: HashMap<PayloadId, PayloadDisposition>,
    deleting: Vec<PayloadId>,
    provider_observations: super::provider_observation::ObservationDelta,
}
impl PayloadDelta {
    pub fn deleting(&self) -> &[PayloadId] {
        &self.deleting
    }
}
impl PayloadIndex {
    pub fn from_history(
        history: &[SessionEvent],
        existing: &[StoredPayload],
    ) -> Result<Self, AppendError> {
        Self::from_index(
            &super::rules::HistoryIndex::from_history(history)?,
            history,
            existing,
            &|_| Ok(None),
        )
    }

    /// Reuse the semantic index rather than duplicating the full event history
    /// and its lifecycle validation during cold reconstruction.
    pub fn from_index(
        facts: &super::rules::HistoryIndex,
        history: &[SessionEvent],
        existing: &[StoredPayload],
        body_length: &dyn Fn(&PayloadId) -> Result<Option<u64>, AppendError>,
    ) -> Result<Self, AppendError> {
        let mut index = Self::default();
        index.records.extend(
            existing
                .iter()
                .map(|row| (row.reference.payload_id.clone(), row.clone())),
        );
        index.observe(history, facts, body_length)?;
        Ok(index)
    }
    pub fn commit(&mut self, delta: PayloadDelta) {
        self.provider_observations
            .commit(delta.provider_observations);
        self.records
            .extend(delta.records.into_iter().map(|(id, mut row)| {
                if content_leaf(&row.reference.codec) {
                    row.body = None;
                }
                (id, row)
            }));
        self.message_ids.extend(delta.message_ids);
        self.approvals.extend(delta.approvals);
        self.dispositions.extend(delta.dispositions);
        for id in delta.deleting {
            if let Some(row) = self.records.get_mut(&id) {
                row.body = None;
            }
        }
    }
    pub fn extend_committed(
        &mut self,
        events: &[SessionEvent],
        records: Vec<StoredPayload>,
        history: &super::rules::HistoryIndex,
        body_length: &dyn Fn(&PayloadId) -> Result<Option<u64>, AppendError>,
    ) -> Result<(), AppendError> {
        self.records.extend(
            records
                .into_iter()
                .map(|row| (row.reference.payload_id.clone(), row)),
        );
        self.observe(events, history, body_length)
    }
    fn observe(
        &mut self,
        events: &[SessionEvent],
        history: &super::rules::HistoryIndex,
        body_length: &dyn Fn(&PayloadId) -> Result<Option<u64>, AppendError>,
    ) -> Result<(), AppendError> {
        self.provider_observations
            .observe_committed(events, &self.records, body_length)?;
        for event in events {
            match &event.body.data {
                EventData::UserContentRecorded { content_ref, .. }
                | EventData::AssistantContentRecorded { content_ref, .. } => {
                    let row = self.records.get(&content_ref.payload_id).ok_or_else(|| {
                        AppendError::Corrupt("content payload index missing".into())
                    })?;
                    let content: ContentPayload =
                        serde_json::from_slice(row.body.as_deref().ok_or_else(|| {
                            AppendError::Corrupt("content structure missing".into())
                        })?)
                        .map_err(|error| AppendError::Corrupt(error.to_string()))?;
                    self.message_ids.insert(content.message_id);
                }
                EventData::ApprovalRequested { request_id, .. } => {
                    self.approvals.insert(
                        (event.stream_id.clone(), request_id.clone()),
                        ApprovalState {
                            requested: event.event_id.clone(),
                            resolved: false,
                            invalidated: false,
                        },
                    );
                }
                EventData::ApprovalResolved { request_id, .. } => {
                    if let Some(state) = self
                        .approvals
                        .get_mut(&(event.stream_id.clone(), request_id.clone()))
                    {
                        state.resolved = true;
                    }
                }
                EventData::ApprovalInvalidated { request_id, .. } => {
                    if let Some(state) = self
                        .approvals
                        .get_mut(&(event.stream_id.clone(), request_id.clone()))
                    {
                        state.invalidated = true;
                    }
                }
                EventData::PayloadErasureRequested { targets, .. } => {
                    for target in targets {
                        self.dispositions.insert(
                            target.payload_id.clone(),
                            PayloadDisposition::ErasurePending,
                        );
                    }
                }
                _ => {}
            }
        }
        for event in events {
            if let EventData::PayloadRedacted {
                erasure_requested_event_id,
            } = &event.body.data
            {
                let intent = history
                    .get_event(erasure_requested_event_id)
                    .ok_or_else(|| {
                        AppendError::Corrupt("redaction reconstruction intent missing".into())
                    })?;
                let EventData::PayloadErasureRequested { targets, .. } = &intent.body.data else {
                    return Err(AppendError::Corrupt(
                        "redaction reconstruction intent kind".into(),
                    ));
                };
                for target in targets {
                    self.dispositions
                        .insert(target.payload_id.clone(), PayloadDisposition::Redacted);
                }
            }
        }
        Ok(())
    }
    pub fn plan(
        &self,
        history: &super::rules::HistoryIndex,
        request: &AppendRequest,
    ) -> Result<PayloadDelta, AppendError> {
        use super::index::{OverlayMap, OverlaySet};
        let mut payloads = OverlayMap::new(&self.records);
        let mut prior = OverlayMap::new(&history.prior);
        let mut message_ids = OverlaySet::new(&self.message_ids);
        let mut approvals = OverlayMap::new(&self.approvals);
        let mut states = OverlayMap::new(&self.dispositions);
        let state = |states: &OverlayMap<'_, PayloadId, PayloadDisposition>, id: &PayloadId| {
            states
                .get(id)
                .copied()
                .unwrap_or(PayloadDisposition::Present)
        };
        for payload in &request.payloads {
            let reference = StoredPayloadRef::from(payload);
            if state(&states, &payload.payload_id) != PayloadDisposition::Present {
                return Err(AppendError::InvalidReference(
                    "erased payload cannot be rewritten or reused".into(),
                ));
            }
            if let Some(old) = payloads.get(&payload.payload_id)
                && (old.reference != reference
                    || old.body.as_ref().is_some_and(|body| body != &payload.body))
            {
                return Err(ContractError::PayloadConflict(payload.payload_id.to_string()).into());
            }
            // Validate structure before indexing any user-supplied reference.
            let row = StoredPayload {
                reference,
                body: Some(payload.body.clone()),
            };
            nested_refs(&row)?;
            payloads.insert(payload.payload_id.clone(), row);
        }
        fn visit(
            id: &PayloadId,
            payloads: &OverlayMap<'_, PayloadId, StoredPayload>,
            states: &OverlayMap<'_, PayloadId, PayloadDisposition>,
            active: &mut HashSet<PayloadId>,
            done: &mut HashSet<PayloadId>,
            depth: usize,
        ) -> Result<(), AppendError> {
            if depth > 64 {
                return Err(AppendError::InvalidReference(
                    "payload graph depth exceeded".into(),
                ));
            }
            if states
                .get(id)
                .is_some_and(|state| *state != PayloadDisposition::Present)
            {
                return Err(AppendError::InvalidReference(
                    "payload graph references frozen content".into(),
                ));
            }
            if done.contains(id) {
                return Ok(());
            }
            if !active.insert(id.clone()) {
                return Err(AppendError::InvalidReference("cyclic payload graph".into()));
            }
            let payload = payloads.get(id).ok_or_else(|| {
                AppendError::InvalidReference("payload graph target missing".into())
            })?;
            for reference in nested_refs(payload)? {
                let target = payloads.get(&reference.payload_id).ok_or_else(|| {
                    AppendError::InvalidReference("nested payload missing".into())
                })?;
                if reference != target.reference {
                    return Err(
                        ContractError::PayloadConflict(reference.payload_id.to_string()).into(),
                    );
                }
                visit(
                    &reference.payload_id,
                    payloads,
                    states,
                    active,
                    done,
                    depth + 1,
                )?;
            }
            active.remove(id);
            done.insert(id.clone());
            Ok(())
        }
        let mut deleting = HashSet::new();
        for event in &request.events {
            let erasure = matches!(
                event.body.kind(),
                EventKind::PayloadErasureRequested | EventKind::PayloadRedacted
            );
            let mut references = event.body.data.stored_refs()?;
            if matches!(event.body.data, EventData::ApprovalResolved { .. })
                && let Some(reply) = event.causation_id.as_ref().and_then(|id| prior.get(id))
                && let EventData::ControlInputRecorded { control_ref, .. } = &reply.body.data
                && control_ref.codec == "session.approval-reply/v1"
            {
                // Adopting a reply requires its content to remain available,
                // including every erasable replacement leaf in its graph.
                references.push(control_ref.clone());
            }
            for reference in references {
                let stored = payloads
                    .get(&reference.payload_id)
                    .ok_or_else(|| AppendError::InvalidReference("payload is absent".into()))?;
                if stored.reference != reference {
                    return Err(
                        ContractError::PayloadConflict(reference.payload_id.to_string()).into(),
                    );
                }
                if !erasure {
                    visit(
                        &reference.payload_id,
                        &payloads,
                        &states,
                        &mut HashSet::new(),
                        &mut HashSet::new(),
                        0,
                    )?;
                }
            }
            validate_semantic_payloads(
                history,
                &prior,
                &payloads,
                &message_ids,
                &approvals,
                event,
                request,
            )?;
            match &event.body.data {
                EventData::UserContentRecorded { content_ref, .. }
                | EventData::AssistantContentRecorded { content_ref, .. } => {
                    let row = payloads
                        .get(&content_ref.payload_id)
                        .ok_or_else(|| AppendError::InvalidReference("content absent".into()))?;
                    let content: ContentPayload = serde_json::from_slice(
                        row.body
                            .as_deref()
                            .ok_or_else(|| AppendError::Corrupt("content missing".into()))?,
                    )
                    .map_err(|error| AppendError::InvalidReference(error.to_string()))?;
                    message_ids.insert(content.message_id);
                }
                EventData::ApprovalRequested { request_id, .. } => {
                    approvals.insert(
                        (event.stream_id.clone(), request_id.clone()),
                        ApprovalState {
                            requested: event.event_id.clone(),
                            resolved: false,
                            invalidated: false,
                        },
                    );
                }
                EventData::ApprovalResolved { request_id, .. } => {
                    if let Some(value) =
                        approvals.get_mut(&(event.stream_id.clone(), request_id.clone()))
                    {
                        value.resolved = true;
                    }
                }
                EventData::ApprovalInvalidated { request_id, .. } => {
                    if let Some(value) =
                        approvals.get_mut(&(event.stream_id.clone(), request_id.clone()))
                    {
                        value.invalidated = true;
                    }
                }
                EventData::PayloadErasureRequested { targets, .. } => {
                    if targets.is_empty() {
                        return Err(AppendError::InvalidReference(
                            "empty erasure targets".into(),
                        ));
                    }
                    let events: Vec<_> = history.events().cloned().collect();
                    let existing: Vec<_> = self.records.values().cloned().collect();
                    let (_, commitment) = erasure_scope(&events, &existing, targets)?;
                    if request.erasure_reference_commitment.as_ref() != Some(&commitment) {
                        return Err(AppendError::ReferenceConflict(
                            "erasure reference set changed or not authorized".into(),
                        ));
                    }
                    for payload in payloads.values() {
                        nested_refs(payload)?;
                    }
                    for target in targets {
                        if !content_leaf(&target.codec) {
                            return Err(AppendError::InvalidReference(
                                "cannot erase identity or reference skeleton".into(),
                            ));
                        }
                        if state(&states, &target.payload_id) != PayloadDisposition::Present {
                            return Err(AppendError::InvalidTransition(
                                "erasure already requested".into(),
                            ));
                        }
                        states.insert(
                            target.payload_id.clone(),
                            PayloadDisposition::ErasurePending,
                        );
                    }
                }
                EventData::PayloadRedacted {
                    erasure_requested_event_id,
                } => {
                    let intent = prior.get(erasure_requested_event_id).ok_or_else(|| {
                        AppendError::InvalidReference("erasure intent missing".into())
                    })?;
                    let EventData::PayloadErasureRequested { targets, .. } = &intent.body.data
                    else {
                        return Err(AppendError::InvalidReference(
                            "redaction reference is not an intent".into(),
                        ));
                    };
                    for target in targets {
                        if state(&states, &target.payload_id) != PayloadDisposition::ErasurePending
                        {
                            return Err(AppendError::InvalidTransition(
                                "payload already redacted".into(),
                            ));
                        }
                        states.insert(target.payload_id.clone(), PayloadDisposition::Redacted);
                        deleting.insert(target.payload_id.clone());
                    }
                }
                _ => {}
            }
            prior.insert(event.event_id.clone(), event.clone());
        }
        for payload in &request.payloads {
            visit(
                &payload.payload_id,
                &payloads,
                &states,
                &mut HashSet::new(),
                &mut HashSet::new(),
                0,
            )?;
        }
        let provider_observations = self
            .provider_observations
            .plan(&request.events, &payloads)?;
        Ok(PayloadDelta {
            records: payloads.into_changes(),
            message_ids: message_ids.into_changes(),
            approvals: approvals.into_changes(),
            dispositions: states.into_changes(),
            deleting: deleting.into_iter().collect(),
            provider_observations,
        })
    }
}
/// Compatibility facade over the same incremental payload validator.
pub fn validate_payload_batch(
    history: &[SessionEvent],
    existing: &[StoredPayload],
    request: &AppendRequest,
) -> Result<Vec<PayloadId>, AppendError> {
    let facts = super::rules::HistoryIndex::from_history(history)?;
    let index = PayloadIndex::from_index(&facts, history, existing, &|_| Ok(None))?;
    Ok(index.plan(&facts, request)?.deleting)
}
fn validate_semantic_payloads(
    index: &super::rules::HistoryIndex,
    history: &super::index::OverlayMap<'_, EventId, SessionEvent>,
    payloads: &super::index::OverlayMap<'_, PayloadId, StoredPayload>,
    message_ids: &super::index::OverlaySet<'_, MessageId>,
    approvals: &super::index::OverlayMap<'_, (SessionStreamId, String), ApprovalState>,
    event: &SessionEvent,
    request: &AppendRequest,
) -> Result<(), AppendError> {
    fn decode<T: serde::de::DeserializeOwned>(
        payloads: &super::index::OverlayMap<'_, PayloadId, StoredPayload>,
        reference: &StoredPayloadRef,
        codec: &str,
    ) -> Result<T, AppendError> {
        if reference.codec != codec {
            return Err(AppendError::InvalidReference(format!(
                "expected codec {codec}"
            )));
        }
        let row = payloads
            .get(&reference.payload_id)
            .ok_or_else(|| AppendError::InvalidReference("semantic payload missing".into()))?;
        serde_json::from_slice(
            row.body
                .as_deref()
                .ok_or_else(|| AppendError::Corrupt("structural payload missing".into()))?,
        )
        .map_err(|error| AppendError::InvalidReference(error.to_string()))
    }
    fn validate_replacement(
        reply: &ApprovalReplyPayload,
        payloads: &super::index::OverlayMap<'_, PayloadId, StoredPayload>,
    ) -> Result<(), AppendError> {
        match (&reply.decision, &reply.replacement_ref) {
            (ApprovalReplyDecision::Replace, Some(reference)) => {
                let content = payloads.get(&reference.payload_id).ok_or_else(|| {
                    AppendError::InvalidReference("approval replacement absent".into())
                })?;
                if reference.codec != "session.text/v1"
                    || content.reference != *reference
                    || content
                        .body
                        .as_deref()
                        .is_some_and(|bytes| std::str::from_utf8(bytes).is_err())
                {
                    return Err(AppendError::InvalidReference(
                        "approval replacement unavailable".into(),
                    ));
                }
            }
            (ApprovalReplyDecision::Replace, None) => {
                return Err(AppendError::InvalidReference(
                    "approval replacement content reference required".into(),
                ));
            }
            (_, Some(_)) => {
                return Err(AppendError::InvalidReference(
                    "unexpected approval replacement content".into(),
                ));
            }
            (_, None) => {}
        }
        Ok(())
    }
    let invalid = |reason: &str| AppendError::InvalidReference(reason.into());
    match &event.body.data {
        EventData::SessionMetadataChanged {
            field,
            new_value_ref,
        } if field == "family_key" => {
            let family: String = decode(payloads, new_value_ref, "session.authority/v1")?;
            if family.is_empty() || family.len() > 512 {
                return Err(invalid("invalid family identity"));
            }
            let root = history.values().find(|prior| {
                prior.stream_id == event.stream_id && prior.body.kind() == EventKind::SessionStarted
            });
            if root.is_none() || history.values().any(|prior| prior.event_id != event.event_id
                && prior.stream_id == event.stream_id
                && matches!(&prior.body.data, EventData::SessionMetadataChanged { field, .. } if field == "family_key")) {
                return Err(invalid("family identity requires an unbound root stream"));
            }
            for prior in history.values().filter(|prior| {
                prior.event_id != event.event_id && prior.stream_id != event.stream_id
            }) {
                if let EventData::SessionMetadataChanged {
                    field,
                    new_value_ref,
                } = &prior.body.data
                    && field == "family_key"
                    && decode::<String>(payloads, new_value_ref, "session.authority/v1")? == family
                {
                    return Err(invalid("family identity already belongs to another root"));
                }
            }
        }
        EventData::UserContentRecorded { content_ref, .. }
        | EventData::AssistantContentRecorded { content_ref, .. } => {
            let content: ContentPayload = decode(payloads, content_ref, "session.content/v1")?;
            if message_ids.contains(&content.message_id) {
                return Err(invalid("message identity already created"));
            }
        }
        EventData::ContextProjectionCreated {
            result_ref,
            transform_started_event_id,
            ..
        } => {
            let context: Value = decode(payloads, result_ref, "session.context/v1")?;
            if let Some(items) = context.get("message_refs") {
                let refs: Vec<MessageRef> =
                    serde_json::from_value(items.clone()).map_err(|e| invalid(&e.to_string()))?;
                let mut ids = HashSet::new();
                let input = transform_started_event_id
                    .as_ref()
                    .and_then(|id| history.get(id))
                    .and_then(|e| match &e.body.data {
                        EventData::ContextTransformStarted {
                            input_projection_event_id,
                            ..
                        } => history.get(input_projection_event_id),
                        _ => None,
                    });
                let inherited = input
                    .and_then(|e| match &e.body.data {
                        EventData::ContextProjectionCreated { result_ref, .. } => {
                            payloads.get(&result_ref.payload_id)
                        }
                        _ => None,
                    })
                    .and_then(|p| p.body.as_ref())
                    .map(|body| serde_json::from_slice::<Value>(body))
                    .transpose()
                    .map_err(|e| invalid(&e.to_string()))?;
                let old_refs: Vec<MessageRef> = inherited
                    .as_ref()
                    .and_then(|v| v.get("message_refs"))
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|e| invalid(&e.to_string()))?
                    .unwrap_or_default();
                let lineage = event
                    .branch_id
                    .as_ref()
                    .map(|branch| {
                        super::lineage::frozen_lineage(
                            history.values(),
                            &event.stream_id,
                            branch,
                            u64::MAX,
                        )
                    })
                    .transpose()
                    .map_err(|e| invalid(&e.to_string()))?;
                let mut canonical_origins = HashMap::<EventId, Vec<MessageRef>>::new();
                for (index, item) in refs.iter().enumerate() {
                    if !ids.insert(&item.message_id) {
                        return Err(invalid("duplicate context message identity"));
                    }
                    match &item.source {
                        ContextMessageSource::Content {
                            content_event_id,
                            content_ref,
                        } => {
                            let source = history
                                .get(content_event_id)
                                .ok_or_else(|| invalid("context content event missing"))?;
                            if source.sequence == 0 && source.branch_id != event.branch_id {
                                return Err(invalid("uncommitted context source crosses branch"));
                            }
                            if !lineage
                                .as_ref()
                                .is_some_and(|lineage| super::lineage::includes(lineage, source))
                            {
                                return Err(invalid("context source outside frozen branch"));
                            }
                            let actual = match &source.body.data {
                                EventData::UserContentRecorded { content_ref, .. }
                                | EventData::AssistantContentRecorded { content_ref, .. } => {
                                    content_ref
                                }
                                EventData::ContextProjectionCreated {
                                    reason_key,
                                    result_ref,
                                    ..
                                } if reason_key == "legacy_import" => {
                                    let context: Value =
                                        decode(payloads, result_ref, "session.context/v1")?;
                                    if !context
                                        .get("legacy_messages")
                                        .and_then(Value::as_array)
                                        .is_some_and(|messages| {
                                            messages.iter().any(|message| {
                                                message.get("content_ref")
                                                    == Some(&serde_json::json!(content_ref))
                                            })
                                        })
                                    {
                                        return Err(invalid(
                                            "imported content has no canonical origin",
                                        ));
                                    }
                                    content_ref
                                }
                                _ => return Err(invalid("context content event kind mismatch")),
                            };
                            if actual != content_ref {
                                return Err(invalid("context content ref mismatch"));
                            }
                            let content: ContentPayload =
                                decode(payloads, content_ref, "session.content/v1")?;
                            if content.message_id != item.message_id {
                                return Err(invalid("context message identity mismatch"));
                            }
                        }
                        ContextMessageSource::Conversation {
                            payload_ref,
                            item_index,
                        } => {
                            if payload_ref.codec != "session.conversation/v1" {
                                return Err(invalid("context conversation codec mismatch"));
                            }
                            if old_refs.iter().any(|old| old == item) {
                                continue;
                            }
                            // Imported conversation chunks create identities in
                            // their projection. Reuse that exact canonical item
                            // without copying bytes into each subsequent chunk.
                            if let Some((origin, position)) = item
                                .message_id
                                .as_str()
                                .strip_prefix("context-message-")
                                .and_then(|id| id.rsplit_once('-'))
                                && let (Ok(origin), Ok(position)) =
                                    (EventId::parse(origin), position.parse::<usize>())
                                && let Some(source) = history.get(&origin)
                                && source.event_id != event.event_id
                                && source.sequence > 0
                                && source.stream_id == event.stream_id
                                && lineage.as_ref().is_some_and(|lineage| {
                                    super::lineage::includes(lineage, source)
                                })
                                && let EventData::ContextProjectionCreated { result_ref, .. } =
                                    &source.body.data
                            {
                                if !canonical_origins.contains_key(&origin) {
                                    let body: Value =
                                        decode(payloads, result_ref, "session.context/v1")?;
                                    let items = body
                                        .get("message_refs")
                                        .cloned()
                                        .unwrap_or_else(|| serde_json::json!([]));
                                    canonical_origins.insert(
                                        origin.clone(),
                                        serde_json::from_value(items)
                                            .map_err(|e| invalid(&e.to_string()))?,
                                    );
                                }
                                if canonical_origins
                                    .get(&origin)
                                    .and_then(|items| items.get(position))
                                    == Some(item)
                                {
                                    continue;
                                }
                            }
                            if let (Some(input), Some(inherited)) = (input, inherited.as_ref())
                                && inherited.get("messages_ref").is_some_and(|v| {
                                    serde_json::from_value::<StoredPayloadRef>(v.clone())
                                        .ok()
                                        .as_ref()
                                        == Some(payload_ref)
                                })
                            {
                                let expected = inherited
                                    .get("seed_messages")
                                    .and_then(Value::as_array)
                                    .and_then(|seeds| {
                                        seeds.iter().find(|s| {
                                            s.get("source_index").and_then(Value::as_u64)
                                                == Some(*item_index as u64)
                                        })
                                    })
                                    .and_then(|s| s.get("message_id"))
                                    .and_then(Value::as_str)
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| {
                                        format!("context-message-{}-{item_index}", input.event_id)
                                    });
                                if item.message_id.as_str() != expected {
                                    return Err(invalid("legacy context identity mismatch"));
                                }
                                continue;
                            }
                            let newly_supplied = request
                                .payloads
                                .iter()
                                .any(|p| p.payload_id == payload_ref.payload_id);
                            if !newly_supplied {
                                return Err(invalid(
                                    "context conversation source has no canonical origin",
                                ));
                            }
                            let rows: Vec<Value> =
                                decode(payloads, payload_ref, "session.conversation/v1")?;
                            if *item_index >= rows.len() {
                                return Err(invalid("context item index invalid"));
                            }
                            let origin = transform_started_event_id
                                .as_ref()
                                .unwrap_or(&event.event_id);
                            if item.message_id.as_str()
                                != format!("context-message-{origin}-{index}")
                            {
                                return Err(invalid("new context identity not bound to creation"));
                            }
                        }
                    }
                }
            }
        }
        EventData::FileAttached {
            file_id,
            content_event_id,
            part_index,
            ..
        } => {
            let content = history
                .get(content_event_id)
                .ok_or_else(|| invalid("content event missing"))?;
            let reference = match &content.body.data {
                EventData::UserContentRecorded { content_ref, .. }
                | EventData::AssistantContentRecorded { content_ref, .. } => content_ref,
                _ => return Err(invalid("invalid file content origin")),
            };
            let content: ContentPayload = decode(payloads, reference, "session.content/v1")?;
            if !matches!(content.parts.get(*part_index),Some(RecordedContentPart::File {file_id:id}) if id==file_id)
            {
                return Err(invalid("FileAttached part does not match content"));
            }
        }
        EventData::ToolCallProposed {
            provider_result_event_id,
            block_index,
        } => {
            let result = history
                .get(provider_result_event_id)
                .ok_or_else(|| invalid("provider result absent"))?;
            let EventData::ProviderAttemptCompleted { response_ref, .. } = &result.body.data else {
                return Err(invalid("tool proposal requires provider result"));
            };
            let response: ProviderResponsePayload =
                decode(payloads, response_ref, "session.response/v1")?;
            if !matches!(
                response.blocks.get(*block_index),
                Some(ResponseBlock::ToolCall { .. } | ResponseBlock::Text { .. })
            ) {
                return Err(invalid("proposal has no provider tool or text origin"));
            }
        }
        EventData::ControlInputRecorded {
            control_ref,
            expected_target_ref,
            ..
        } if control_ref.codec == "session.approval-reply/v1" => {
            let reply: ApprovalReplyPayload =
                decode(payloads, control_ref, "session.approval-reply/v1")?;
            validate_replacement(&reply, payloads)?;
            let requested = event
                .causation_id
                .as_ref()
                .and_then(|id| history.get(id))
                .ok_or_else(|| invalid("approval reply request absent"))?;
            let EventData::ApprovalRequested { request_id, .. } = &requested.body.data else {
                return Err(invalid("approval reply cause is not its request"));
            };
            if event.stream_id != requested.stream_id
                || event.branch_id != requested.branch_id
                || event.turn_id != requested.turn_id
                || event.invocation_id != requested.invocation_id
                || !matches!(expected_target_ref, TargetRef::Invocation { invocation_id } if Some(invocation_id) == event.invocation_id.as_ref())
                || approvals
                    .get(&(event.stream_id.clone(), request_id.clone()))
                    .is_none_or(|state| {
                        state.requested != requested.event_id || state.resolved || state.invalidated
                    })
            {
                return Err(invalid("approval reply is stale or cross-bound"));
            }
        }
        EventData::ApprovalResolved {
            request_id,
            resolution_ref,
        } => {
            let resolution: ApprovalResolution =
                decode(payloads, resolution_ref, "session.approval-resolution/v1")?;
            let approved = matches!(resolution, ApprovalResolution::Approved { .. });
            let evidence_ref = match &resolution {
                ApprovalResolution::Approved {
                    verification_evidence_ref,
                    ..
                }
                | ApprovalResolution::Denied {
                    verification_evidence_ref,
                    ..
                }
                | ApprovalResolution::ChannelUnsupported {
                    verification_evidence_ref,
                    ..
                } => verification_evidence_ref,
            };
            let evidence: Value = decode(payloads, evidence_ref, "session.evidence/v1")?;
            if matches!(resolution, ApprovalResolution::ChannelUnsupported { .. })
                && (evidence["source"] != "unsupported_backchannel"
                    || evidence["operator_reply_available"] != false
                    || event.causation_id.is_some())
            {
                return Err(invalid("invalid unsupported channel resolution"));
            }
            if evidence["operator_reply_available"] == false
                && (approved || event.causation_id.is_some())
            {
                return Err(invalid(
                    "runtime approval resolution claims an operator reply",
                ));
            }
            let decision = evidence
                .get("decision")
                .map(|value| {
                    serde_json::from_value::<ApprovalReplyDecision>(value.clone())
                        .map_err(|_| invalid("invalid approval resolution decision"))
                })
                .transpose()?;
            if decision.is_some_and(|decision| {
                approved
                    != matches!(
                        decision,
                        ApprovalReplyDecision::ApproveOnce | ApprovalReplyDecision::ApproveSession
                    )
            }) {
                return Err(invalid("approval resolution decision contradicts outcome"));
            }
            if decision == Some(ApprovalReplyDecision::Replace) && event.causation_id.is_none() {
                return Err(invalid(
                    "replacement resolution requires its observed reply",
                ));
            }
            if approvals
                .get(&(event.stream_id.clone(), request_id.clone()))
                .is_none_or(|state| state.resolved || state.invalidated)
            {
                return Err(invalid("approval request absent or already resolved"));
            }
            if let Some(reply_id) = &event.causation_id {
                let reply = history
                    .get(reply_id)
                    .ok_or_else(|| invalid("approval reply absent"))?;
                let EventData::ControlInputRecorded { control_ref, .. } = &reply.body.data else {
                    return Err(invalid("approval resolution cause is not a reply"));
                };
                let reply_payload: ApprovalReplyPayload =
                    decode(payloads, control_ref, "session.approval-reply/v1")?;
                validate_replacement(&reply_payload, payloads)?;
                let approval = approvals
                    .get(&(event.stream_id.clone(), request_id.clone()))
                    .ok_or_else(|| invalid("approval request absent"))?;
                if reply.stream_id != event.stream_id
                    || reply.branch_id != event.branch_id
                    || reply.turn_id != event.turn_id
                    || reply.invocation_id != event.invocation_id
                    || reply.causation_id.as_ref() != Some(&approval.requested)
                    || decision.is_some_and(|decision| decision != reply_payload.decision)
                    || approved
                        != matches!(
                            reply_payload.decision,
                            ApprovalReplyDecision::ApproveOnce
                                | ApprovalReplyDecision::ApproveSession
                        )
                {
                    return Err(invalid(
                        "approval resolution disagrees with authenticated reply",
                    ));
                }
            }
        }
        EventData::ToolEffectStarted {
            policy_event_id,
            confirmation_event_id: Some(confirmation),
            execution_ref,
            ..
        } => {
            let resolved = history
                .get(confirmation)
                .ok_or_else(|| invalid("approval resolution absent"))?;
            let EventData::ApprovalResolved {
                request_id,
                resolution_ref,
            } = &resolved.body.data
            else {
                return Err(invalid("approval reference is not resolution"));
            };
            let resolution: ApprovalResolution =
                decode(payloads, resolution_ref, "session.approval-resolution/v1")?;
            if let ApprovalResolution::ChannelUnsupported {
                confirmation_id, ..
            } = &resolution
            {
                let action: Value = decode(payloads, execution_ref, "session.tool-call/v1")?;
                if action["name"] != "shell" || confirmation_id != request_id {
                    return Err(invalid(
                        "unsupported channel fallback is restricted to shell",
                    ));
                }
            }
            let confirmation_id = match resolution {
                ApprovalResolution::Approved {
                    confirmation_id, ..
                }
                | ApprovalResolution::ChannelUnsupported {
                    confirmation_id, ..
                } => confirmation_id,
                ApprovalResolution::Denied { .. } => {
                    return Err(invalid("denied approval cannot be consumed"));
                }
            };
            let approval = approvals
                .get(&(event.stream_id.clone(), request_id.clone()))
                .ok_or_else(|| invalid("approval request absent"))?;
            let approved_request = history
                .get(&approval.requested)
                .ok_or_else(|| invalid("approval request event absent"))?;
            let EventData::ApprovalRequested {
                policy_event_id: approved_policy,
                expires_at,
                ..
            } = &approved_request.body.data
            else {
                return Err(invalid("approval request has wrong kind"));
            };
            if approved_policy!=policy_event_id || expires_at.is_some_and(|deadline|deadline<=event.occurred_at) || approval.invalidated || !request.events.iter().any(|item|item.invocation_id==event.invocation_id && matches!(&item.body.data,EventData::ApprovalConsumed {confirmation_id:id} if id==&confirmation_id)) {return Err(invalid("approval expired, stale, invalidated or not atomically consumed"));}
        }
        EventData::OutcomeReconciled {
            unknown_event_id,
            resolution,
            verification_evidence_ref,
        } => {
            let unknown = history
                .get(unknown_event_id)
                .ok_or_else(|| invalid("unknown event absent"))?;
            let origin = unknown
                .invocation_id
                .as_ref()
                .and_then(|invocation| {
                    index.invocation_origin(invocation).or_else(|| {
                        request.events.iter().find(|item| {
                            item.invocation_id.as_ref() == Some(invocation)
                                && item.body.kind() == EventKind::ChildInvocationRequested
                        })
                    })
                })
                .filter(|item| item.body.kind() == EventKind::ChildInvocationRequested);
            if let Some(origin) = origin {
                let evidence: ChildTerminalEvidence =
                    decode(payloads, verification_evidence_ref, "session.evidence/v1")?;
                let terminal = history
                    .get(&evidence.terminal_event_id)
                    .ok_or_else(|| invalid("child terminal evidence target absent"))?;
                let derived = index
                    .child_derivation(&origin.event_id)
                    .ok_or_else(|| invalid("child derivation missing"))?;
                super::rules::validate_child_terminal(origin, derived, terminal)?;
                if terminal.stream_id != evidence.child_terminal_cursor.stream_id
                    || terminal.sequence != evidence.child_terminal_cursor.sequence
                {
                    return Err(invalid("child terminal evidence cursor mismatch"));
                }
                let matches = match (&terminal.body.data, resolution) {
                    (EventData::TurnCancelled { .. }, Reconciliation::Cancelled) => true,
                    (EventData::TurnCommitted { completion_ref }, resolution) => {
                        let completion: TurnCompletion =
                            decode(payloads, completion_ref, "session.turn-completion/v1")?;
                        match (completion, resolution) {
                            (
                                TurnCompletion::Success { .. },
                                Reconciliation::Completed { result_ref: None },
                            ) => true,
                            (
                                TurnCompletion::Failed {
                                    error_ref: actual, ..
                                },
                                Reconciliation::Failed { error_ref },
                            ) => actual == *error_ref,
                            _ => false,
                        }
                    }
                    _ => false,
                };
                if !matches {
                    return Err(invalid("child reconciliation contradicts terminal outcome"));
                }
            }
        }
        EventData::TurnCommitted { completion_ref } => {
            let completion: TurnCompletion =
                decode(payloads, completion_ref, "session.turn-completion/v1")?;
            let ids = match completion {
                TurnCompletion::Success { result_event_ids } => result_event_ids,
                TurnCompletion::Failed {
                    observed_result_event_ids,
                    ..
                } => observed_result_event_ids,
            };
            for id in ids {
                if history.get(&id).is_none_or(|item| {
                    item.stream_id != event.stream_id || item.turn_id != event.turn_id
                }) {
                    return Err(invalid("turn completion result belongs to another turn"));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod approval_reply_tests {
    use super::*;

    #[test]
    fn legacy_replacement_reply_remains_readable_without_fabricating_content() {
        let payload = PayloadRecord::json(
            "session.approval-reply/v1",
            &serde_json::json!({"decision":"replace"}),
        )
        .expect("legacy reply");
        let reply: ApprovalReplyPayload = serde_json::from_slice(&payload.body).expect("old codec");
        assert_eq!(reply.decision, ApprovalReplyDecision::Replace);
        assert_eq!(reply.replacement_ref, None);
        assert!(
            nested_refs(&StoredPayload {
                reference: StoredPayloadRef::from(&payload),
                body: Some(payload.body),
            })
            .expect("historical graph retains its original skeleton")
            .is_empty()
        );
    }
}

#[cfg(test)]
mod response_read_tests {
    use super::*;

    #[test]
    fn future_blocks_preserve_slots_without_authorizing_their_references() {
        let leaf = PayloadRecord::new(PayloadId::new(), "session.text/v1".into(), b"text".to_vec())
            .expect("text");
        let reference = StoredPayloadRef::from(&leaf);
        let body = serde_json::json!({
            "blocks":[
                {"kind":"text", "text_ref":reference},
                {"kind":"future_audio", "raw_ref":reference},
                {"kind":"future_video", "provider_data":{"payload_id":"opaque provider ID"}},
                {"kind":"reasoning", "representation_kind":"text", "payload_ref":reference,
                 "completion":"complete", "replay_requirement":"none"},
                {"kind":"text", "text_ref":reference}
            ],
            "completion":"complete"
        });
        let bytes = canonical_json(&body).expect("encode");
        let view = response_references_for_read(&bytes).expect("read forward-compatible response");
        assert_eq!(view.unknown_block_indices, vec![1, 2]);
        assert!(
            matches!(&view.references[0], ResponseReadReference::Content(value) if value == &reference)
        );
        assert!(matches!(
            &view.references[1],
            ResponseReadReference::Unknown
        ));
        assert!(matches!(
            &view.references[2],
            ResponseReadReference::Unknown
        ));
        assert!(
            matches!(&view.references[3], ResponseReadReference::Reasoning(value) if value == &reference)
        );
        assert!(
            matches!(&view.references[4], ResponseReadReference::Content(value) if value == &reference)
        );
        assert!(serde_json::from_slice::<ProviderResponsePayload>(&bytes).is_err());
        let payload = PayloadRecord::new(PayloadId::new(), "session.response/v1".into(), bytes)
            .expect("response");
        assert!(
            nested_refs(&StoredPayload {
                reference: StoredPayloadRef::from(&payload),
                body: Some(payload.body),
            })
            .is_err()
        );
    }

    #[test]
    fn malformed_known_blocks_and_outer_structure_remain_errors() {
        for body in [
            serde_json::json!({"blocks":[{"kind":"text"}],"completion":"complete"}),
            serde_json::json!({"blocks":[{"kind":"reasoning"}],"completion":"complete"}),
            serde_json::json!({"blocks":[{"kind":"future_audio"}],"completion":"invalid"}),
            serde_json::json!({"blocks":[{"missing_kind":"future_audio"}],"completion":"complete"}),
            serde_json::json!({"blocks":{},"completion":"complete"}),
        ] {
            assert!(response_references_for_read(&canonical_json(&body).expect("encode")).is_err());
        }
    }
}
