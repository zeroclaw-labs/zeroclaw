//! Authorized paginated reads and durable-cursor subscriptions.
use super::*;
mod summary;
use zeroclaw_api::principal::PrincipalId;
use zeroclaw_api::session_events::payloads::{
    PayloadDisposition, ResponseReadReference, StoredPayload, nested_refs,
    response_references_for_read,
};
use zeroclaw_api::session_events::{EventData, EventPage, PayloadRead, StoredPayloadRef};

#[derive(Clone, Copy, Debug)]
pub enum SessionAccess {
    Read,
    Write,
    Erase,
}

/// Implementations resolve current policy on every call. Holding this resolver
/// never holds a historical permission snapshot or a bearer credential.
pub trait SessionAccessPolicy: Send + Sync {
    /// Reading ordinary history does not disclose model reasoning or opaque
    /// continuation bytes. Hosts opt in using their current trusted authority.
    fn authorize_reasoning(
        &self,
        _actor: &EventActor,
        _stream: &SessionStreamId,
    ) -> Result<(), SessionError> {
        Err(SessionError::PermissionDenied(
            "reasoning disclosure denied".into(),
        ))
    }
    /// A System actor is an admitted host component, never an invented
    /// PrincipalId. Existing principal-only policies default closed to it.
    fn authorize_actor(
        &self,
        actor: &EventActor,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError> {
        match actor {
            EventActor::Principal { principal_id } => {
                self.authorize(&PrincipalId(principal_id.clone()), stream, access)
            }
            EventActor::System { .. } | EventActor::ChannelResponder { .. } => Err(
                SessionError::PermissionDenied("system session admission unsupported".into()),
            ),
        }
    }
    fn authorize(
        &self,
        principal: &PrincipalId,
        stream: &SessionStreamId,
        access: SessionAccess,
    ) -> Result<(), SessionError>;
}

pub use zeroclaw_api::session_events::views::{
    HistoricalMessage, HistoricalMessagePage, MessageCursor, PayloadSelector, SessionSummary,
};

pub struct SessionQuery {
    coordinator: Arc<SessionCoordinator>,
    actor: EventActor,
    policy: Arc<dyn SessionAccessPolicy>,
    branch: Option<BranchId>,
}

impl SessionCoordinator {
    pub fn query(
        self: &Arc<Self>,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
    ) -> SessionQuery {
        self.query_actor(
            EventActor::Principal {
                principal_id: principal.0,
            },
            policy,
        )
    }
    pub fn query_actor(
        self: &Arc<Self>,
        actor: EventActor,
        policy: Arc<dyn SessionAccessPolicy>,
    ) -> SessionQuery {
        SessionQuery {
            coordinator: Arc::clone(self),
            actor,
            policy,
            branch: None,
        }
    }

    pub async fn request_erasure(
        self: &Arc<Self>,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
        request: AppendRequest,
    ) -> Result<AppendReceipt, SessionError> {
        self.erasure_command(principal, policy, request, true).await
    }

    #[cfg(test)]
    pub(super) async fn admit_erasure(
        self: &Arc<Self>,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
        request: AppendRequest,
    ) -> Result<AppendReceipt, SessionError> {
        self.erasure_command(principal, policy, request, false)
            .await
    }

    async fn erasure_command(
        self: &Arc<Self>,
        principal: PrincipalId,
        policy: Arc<dyn SessionAccessPolicy>,
        mut request: AppendRequest,
        finish: bool,
    ) -> Result<AppendReceipt, SessionError> {
        policy.authorize(&principal, &request.stream_id, SessionAccess::Erase)?;
        if request.events.iter().any(|event| {
            !matches!(&event.body.data, EventData::PayloadErasureRequested { .. })
                || event.actor
                    != EventActor::Principal {
                        principal_id: principal.0.clone(),
                    }
        }) {
            return Err(SessionError::InvalidAdmission(
                "erasure command contains foreign facts or actor".into(),
            ));
        }
        let targets: Vec<_> = request
            .events
            .iter()
            .filter_map(|event| match &event.body.data {
                EventData::PayloadErasureRequested { targets, .. } => Some(targets.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        if targets.is_empty() {
            return Err(SessionError::InvalidAdmission(
                "missing erasure intent".into(),
            ));
        }
        let owner = Arc::clone(self);
        self.blocking(move || {
            let lock = owner.lock_for(&request.stream_id);
            let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let (scopes, commitment) = owner.store.erasure_scope(&targets)?;
            if scopes.is_empty() {
                return Err(SessionError::InvalidAdmission(
                    "erasure target has no authorized event reference".into(),
                ));
            }
            policy.authorize(&principal, &request.stream_id, SessionAccess::Erase)?;
            for stream in scopes {
                policy.authorize(&principal, &stream, SessionAccess::Erase)?;
            }
            request.erasure_reference_commitment = Some(commitment);
            // Backend rechecks the graph while holding the publication lock.
            let stream = request.stream_id.clone();
            let receipt = owner.store.append_batch(request)?;
            let _ = owner.wakeups.send(stream.clone());
            if finish {
                // This worker owns both durable phases even if its async
                // caller stops waiting after the intent commits.
                owner.finish_erasures_locked(&stream)?;
            }
            Ok(receipt)
        })
        .await
    }
}

impl SessionQuery {
    pub fn on_branch(mut self, branch: BranchId) -> Self {
        self.branch = Some(branch);
        self
    }
    /// One contiguous, fixed history prefix. Authorization is checked on every
    /// page; concurrent appends cannot move this projection's frontier.
    pub async fn project_history(
        &self,
        stream: SessionStreamId,
    ) -> Result<zeroclaw_api::session_events::projection::HistoryProjection, SessionError> {
        let mut projection =
            zeroclaw_api::session_events::projection::HistoryProjection::empty(stream.clone());
        let mut through = None;
        loop {
            let page = self
                .read_page(
                    stream.clone(),
                    projection.source_cursor.clone(),
                    through.clone(),
                    256,
                )
                .await?;
            through = Some(page.through.clone());
            projection.apply_page(&page)?;
            for event in &page.events {
                if let EventData::ChildInvocationCompleted {
                    child_terminal_cursor,
                    ..
                } = &event.body.data
                {
                    let owner = self.coordinator.clone();
                    let terminal = child_terminal_cursor.clone();
                    let status = self
                        .coordinator
                        .blocking(move || {
                            use zeroclaw_api::session_events::{
                                TurnCompletion, projection::InvocationStatus,
                            };
                            let page = owner.store.read_page(
                                &terminal.stream_id,
                                &EventCursor {
                                    stream_id: terminal.stream_id.clone(),
                                    sequence: terminal.sequence - 1,
                                },
                                Some(&terminal),
                                1,
                            )?;
                            let event = page
                                .events
                                .first()
                                .ok_or_else(|| ReadError::Gap("child terminal missing".into()))?;
                            match &event.body.data {
                                EventData::TurnCancelled { .. } => Ok(InvocationStatus::Cancelled),
                                EventData::TurnCommitted { completion_ref } => {
                                    let PayloadRead::Present(record) =
                                        owner.store.read_payload(&completion_ref.payload_id)?
                                    else {
                                        return Err(ReadError::Corrupt(
                                            "child terminal skeleton absent".into(),
                                        )
                                        .into());
                                    };
                                    let completion: TurnCompletion =
                                        serde_json::from_slice(&record.body)
                                            .map_err(|e| ReadError::Corrupt(e.to_string()))?;
                                    Ok(match completion {
                                        TurnCompletion::Success { .. } => {
                                            InvocationStatus::Completed
                                        }
                                        TurnCompletion::Failed { .. } => InvocationStatus::Failed,
                                    })
                                }
                                _ => {
                                    Err(ReadError::Corrupt("invalid child terminal kind".into())
                                        .into())
                                }
                            }
                        })
                        .await?;
                    if let Some(invocation) = event
                        .invocation_id
                        .as_ref()
                        .and_then(|id| projection.invocations.get_mut(id))
                    {
                        invocation.status = status;
                    }
                }
            }
            if !page.has_more {
                self.policy
                    .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
                return Ok(projection);
            }
        }
    }

    /// Text compatibility view over canonical message facts. File identity is
    /// returned independently; transports must not infer files from markers.
    pub async fn messages(
        &self,
        stream: SessionStreamId,
    ) -> Result<Vec<HistoricalMessage>, SessionError> {
        Ok(self
            .message_window(stream, None, None, None)
            .await?
            .messages)
    }

    pub async fn search_messages(
        &self,
        stream: SessionStreamId,
        keyword: &str,
        limit: usize,
    ) -> Result<Vec<HistoricalMessage>, SessionError> {
        if keyword.trim().is_empty() || keyword.len() > 4096 || !(1..=1000).contains(&limit) {
            return Err(SessionError::InvalidAdmission(
                "invalid message search bounds".into(),
            ));
        }
        // Resolve the canonical visible messages with current authorization and
        // erasure before building the index; never filter global hits afterward.
        let messages = self.messages(stream.clone()).await?;
        let search = self.coordinator.search.clone().ok_or_else(|| {
            SessionError::InvalidAdmission("session search capability is unavailable".into())
        })?;
        let keyword = keyword.to_owned();
        let needle = keyword.trim().to_lowercase();
        let matches = self
            .coordinator
            .blocking(move || {
                let texts: Vec<_> = messages
                    .iter()
                    .map(|message| message.text.as_str())
                    .collect();
                let positions = search.matching_positions(&texts, &keyword, limit)?;
                positions
                    .into_iter()
                    .map(|position| {
                        messages.get(position).cloned().ok_or_else(|| {
                            SessionError::Read(ReadError::Corrupt(
                                "search position outside authorized window".into(),
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        // Erasure may race materialization. Re-read selected messages from the
        // latest view before returning any content from the ephemeral index.
        let current = self.messages(stream).await?;
        let ids: std::collections::HashSet<_> = matches
            .into_iter()
            .map(|message| message.message_id)
            .collect();
        Ok(current
            .into_iter()
            .filter(|message| {
                ids.contains(&message.message_id) && message.text.to_lowercase().contains(&needle)
            })
            .collect())
    }
    /// Freeze selection at the event prefix while resolving permission and
    /// erasure afresh. Only page-selected native content leaves are loaded.
    pub async fn message_page(
        &self,
        stream: SessionStreamId,
        cursor: Option<MessageCursor>,
        limit: usize,
    ) -> Result<HistoricalMessagePage, SessionError> {
        if limit == 0 || limit > 1000 {
            return Err(SessionError::InvalidAdmission(
                "message page limit must be 1..=1000".into(),
            ));
        }
        if cursor.as_ref().is_some_and(|cursor| {
            cursor.through.stream_id != stream || cursor.branch != self.branch
        }) {
            return Err(SessionError::InvalidAdmission(
                "message cursor belongs to another stream".into(),
            ));
        }
        let (through, before) = cursor.map_or((None, None), |cursor| {
            (Some(cursor.through), Some(cursor.before))
        });
        self.message_window(stream, through, before, Some(limit))
            .await
    }
    async fn message_window(
        &self,
        stream: SessionStreamId,
        frozen: Option<EventCursor>,
        before: Option<usize>,
        limit: Option<usize>,
    ) -> Result<HistoricalMessagePage, SessionError> {
        let mut history = Vec::new();
        let mut after = EventCursor::beginning(stream.clone());
        let mut through = frozen;
        loop {
            let page = self
                .read_page(stream.clone(), after.clone(), through.clone(), 256)
                .await?;
            through = Some(page.through.clone());
            if let Some(last) = page.events.last() {
                after.sequence = last.sequence;
            }
            history.extend(page.events);
            if !page.has_more {
                break;
            }
        }
        let branch = self.branch.clone().or_else(|| {
            history
                .iter()
                .find(|e| {
                    matches!(
                        e.body.kind(),
                        EventKind::SessionStarted | EventKind::DerivedSessionStarted
                    )
                })
                .and_then(|e| e.branch_id.clone())
        });
        let lineage = branch
            .map(|branch| {
                zeroclaw_api::session_events::lineage::frozen_lineage(
                    &history,
                    &stream,
                    &branch,
                    after.sequence,
                )
            })
            .transpose()?;
        let mut imported = Vec::new();
        let mut seeds = Vec::new();
        let mut messages = Vec::new();
        for event in &history {
            if lineage
                .as_ref()
                .is_some_and(|l| !zeroclaw_api::session_events::lineage::includes(l, event))
            {
                continue;
            }
            match &event.body.data {
                EventData::ContextProjectionCreated { reason_key, .. }
                    if reason_key == "legacy_import" =>
                {
                    let PayloadRead::Present(record) = self
                        .read_payload(
                            stream.clone(),
                            PayloadSelector {
                                event_sequence: event.sequence,
                                reference_path: vec![0],
                            },
                        )
                        .await?
                    else {
                        return Err(SessionError::StaleTarget(
                            "legacy history structure unavailable".into(),
                        ));
                    };
                    let value: serde_json::Value = serde_json::from_slice(&record.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    for (index, entry) in value
                        .get("legacy_messages")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| ReadError::Corrupt("legacy messages absent".into()))?
                        .iter()
                        .enumerate()
                    {
                        let role = entry
                            .get("role")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| ReadError::Corrupt("legacy role absent".into()))?;
                        let created_at = match entry.get("created_at") {
                            None | Some(serde_json::Value::Null) => None,
                            Some(value) => Some(value.as_i64().ok_or_else(|| {
                                ReadError::Corrupt("legacy message timestamp invalid".into())
                            })?),
                        };
                        let attachments = entry
                            .get("attachments")
                            .cloned()
                            .map(
                                serde_json::from_value::<
                                    Vec<zeroclaw_api::media::AttachmentReference>,
                                >,
                            )
                            .transpose()
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?
                            .unwrap_or_default();
                        imported.push((
                            event.sequence,
                            vec![0, index],
                            role.to_owned(),
                            created_at,
                            attachments,
                        ));
                    }
                }
                EventData::ContextProjectionCreated { reason_key, .. }
                    if reason_key == "host_seed" =>
                {
                    seeds.extend(self.seed_messages_at(&stream, event).await?)
                }
                EventData::UserContentRecorded { .. } => messages.push((
                    event.event_id.clone(),
                    event.sequence,
                    "user",
                    event.occurred_at,
                )),
                EventData::AssistantContentRecorded { .. } => messages.push((
                    event.event_id.clone(),
                    event.sequence,
                    "assistant",
                    event.occurred_at,
                )),
                EventData::ContentSuperseded {
                    target_event_ids, ..
                } => messages.retain(|(id, _, _, _)| !target_event_ids.contains(id)),
                _ => {}
            }
        }
        for (_, sequence, role, time) in messages {
            imported.push((sequence, vec![0], role.into(), Some(time), Vec::new()));
        }
        let total = seeds.len() + imported.len();
        let end = before.unwrap_or(total);
        if end > total {
            return Err(SessionError::InvalidAdmission(
                "message cursor outside fixed history".into(),
            ));
        }
        let start = end.saturating_sub(limit.unwrap_or(total));
        let mut selected = Vec::new();
        for (index, message) in seeds.into_iter().enumerate() {
            if (start..end).contains(&index) {
                selected.push(message);
            }
        }
        let seed_count = total - imported.len();
        for (index, (sequence, path, role, created_at, attachments)) in
            imported.into_iter().enumerate()
        {
            if (start..end).contains(&(seed_count + index)) {
                let mut message = self.message_at(&stream, sequence, path, &role).await?;
                message.created_at = created_at;
                message.attachments.extend(attachments);
                if !message.files.is_empty() {
                    // Representations belong to the original content event,
                    // not a later use of the same file ID. Read through the
                    // authorized payload port so erasure remains effective.
                    let content_event_id = history
                        .iter()
                        .find(|event| event.sequence == sequence)
                        .map(|event| &event.event_id);
                    for event in &history {
                        if let EventData::FileUsed {
                            file_id, operation, ..
                        } = &event.body.data
                            && message.files.contains(file_id)
                            && operation == "ingress_representation_captured"
                            && event.causation_id.as_ref() == content_event_id
                            && let Some(reference) = self.ingress_attachment(&stream, event).await?
                        {
                            message.attachments.push(reference);
                        }
                    }
                }
                selected.push(message);
            }
        }
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        Ok(HistoricalMessagePage {
            messages: selected,
            next_cursor: if start > 0 {
                Some(MessageCursor {
                    through: through
                        .ok_or_else(|| ReadError::Gap("message prefix absent".into()))?,
                    before: start,
                    branch: self.branch.clone(),
                })
            } else {
                None
            },
        })
    }

    /// Resolve the attachment from this input's captured representation or
    /// metadata. Never load raw file bytes merely to build a history row.
    async fn ingress_attachment(
        &self,
        stream: &SessionStreamId,
        event: &SessionEvent,
    ) -> Result<Option<zeroclaw_api::media::AttachmentReference>, SessionError> {
        let EventData::FileUsed {
            representation_ref,
            policy_evidence_ref,
            ..
        } = &event.body.data
        else {
            return Ok(None);
        };
        let typed = representation_ref.codec == "session.attachment-reference/v1";
        let reference = if typed {
            representation_ref
        } else {
            policy_evidence_ref
        };
        let roots = event.body.data.stored_refs().map_err(AppendError::from)?;
        let index = roots
            .iter()
            .position(|root| root == reference)
            .ok_or_else(|| ReadError::Corrupt("file reference missing from event".into()))?;
        let mut selector = PayloadSelector {
            event_sequence: event.sequence,
            reference_path: vec![index],
        };
        let PayloadRead::Present(record) =
            self.read_payload(stream.clone(), selector.clone()).await?
        else {
            return Ok(None);
        };
        if typed {
            return serde_json::from_slice(&record.body)
                .map(Some)
                .map_err(|error| ReadError::Corrupt(error.to_string()).into());
        }
        let evidence: serde_json::Value = serde_json::from_slice(&record.body)
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let Some(metadata) = evidence.get("observed_file_metadata_ref") else {
            return Ok(None);
        };
        let metadata: StoredPayloadRef = serde_json::from_value(metadata.clone())
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let children = nested_refs(&StoredPayload {
            reference: reference.clone(),
            body: Some(record.body),
        })?;
        let index = children
            .iter()
            .position(|child| child == &metadata)
            .ok_or_else(|| ReadError::Corrupt("file metadata reference missing".into()))?;
        selector.reference_path.push(index);
        let PayloadRead::Present(record) = self.read_payload(stream.clone(), selector).await?
        else {
            return Ok(None);
        };
        let metadata: serde_json::Value = serde_json::from_slice(&record.body)
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        metadata
            .get("reference")
            .filter(|reference| !reference.is_null())
            .map(|reference| {
                serde_json::from_value(reference.clone())
                    .map_err(|error| ReadError::Corrupt(error.to_string()).into())
            })
            .transpose()
    }

    /// Materialize only chat rows from the seed, never exposing raw native
    /// context, reasoning continuation or opaque tool extensions to a viewer.
    async fn seed_messages_at(
        &self,
        stream: &SessionStreamId,
        event: &SessionEvent,
    ) -> Result<Vec<HistoricalMessage>, SessionError> {
        self.policy
            .authorize_actor(&self.actor, stream, SessionAccess::Read)?;
        let EventData::ContextProjectionCreated { result_ref, .. } = &event.body.data else {
            return Err(ReadError::Corrupt("seed context kind mismatch".into()).into());
        };
        let store = self.coordinator.store.clone();
        let reference = result_ref.clone();
        let messages = self
            .coordinator
            .blocking(move || {
                let structure = store.read_payload(&reference.payload_id)?;
                verify_reference(&reference, &structure)?;
                let PayloadRead::Present(structure) = structure else {
                    return Err(SessionError::StaleTarget("seed context unavailable".into()));
                };
                let context: serde_json::Value = serde_json::from_slice(&structure.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                let reference: StoredPayloadRef =
                    serde_json::from_value(context.get("messages_ref").cloned().ok_or_else(
                        || ReadError::Corrupt("seed messages reference missing".into()),
                    )?)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                let raw = store.read_payload(&reference.payload_id)?;
                verify_reference(&reference, &raw)?;
                let (conversation, unavailable) = match raw {
                    PayloadRead::Present(raw) => (
                        Some(
                            serde_json::from_slice::<Vec<serde_json::Value>>(&raw.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                        ),
                        None,
                    ),
                    PayloadRead::ErasurePending => (None, Some(PayloadDisposition::ErasurePending)),
                    PayloadRead::Redacted => (None, Some(PayloadDisposition::Redacted)),
                };
                let entries = context
                    .get("seed_messages")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| ReadError::Corrupt("seed identities missing".into()))?;
                let mut messages = Vec::new();
                for entry in entries {
                    let message_id: zeroclaw_api::session_events::MessageId =
                        serde_json::from_value(
                            entry.get("message_id").cloned().ok_or_else(|| {
                                ReadError::Corrupt("seed identity missing".into())
                            })?,
                        )
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    let index = entry
                        .get("source_index")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|index| usize::try_from(index).ok())
                        .ok_or_else(|| ReadError::Corrupt("seed index invalid".into()))?;
                    if let Some(disposition) = unavailable {
                        let role = entry
                            .get("role")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown");
                        if role == "system" {
                            return Err(ReadError::Corrupt(
                                "seed system disclosure refused".into(),
                            )
                            .into());
                        }
                        messages.push(HistoricalMessage {
                            created_at: None,
                            message_id,
                            role: role.into(),
                            text: String::new(),
                            files: vec![],
                            attachments: vec![],
                            unresolved_parts: vec![],
                            unavailable_parts: vec![
                                zeroclaw_api::session_events::UnavailableContentPart {
                                    part_index: 0,
                                    disposition,
                                },
                            ],
                            content_source: None,
                        });
                        continue;
                    }
                    let Some(zeroclaw_api::model_provider::ConversationMessage::Chat(chat)) =
                        super::engine::decode_context_item(
                            conversation
                                .as_ref()
                                .ok_or_else(|| {
                                    ReadError::Corrupt("seed conversation absent".into())
                                })?
                                .get(index)
                                .ok_or_else(|| ReadError::Corrupt("seed index absent".into()))?
                                .clone(),
                        )
                        .map_err(|e| ReadError::Corrupt(e.to_string()))?
                    else {
                        return Err(ReadError::Corrupt("seed chat missing".into()).into());
                    };
                    if chat.role == "system" {
                        return Err(
                            ReadError::Corrupt("seed system disclosure refused".into()).into()
                        );
                    }
                    messages.push(HistoricalMessage {
                        created_at: None,
                        message_id,
                        role: chat.role.clone(),
                        text: chat.content.clone(),
                        files: vec![],
                        attachments: chat.attachments.clone(),
                        unresolved_parts: vec![],
                        unavailable_parts: vec![],
                        content_source: None,
                    });
                }
                Ok(messages)
            })
            .await?;
        self.policy
            .authorize_actor(&self.actor, stream, SessionAccess::Read)?;
        Ok(messages)
    }

    async fn message_at(
        &self,
        stream: &SessionStreamId,
        sequence: u64,
        path: Vec<usize>,
        role: &str,
    ) -> Result<HistoricalMessage, SessionError> {
        let PayloadRead::Present(record) = self
            .read_payload(
                stream.clone(),
                PayloadSelector {
                    event_sequence: sequence,
                    reference_path: path.clone(),
                },
            )
            .await?
        else {
            return Err(SessionError::StaleTarget(
                "historical message structure unavailable".into(),
            ));
        };
        let content: zeroclaw_api::session_events::ContentPayload =
            serde_json::from_slice(&record.body)
                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let mut text = String::new();
        let mut files = Vec::new();
        // Reference paths enumerate stored payload references, excluding File
        // parts. Keep a separate reference ordinal from the content part index.
        let mut reference = 0;
        let raw: serde_json::Value = serde_json::from_slice(&record.body)
            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
        let mut unresolved_parts = Vec::new();
        let mut unavailable_parts = Vec::new();
        for (part_index, part) in content.parts.iter().enumerate() {
            match part {
                zeroclaw_api::session_events::RecordedContentPart::Text { .. } => {
                    let mut leaf = path.clone();
                    leaf.push(reference);
                    reference += 1;
                    let content = self
                        .read_payload(
                            stream.clone(),
                            PayloadSelector {
                                event_sequence: sequence,
                                reference_path: leaf,
                            },
                        )
                        .await?;
                    match content {
                        PayloadRead::Present(record) => text.push_str(
                            std::str::from_utf8(&record.body)
                                .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                        ),
                        PayloadRead::ErasurePending | PayloadRead::Redacted => {
                            unavailable_parts.push(
                                zeroclaw_api::session_events::UnavailableContentPart {
                                    part_index,
                                    disposition: if matches!(content, PayloadRead::ErasurePending) {
                                        PayloadDisposition::ErasurePending
                                    } else {
                                        PayloadDisposition::Redacted
                                    },
                                },
                            );
                            // A removed span must not join formerly separated words.
                            if !text.is_empty() {
                                text.push('\n');
                            }
                        }
                    }
                }
                zeroclaw_api::session_events::RecordedContentPart::File { file_id } => {
                    files.push(file_id.clone())
                }
                zeroclaw_api::session_events::RecordedContentPart::Opaque { .. } => {
                    unresolved_parts.push(part_index);
                    // Preserve the payload in the event graph while omitting
                    // unknown content from the legacy text projection.
                    reference += 1;
                }
                zeroclaw_api::session_events::RecordedContentPart::Other => {
                    unresolved_parts.push(part_index);
                    // Unknown parts may contain several nested references.
                    // Derive ordinals from canonical raw bytes, not the lossy enum.
                    reference += zeroclaw_api::session_events::payloads::refs_in_value(
                        &raw["parts"][part_index],
                    )?
                    .len();
                }
            }
        }
        Ok(HistoricalMessage {
            created_at: None,
            message_id: content.message_id,
            role: role.into(),
            text,
            files,
            attachments: Vec::new(),
            unresolved_parts,
            unavailable_parts,
            content_source: Some(PayloadSelector {
                event_sequence: sequence,
                reference_path: path,
            }),
        })
    }
    pub async fn latest_event(
        &self,
        stream: SessionStreamId,
        branch: BranchId,
        turn: Option<TurnId>,
        kind: EventKind,
    ) -> Result<Option<SessionEvent>, SessionError> {
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        let store = Arc::clone(&self.coordinator.store);
        let scope = stream.clone();
        let event = self
            .coordinator
            .blocking(move || Ok(store.read_latest_event(&scope, &branch, turn.as_ref(), &kind)?))
            .await?;
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        Ok(event)
    }
    pub async fn read_page(
        &self,
        stream: SessionStreamId,
        after: EventCursor,
        through: Option<EventCursor>,
        max_events: usize,
    ) -> Result<EventPage, SessionError> {
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        let store = Arc::clone(&self.coordinator.store);
        let scope = stream.clone();
        let page = self
            .coordinator
            .blocking(move || Ok(store.read_page(&scope, &after, through.as_ref(), max_events)?))
            .await?;
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        Ok(page)
    }

    pub async fn read_payload(
        &self,
        stream: SessionStreamId,
        selector: PayloadSelector,
    ) -> Result<PayloadRead, SessionError> {
        if selector.event_sequence == 0
            || selector.reference_path.is_empty()
            || selector.reference_path.len() > 32
        {
            return Err(ReadError::TooLarge.into());
        }
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        let store = Arc::clone(&self.coordinator.store);
        let scope = stream.clone();
        let policy = self.policy.clone();
        let actor = self.actor.clone();
        let result = self
            .coordinator
            .blocking(move || {
                let cursor = EventCursor {
                    stream_id: scope.clone(),
                    sequence: selector.event_sequence,
                };
                let after = EventCursor {
                    stream_id: scope.clone(),
                    sequence: selector.event_sequence - 1,
                };
                let page = store.read_page(&scope, &after, Some(&cursor), 1)?;
                let event = page
                    .events
                    .first()
                    .ok_or_else(|| ReadError::Gap("payload selector event absent".into()))?;
                let mut refs: Vec<_> = event
                    .body
                    .data
                    .stored_refs()
                    .map_err(AppendError::from)?
                    .into_iter()
                    .map(Some)
                    .collect();
                let mut reasoning = false;
                for (depth, index) in selector.reference_path.iter().enumerate() {
                    let reference = refs.get(*index).and_then(Option::as_ref).ok_or_else(|| {
                        ReadError::AccessDenied("selector is outside authorized references".into())
                    })?;
                    if matches!(
                        reference.codec.as_str(),
                        "session.reasoning/v1" | "session.conversation/v1"
                    ) {
                        reasoning = true;
                        policy.authorize_reasoning(&actor, &scope)?;
                    }
                    let content = store.read_payload(&reference.payload_id)?;
                    verify_reference(reference, &content)?;
                    let PayloadRead::Present(record) = content else {
                        return Ok((content, reasoning));
                    };
                    let response = (record.codec == "session.response/v1")
                        .then(|| response_references_for_read(&record.body))
                        .transpose()?;
                    if depth + 1 == selector.reference_path.len() {
                        if response
                            .as_ref()
                            .is_some_and(|view| !view.unknown_block_indices.is_empty())
                        {
                            reasoning = true;
                            policy.authorize_reasoning(&actor, &scope)?;
                        }
                        return Ok((PayloadRead::Present(record), reasoning));
                    }
                    if response.as_ref().is_some_and(|view| {
                        matches!(
                            view.references.get(selector.reference_path[depth + 1]),
                            Some(ResponseReadReference::Reasoning(_))
                        )
                    }) {
                        reasoning = true;
                    }
                    // Stream checkpoints can include reasoning fragments and
                    // finalized blocks. Their narrow raw surface is restricted
                    // as a whole instead of guessing a provider's component.
                    if record.codec == "session.context/v1"
                        && serde_json::from_slice::<serde_json::Value>(&record.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?
                            .get("message_refs")
                            .is_some()
                    {
                        reasoning = true;
                    }
                    if record.codec == "session.provider-observation/v1" {
                        reasoning = true;
                    }
                    if reasoning {
                        policy.authorize_reasoning(&actor, &scope)?;
                    }
                    refs = if let Some(response) = response {
                        response
                            .references
                            .into_iter()
                            .map(|reference| match reference {
                                ResponseReadReference::Content(reference)
                                | ResponseReadReference::Reasoning(reference) => Some(reference),
                                ResponseReadReference::Unknown => None,
                            })
                            .collect()
                    } else {
                        nested_refs(&StoredPayload {
                            reference: reference.clone(),
                            body: Some(record.body),
                        })?
                        .into_iter()
                        .map(Some)
                        .collect()
                    };
                }
                Err(SessionError::InvalidAdmission("empty payload path".into()))
            })
            .await?;
        self.policy
            .authorize_actor(&self.actor, &stream, SessionAccess::Read)?;
        if result.1 {
            self.policy.authorize_reasoning(&self.actor, &stream)?;
        }
        // A content read races an erasure request exactly like an ordinary
        // file read: backend serializes each read against intent publication.
        Ok(result.0)
    }

    pub fn subscribe(self, cursor: EventCursor) -> SessionSubscription {
        // Subscribe before the first history read: a racing commit is either
        // inside that read or wakes its successor. Notifications carry no data.
        let wakeups = self.coordinator.wakeups.subscribe();
        SessionSubscription {
            query: self,
            cursor,
            offered_through: None,
            wakeups,
        }
    }
}

fn verify_reference(
    reference: &StoredPayloadRef,
    content: &PayloadRead,
) -> Result<(), SessionError> {
    if let PayloadRead::Present(record) = content
        && StoredPayloadRef::from(record) != *reference
    {
        return Err(ReadError::Corrupt("payload selector metadata mismatch".into()).into());
    }
    Ok(())
}

pub enum SubscriptionUpdate {
    Page(EventPage),
    Lagged {
        resume_after: EventCursor,
    },
    ResyncRequired {
        resume_after: EventCursor,
        reason: String,
    },
}

pub struct SessionSubscription {
    query: SessionQuery,
    cursor: EventCursor,
    offered_through: Option<EventCursor>,
    wakeups: broadcast::Receiver<SessionStreamId>,
}

impl SessionSubscription {
    pub async fn validate_cursor(&self) -> Result<(), SessionError> {
        self.query
            .read_page(
                self.cursor.stream_id.clone(),
                self.cursor.clone(),
                Some(self.cursor.clone()),
                1,
            )
            .await?;
        Ok(())
    }
    /// Hosts recheck after obtaining transport capacity, before disclosing an
    /// already-materialized page. A cursor carries no authorization grant.
    pub fn authorize_current(&self) -> Result<(), SessionError> {
        self.query.policy.authorize_actor(
            &self.query.actor,
            &self.cursor.stream_id,
            SessionAccess::Read,
        )
    }
    /// Advance only after the caller has accepted a page. Cancellation of a
    /// wait never loses history or implicitly acknowledges delivery.
    pub fn acknowledge(&mut self, cursor: EventCursor) -> Result<(), SessionError> {
        if cursor.stream_id != self.cursor.stream_id
            || cursor.sequence < self.cursor.sequence
            || cursor.sequence
                > self
                    .offered_through
                    .as_ref()
                    .map_or(self.cursor.sequence, |end| end.sequence)
        {
            return Err(SessionError::StaleTarget(
                "subscription acknowledgement".into(),
            ));
        }
        self.cursor = cursor;
        Ok(())
    }

    pub async fn next(&mut self, max_events: usize) -> Result<SubscriptionUpdate, SessionError> {
        loop {
            let page = match self
                .query
                .read_page(
                    self.cursor.stream_id.clone(),
                    self.cursor.clone(),
                    None,
                    max_events,
                )
                .await
            {
                Err(SessionError::Read(ReadError::Gap(reason))) => {
                    return Ok(SubscriptionUpdate::ResyncRequired {
                        resume_after: self.cursor.clone(),
                        reason,
                    });
                }
                result => result?,
            };
            if !page.events.is_empty() {
                self.offered_through = page.events.last().map(|event| EventCursor {
                    stream_id: page.stream_id.clone(),
                    sequence: event.sequence,
                });
                return Ok(SubscriptionUpdate::Page(page));
            }
            tokio::select! {
                message = self.wakeups.recv() => match message {
                    Err(broadcast::error::RecvError::Lagged(_)) => return Ok(SubscriptionUpdate::Lagged {resume_after:self.cursor.clone()}),
                    Err(broadcast::error::RecvError::Closed) => return Err(SessionError::StoreUnhealthy("subscription writer closed".into())),
                    Ok(_) => {}
                },
                // Other process/handle publications have no local wakeup.
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
            }
        }
    }
}
