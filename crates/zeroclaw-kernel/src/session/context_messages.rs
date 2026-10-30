//! Context planning reuses the API's canonical context-reference reader.
use super::*;
use std::collections::HashSet;
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};
fn context_read_error(error: ReadError) -> SessionError {
    match error {
        ReadError::OriginalPayloadErased(message) => SessionError::StaleTarget(message),
        error => error.into(),
    }
}

pub(super) fn context_refs(
    store: &dyn SessionEventStore,
    event: &SessionEvent,
) -> Result<Vec<MessageRef>, SessionError> {
    zeroclaw_api::session_events::context_read::context_refs(store, event)
        .map_err(context_read_error)
}

pub(super) fn resolve_message(
    store: &dyn SessionEventStore,
    stream: &SessionStreamId,
    item: &MessageRef,
) -> Result<Option<ConversationMessage>, SessionError> {
    zeroclaw_api::session_events::context_read::resolve_message(store, stream, item)
        .map_err(context_read_error)
}
use zeroclaw_api::session_events::*;

fn present(
    store: &dyn SessionEventStore,
    reference: &StoredPayloadRef,
) -> Result<PayloadRecord, SessionError> {
    zeroclaw_api::session_events::context_read::present(store, reference)
        .map_err(context_read_error)
}

pub(super) struct ContextPlan {
    pub refs: Vec<MessageRef>,
    pub payloads: Vec<PayloadRecord>,
    pub ranges: Vec<SourceRange>,
    pub summary_ref: Option<StoredPayloadRef>,
    pub summary_source_refs: Vec<ProviderBlockRef>,
}
impl SessionCoordinator {
    /// New assistant chat items create their identity in a content event.
    /// Context projections and final settlement only reference that creation.
    pub(super) fn record_context_assistants(
        &self,
        stream: &SessionStreamId,
        branch: &BranchId,
        turn: &TurnId,
        creation: &EventId,
        plan: &mut ContextPlan,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let history = self.stream_history(stream)?;
        let mut provenance_refs = Vec::new();
        let mut provider_parts = Vec::new();
        let mut provider_text = String::new();
        if let Some(event) = history.iter().rev().find(|event| {
            event.turn_id.as_ref() == Some(turn)
                && event.body.kind() == EventKind::ProviderAttemptCompleted
        }) {
            let EventData::ProviderAttemptCompleted { response_ref, .. } = &event.body.data else {
                return Err(SessionError::InvocationMismatch);
            };
            let response: ProviderResponsePayload =
                serde_json::from_slice(&present(self.store.as_ref(), response_ref)?.body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
            for (block_index, block) in response.blocks.iter().enumerate() {
                if let ResponseBlock::Text { text_ref } = block {
                    provider_text.push_str(
                        std::str::from_utf8(&present(self.store.as_ref(), text_ref)?.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?,
                    );
                    provider_parts.push(RecordedContentPart::Text {
                        text_ref: text_ref.clone(),
                    });
                }
                provenance_refs.push(ProviderBlockRef {
                    result_event_id: event.event_id.clone(),
                    block_index,
                });
            }
        }
        let mut events = Vec::new();
        let final_index = plan.refs.len().checked_sub(1);
        for (index, item) in plan.refs.iter_mut().enumerate() {
            let ContextMessageSource::Conversation {
                payload_ref,
                item_index,
            } = &item.source
            else {
                continue;
            };
            let Some(position) = plan
                .payloads
                .iter()
                .position(|p| p.payload_id == payload_ref.payload_id)
            else {
                continue;
            };
            let rows: Vec<serde_json::Value> =
                serde_json::from_slice(&plan.payloads[position].body)
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?;
            let row = rows
                .get(*item_index)
                .ok_or_else(|| ReadError::Corrupt("context item index invalid".into()))?;
            let Some(ConversationMessage::Chat(chat)) =
                super::engine::decode_context_item(row.clone()).map_err(SessionError::Operation)?
            else {
                continue;
            };
            if chat.role != "assistant" || !chat.attachments.is_empty() {
                continue;
            }
            let matching_provider = chat.content == provider_text;
            let mut parts = provider_parts.clone();
            if chat.content != provider_text || parts.is_empty() {
                let identity =
                    canonical_json(&(creation, index, &chat.content)).map_err(AppendError::from)?;
                let leaf = PayloadRecord::new(
                    PayloadId::parse(format!(
                        "context-text-{}",
                        Digest::from_bytes(&identity).as_str()
                    ))
                    .map_err(AppendError::from)?,
                    "session.text/v1".into(),
                    chat.content.into_bytes(),
                )
                .map_err(AppendError::from)?;
                parts = vec![RecordedContentPart::Text {
                    text_ref: StoredPayloadRef::from(&leaf),
                }];
                plan.payloads.push(leaf);
            }
            let content = super::retry::payload(
                creation,
                &format!("assistant-content-{index}"),
                "session.content/v1",
                &ContentPayload {
                    message_id: item.message_id.clone(),
                    parts,
                },
            )?;
            let mut event = SessionEvent::typed(
                stream.clone(),
                EventData::AssistantContentRecorded {
                    content_ref: StoredPayloadRef::from(&content),
                    provenance_refs: if matching_provider || Some(index) == final_index {
                        provenance_refs.clone()
                    } else {
                        vec![]
                    },
                },
                EventActor::System {
                    component: "context_transform".into(),
                },
            )
            .map_err(AppendError::from)?;
            event.event_id = EventId::parse(format!("context-content-{creation}-{index}"))
                .map_err(AppendError::from)?;
            event.branch_id = Some(branch.clone());
            event.turn_id = Some(turn.clone());
            event.causation_id = Some(creation.clone());
            item.source = ContextMessageSource::Content {
                content_event_id: event.event_id.clone(),
                content_ref: StoredPayloadRef::from(&content),
            };
            plan.payloads.remove(position);
            plan.payloads.push(content);
            events.push(event);
        }
        Ok(events)
    }

    pub(super) fn plan_context(
        &self,
        branch: &BranchId,
        input: &SessionEvent,
        creation: &EventId,
        messages: &[ConversationMessage],
        extra: &[SessionEvent],
        extra_payloads: &[PayloadRecord],
    ) -> Result<ContextPlan, SessionError> {
        let stream = &input.stream_id;
        let mut candidates = Vec::new();
        for item in context_refs(self.store.as_ref(), input)? {
            let Some(message) = resolve_message(self.store.as_ref(), stream, &item)? else {
                continue;
            };
            candidates.push((
                canonical_json(&message).map_err(AppendError::from)?,
                item,
                input.sequence,
            ));
        }
        let mut history = self.stream_history(stream)?;
        let lineage =
            lineage::frozen_lineage(&history, stream, branch, self.store.head(stream)?.sequence)?;
        history.extend_from_slice(extra);
        for event in &history {
            if !lineage::includes(&lineage, event)
                && !extra.iter().any(|e| e.event_id == event.event_id)
            {
                continue;
            }
            let (reference, role) = match &event.body.data {
                EventData::UserContentRecorded { content_ref, .. } => (content_ref, "user"),
                EventData::AssistantContentRecorded { content_ref, .. } => {
                    (content_ref, "assistant")
                }
                _ => continue,
            };
            let record = match extra_payloads
                .iter()
                .find(|p| p.payload_id == reference.payload_id)
            {
                Some(p) => p.clone(),
                None => match self.store.read_payload(&reference.payload_id)? {
                    PayloadRead::Present(p) => p,
                    _ => continue,
                },
            };
            let content: ContentPayload = serde_json::from_slice(&record.body)
                .map_err(|e| ReadError::Corrupt(e.to_string()))?;
            let mut text = String::new();
            let mut supported = true;
            for part in &content.parts {
                if let RecordedContentPart::Text { text_ref } = part {
                    let leaf = match extra_payloads
                        .iter()
                        .find(|p| p.payload_id == text_ref.payload_id)
                    {
                        Some(p) => p.clone(),
                        None => match self.store.read_payload(&text_ref.payload_id)? {
                            PayloadRead::Present(p) => p,
                            _ => {
                                supported = false;
                                break;
                            }
                        },
                    };
                    text.push_str(
                        std::str::from_utf8(&leaf.body)
                            .map_err(|e| ReadError::Corrupt(e.to_string()))?,
                    );
                } else {
                    supported = false;
                    break;
                }
            }
            if supported {
                let message = ConversationMessage::Chat(ChatMessage {
                    tool_calls: Vec::new(),
                    tool_result: None,
                    reasoning_content: None,
                    role: role.into(),
                    content: text,

                    attachments: Vec::new(),
                });
                candidates.push((
                    canonical_json(&message).map_err(AppendError::from)?,
                    MessageRef {
                        message_id: content.message_id,
                        source: ContextMessageSource::Content {
                            content_event_id: event.event_id.clone(),
                            content_ref: reference.clone(),
                        },
                    },
                    event.sequence,
                ));
            }
        }
        let mut used = HashSet::new();
        let mut refs = Vec::new();
        let mut payloads = Vec::new();
        for (index, message) in messages.iter().enumerate() {
            let value =
                serde_json::to_value(message).map_err(|e| ReadError::Corrupt(e.to_string()))?;
            let bytes = canonical_json(&value).map_err(AppendError::from)?;
            if let Some((_, item, _)) = candidates
                .iter()
                .find(|(body, item, _)| *body == bytes && !used.contains(&item.message_id))
            {
                used.insert(item.message_id.clone());
                refs.push(item.clone());
            } else {
                let payload = super::retry::payload(
                    creation,
                    &format!("message-{index}"),
                    "session.conversation/v1",
                    &vec![value],
                )?;
                let message_id = MessageId::parse(format!("context-message-{creation}-{index}"))
                    .map_err(AppendError::from)?;
                used.insert(message_id.clone());
                refs.push(MessageRef {
                    message_id,
                    source: ContextMessageSource::Conversation {
                        payload_ref: StoredPayloadRef::from(&payload),
                        item_index: 0,
                    },
                });
                payloads.push(payload);
            }
        }
        // Ranges describe immutable source events. A conversation source may
        // supply several items: mark it included if any item is retained, never
        // emit contradictory dispositions for the same event coordinate.
        let mut coordinates = std::collections::BTreeMap::new();
        for (_, item, sequence) in candidates {
            if sequence == 0 {
                continue;
            }
            let included = used.contains(&item.message_id);
            coordinates
                .entry(sequence)
                .and_modify(|keep| *keep |= included)
                .or_insert(included);
        }
        let ranges = coordinates
            .into_iter()
            .map(|(sequence, included)| SourceRange {
                stream_id: stream.clone(),
                from_seq: sequence,
                to_seq: sequence,
                disposition: if included {
                    RangeDisposition::Included
                } else {
                    RangeDisposition::Omitted
                },
            })
            .collect();
        let summary_ref = match &input.body.data {
            EventData::ContextProjectionCreated { summary_ref: Some(summary), .. }
                if refs.iter().any(|item| matches!(&item.source,
                    ContextMessageSource::Conversation { payload_ref, .. } if payload_ref == summary)) => Some(summary.clone()),
            _ => None,
        };
        let summary_source_refs = match &input.body.data {
            EventData::ContextProjectionCreated {
                summary_source_refs,
                ..
            } if summary_ref.is_some() => summary_source_refs.clone(),
            _ => vec![],
        };
        Ok(ContextPlan {
            refs,
            payloads,
            ranges,
            summary_ref,
            summary_source_refs,
        })
    }
}
