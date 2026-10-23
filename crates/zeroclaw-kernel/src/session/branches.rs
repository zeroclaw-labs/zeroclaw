//! Authorized branch controls and frozen context lookup; no independent cache.
use super::engine::EngineSession;
use super::query::{SessionAccess, SessionAccessPolicy};
use super::*;
use zeroclaw_api::session_events::*;

impl SessionCoordinator {
    pub(super) fn stream_history(
        &self,
        stream: &SessionStreamId,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let head = self.store.head(stream)?;
        let mut after = EventCursor::beginning(stream.clone());
        let mut events = Vec::new();
        loop {
            let page = self
                .store
                .read_page(stream, &after, Some(&head), 256, 4 * 1024 * 1024)?;
            if let Some(last) = page.events.last() {
                after.sequence = last.sequence;
            }
            events.extend(page.events);
            if !page.has_more {
                return Ok(events);
            }
        }
    }
    pub(super) fn branch_context(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
    ) -> Result<Option<SessionEvent>, SessionError> {
        // The common path remains indexed; ancestors need a frozen lookup.
        if let Some(event) = self.store.read_latest_event(
            stream,
            branch,
            None,
            &EventKind::ContextProjectionCreated,
        )? {
            return Ok(Some(event));
        }
        let history = self.stream_history(stream)?;
        let lineage =
            lineage::frozen_lineage(&history, stream, branch, self.store.head(stream)?.sequence)?;
        Ok(history.into_iter().rev().find(|e| {
            e.body.kind() == EventKind::ContextProjectionCreated && lineage::includes(&lineage, e)
        }))
    }
}
impl EngineSession {
    pub async fn fork(&self, branch: BranchId, cursor: EventCursor) -> anyhow::Result<Self> {
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let stream = self.stream.clone();
        let parent = self.branch.clone();
        let actor = self.actor.clone();
        let child = branch.clone();
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = lane.lock().await;
        self.owner.blocking(move || {
            SessionAccessPolicy::authorize_actor(authority.as_ref(), &actor, &stream, SessionAccess::Write)?;
            authority.authorize_context(&stream, &parent)?;
            let lock = owner.lock_for(&stream);
            let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
            let mut event = SessionEvent::typed(stream.clone(), EventData::BranchCreated { parent_branch_id: parent, parent_cursor: cursor }, actor).map_err(AppendError::from)?;
            event.branch_id = Some(child.clone());
            let empty = matches!(&event.body.data, EventData::BranchCreated { parent_cursor, .. } if parent_cursor.sequence == 0);
            let key = format!("branch-create:{child}");
            if let Some(receipt) = owner.store.lookup_receipt(&stream, &key)? {
                let prior = owner.store.read_event(receipt.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("branch receipt empty".into()))?)?.ok_or_else(|| SessionError::StaleTarget("branch receipt event absent".into()))?;
                if prior.body != event.body || prior.actor != event.actor || prior.branch_id != event.branch_id || receipt.event_ids.len() != if empty { 2 } else { 1 } {
                    return Err(AppendError::IdempotencyConflict(key).into());
                }
                return Ok(());
            }
            let mut events = vec![event]; let mut payloads = Vec::new();
            if empty {
                let context = PayloadRecord::json("session.context/v1", &serde_json::json!({"message_refs":[], "history_has_trim_breadcrumb":false})).map_err(AppendError::from)?;
                let mut projection = SessionEvent::typed(stream.clone(), EventData::ContextProjectionCreated {
                    source_ranges: vec![], summary_ref: None, budget: 0, reason_key: "empty_branch_context".into(), algorithm_version: "branch/v1".into(), result_ref: StoredPayloadRef::from(&context), summary_source_refs: vec![], transform_started_event_id: None,
                }, events[0].actor.clone()).map_err(AppendError::from)?;
                projection.branch_id = Some(child); projection.causation_id = Some(events[0].event_id.clone());
                events.push(projection); payloads.push(context);
            }
            owner.store.append_batch(AppendRequest { stream_id: stream.clone(), expected_head: owner.store.head(&stream)?, idempotency_key: key, events, payloads, erasure_reference_commitment: None })?;
            let _ = owner.wakeups.send(stream); Ok(())
        }).await?;
        let mut fork = self.clone();
        fork.branch = branch;
        Ok(fork)
    }
    /// Hide a visible message in this branch without rewriting its source.
    pub async fn hide_content(&self, target: EventId, reason: &str) -> anyhow::Result<()> {
        self.supersede_content(
            target,
            Supersession::Hidden {
                reason_key: reason.into(),
            },
        )
        .await
    }
    pub async fn replace_content(
        &self,
        target: EventId,
        replacement: EventId,
    ) -> anyhow::Result<()> {
        self.supersede_content(
            target,
            Supersession::Replaced {
                replacement_event_id: replacement,
            },
        )
        .await
    }
    async fn supersede_content(
        &self,
        target: EventId,
        disposition: Supersession,
    ) -> anyhow::Result<()> {
        let owner = self.owner.clone();
        let authority = self.authority.clone();
        let stream = self.stream.clone();
        let branch = self.branch.clone();
        let actor = self.actor.clone();
        let lane = self.owner.foreground_lock(&self.stream);
        let _permit = lane.lock().await;
        self.owner
            .blocking(move || {
                SessionAccessPolicy::authorize_actor(
                    authority.as_ref(),
                    &actor,
                    &stream,
                    SessionAccess::Write,
                )?;
                authority.authorize_context(&stream, &branch)?;
                let lock = owner.lock_for(&stream);
                let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
                let history = owner.stream_history(&stream)?;
                let lineage = lineage::frozen_lineage(
                    &history,
                    &stream,
                    &branch,
                    owner.store.head(&stream)?.sequence,
                )?;
                for id in std::iter::once(&target).chain(match &disposition {
                    Supersession::Replaced {
                        replacement_event_id,
                    } => Some(replacement_event_id),
                    _ => None,
                }) {
                    let source = history
                        .iter()
                        .find(|e| &e.event_id == id)
                        .ok_or_else(|| SessionError::StaleTarget("content target absent".into()))?;
                    if !lineage::includes(&lineage, source)
                        || !matches!(
                            source.body.kind(),
                            EventKind::UserContentRecorded | EventKind::AssistantContentRecorded
                        )
                    {
                        return Err(SessionError::InvalidAdmission(
                            "content outside frozen branch".into(),
                        ));
                    }
                }
                let mut event = SessionEvent::typed(
                    stream.clone(),
                    EventData::ContentSuperseded {
                        target_event_ids: vec![target.clone()],
                        disposition: disposition.clone(),
                    },
                    actor,
                )
                .map_err(AppendError::from)?;
                event.branch_id = Some(branch.clone());
                let key = format!("content-supersede:{branch}:{target}");
                if let Some(receipt) = owner.store.lookup_receipt(&stream, &key)? {
                    let prior = owner.store.read_event(receipt.event_ids.first().ok_or_else(|| SessionError::StoreUnhealthy("empty content receipt".into()))?)?.ok_or_else(|| SessionError::StaleTarget("content receipt event absent".into()))?;
                    if prior.body != event.body || prior.actor != event.actor || prior.branch_id != event.branch_id { return Err(AppendError::IdempotencyConflict(key).into()); }
                    return Ok(());
                }
                let mut events = vec![event]; let mut payloads = Vec::new();
                if let Some(input) = owner.branch_context(&stream, &branch)? {
                    let mut refs = super::context_messages::context_refs(owner.store.as_ref(), &input)?;
                    let target_event = owner.store.read_event(&target)?.ok_or_else(|| SessionError::StaleTarget("superseded content missing".into()))?;
                    let target_ref = match &target_event.body.data {
                        EventData::UserContentRecorded { content_ref, .. } | EventData::AssistantContentRecorded { content_ref, .. } => content_ref,
                        _ => return Err(SessionError::InvocationMismatch),
                    };
                    let PayloadRead::Present(target_content) = owner.store.read_payload(&target_ref.payload_id)? else { return Err(SessionError::StaleTarget("superseded structure missing".into())); };
                    let target_content: ContentPayload = serde_json::from_slice(&target_content.body).map_err(|e| ReadError::Corrupt(e.to_string()))?;
                    let replacement = if let Supersession::Replaced { replacement_event_id } = &disposition {
                        let replacement = owner.store.read_event(replacement_event_id)?.ok_or_else(|| SessionError::StaleTarget("replacement missing".into()))?;
                        let content_ref = match &replacement.body.data { EventData::UserContentRecorded { content_ref, .. } | EventData::AssistantContentRecorded { content_ref, .. } => content_ref.clone(), _ => return Err(SessionError::InvocationMismatch) };
                        let PayloadRead::Present(content) = owner.store.read_payload(&content_ref.payload_id)? else { return Err(SessionError::StaleTarget("replacement structure missing".into())); };
                        let content: ContentPayload = serde_json::from_slice(&content.body).map_err(|e| ReadError::Corrupt(e.to_string()))?;
                        let item = MessageRef { message_id:content.message_id,source:ContextMessageSource::Content {content_event_id:replacement.event_id,content_ref} };
                        super::context_messages::resolve_message(owner.store.as_ref(), &stream, &item)?;
                        Some(item)
                    } else { None };
                    let position = refs.iter().position(|item| item.message_id == target_content.message_id);
                    if let Some(index) = position {
                        refs.remove(index);
                        if let Some(item) = replacement { refs.retain(|r| r.message_id != item.message_id); refs.insert(index.min(refs.len()), item); }
                    }
                    let (summary_ref, summary_source_refs) = match &input.body.data {
                        EventData::ContextProjectionCreated { summary_ref: Some(reference), summary_source_refs, .. }
                            if refs.iter().any(|item| matches!(&item.source, ContextMessageSource::Conversation {payload_ref,..} if payload_ref == reference)) => (Some(reference.clone()), summary_source_refs.clone()),
                        _ => (None, vec![]),
                    };
                    let binding = authority.maintenance_binding()?;
                    let spec = PayloadRecord::json("session.transform/v1", &serde_json::json!({"reason_key":"content_superseded","algorithm_version":"branch-edit/v1","budget":0})).map_err(AppendError::from)?;
                    let mut started = SessionEvent::typed(stream.clone(), EventData::ContextTransformStarted {input_projection_event_id:input.event_id,transform_spec_ref:StoredPayloadRef::from(&spec),execution_binding_ref:StoredPayloadRef::from(&binding)},events[0].actor.clone()).map_err(AppendError::from)?;
                    started.branch_id=Some(branch.clone());started.causation_id=Some(events[0].event_id.clone());
                    let context = PayloadRecord::json("session.context/v1", &serde_json::json!({"message_refs":refs,"history_has_trim_breadcrumb":true})).map_err(AppendError::from)?;
                    let mut projection = SessionEvent::typed(stream.clone(),EventData::ContextProjectionCreated {source_ranges:vec![SourceRange {stream_id:stream.clone(),from_seq:target_event.sequence,to_seq:target_event.sequence,disposition:RangeDisposition::Superseded}],summary_ref,budget:0,reason_key:"content_superseded".into(),algorithm_version:"branch-edit/v1".into(),result_ref:StoredPayloadRef::from(&context),summary_source_refs,transform_started_event_id:Some(started.event_id.clone())},events[0].actor.clone()).map_err(AppendError::from)?;
                    projection.branch_id=Some(branch.clone());projection.causation_id=Some(started.event_id.clone());
                    events.extend([started,projection]);payloads.extend([binding,spec,context]);
                }
                owner.store.append_batch(AppendRequest {
                    stream_id: stream.clone(),
                    expected_head: owner.store.head(&stream)?,
                    idempotency_key: key,
                    events,
                    payloads,
                    erasure_reference_commitment: None,
                })?;
                let _ = owner.wakeups.send(stream);
                Ok(())
            })
            .await?;
        Ok(())
    }
}
