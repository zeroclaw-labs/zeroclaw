//! Ordered context references. Unchanged messages reuse their canonical source;
//! a transform creates only its new normalized items, never a second transcript.
use super::*;
use crate::model_provider::{ChatMessage, ConversationMessage};

pub fn present(
    store: &dyn SessionEventStore,
    reference: &StoredPayloadRef,
) -> Result<PayloadRecord, ReadError> {
    let PayloadRead::Present(record) = store.read_payload(&reference.payload_id)? else {
        return Err(ReadError::OriginalPayloadErased(
            "context source content unavailable".into(),
        ));
    };
    if StoredPayloadRef::from(&record) != *reference {
        return Err(ReadError::Corrupt("context reference mismatch".into()));
    }
    Ok(record)
}

pub fn resolve_message(
    store: &dyn SessionEventStore,
    stream: &SessionStreamId,
    item: &MessageRef,
) -> Result<Option<ConversationMessage>, ReadError> {
    match &item.source {
        ContextMessageSource::Conversation {
            payload_ref,
            item_index,
        } => {
            let record = present(store, payload_ref)?;
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&record.body)
                .map_err(|e| ReadError::Corrupt(e.to_string()))?;
            let row = rows
                .get(*item_index)
                .ok_or_else(|| ReadError::Corrupt("context item index invalid".into()))?;
            decode_context_item(row.clone())
        }
        ContextMessageSource::Content {
            content_event_id,
            content_ref,
        } => {
            let event = store
                .read_event(content_event_id)?
                .ok_or_else(|| ReadError::Gap("context content event absent".into()))?;
            if event.stream_id != *stream {
                return Err(ReadError::Corrupt(
                    "context reference outside stream".into(),
                ));
            }
            let (role, attachments, metadata): (
                String,
                Vec<crate::media::AttachmentReference>,
                serde_json::Value,
            ) = match &event.body.data {
                EventData::UserContentRecorded {
                    content_ref: actual,
                    ..
                } if actual == content_ref => {
                    ("user".to_owned(), Vec::new(), serde_json::Value::Null)
                }
                EventData::AssistantContentRecorded {
                    content_ref: actual,
                    ..
                } if actual == content_ref => {
                    ("assistant".to_owned(), Vec::new(), serde_json::Value::Null)
                }
                EventData::ContextProjectionCreated {
                    reason_key,
                    result_ref,
                    ..
                } if reason_key == "legacy_import" => {
                    let context: serde_json::Value =
                        serde_json::from_slice(&present(store, result_ref)?.body)
                            .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                    let source = context
                        .get("legacy_messages")
                        .and_then(serde_json::Value::as_array)
                        .and_then(|messages| {
                            messages.iter().find(|message| {
                                message.get("content_ref") == Some(&serde_json::json!(content_ref))
                            })
                        })
                        .ok_or_else(|| {
                            ReadError::Corrupt("imported context source missing".into())
                        })?;
                    let role = source
                        .get("role")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            ReadError::Corrupt("imported context origin missing".into())
                        })?
                        .to_owned();
                    let attachments = source
                        .get("attachments")
                        .map(|value| serde_json::from_value(value.clone()))
                        .transpose()
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?
                        .unwrap_or_default();
                    (role, attachments, source.clone())
                }
                _ => {
                    return Err(ReadError::Corrupt("context content source mismatch".into()));
                }
            };
            let content: ContentPayload =
                serde_json::from_slice(&present(store, content_ref)?.body)
                    .map_err(|e| ReadError::Corrupt(e.to_string()))?;
            if content.message_id != item.message_id {
                return Err(ReadError::Corrupt(
                    "context message identity mismatch".into(),
                ));
            }
            let mut text = String::new();
            for part in content.parts {
                match part {
                    RecordedContentPart::Text { text_ref } => text.push_str(
                        std::str::from_utf8(&present(store, &text_ref)?.body)
                            .map_err(|e| ReadError::Corrupt(e.to_string()))?,
                    ),
                    _ => {
                        return Err(ReadError::Corrupt(
                            "content source requires a complete chat representation".into(),
                        ));
                    }
                }
            }
            let mut fields = metadata.as_object().cloned().unwrap_or_default();
            fields.insert("role".into(), role.into());
            fields.insert("content".into(), text.into());
            fields.insert("attachments".into(), serde_json::json!(attachments));
            let message: ChatMessage = serde_json::from_value(serde_json::Value::Object(fields))
                .map_err(|error| ReadError::Corrupt(error.to_string()))?;
            Ok(Some(message.into_conversation()))
        }
    }
}

pub fn context_refs(
    store: &dyn SessionEventStore,
    event: &SessionEvent,
) -> Result<Vec<MessageRef>, ReadError> {
    let EventData::ContextProjectionCreated { result_ref, .. } = &event.body.data else {
        return Err(ReadError::Corrupt("context source kind mismatch".into()));
    };
    let body: serde_json::Value = serde_json::from_slice(&present(store, result_ref)?.body)
        .map_err(|e| ReadError::Corrupt(e.to_string()))?;
    if let Some(refs) = body.get("message_refs") {
        return serde_json::from_value(refs.clone()).map_err(|e| ReadError::Corrupt(e.to_string()));
    }
    if let Some(messages) = body
        .get("legacy_messages")
        .and_then(serde_json::Value::as_array)
    {
        return messages
            .iter()
            .map(|message| {
                let content_ref: StoredPayloadRef =
                    serde_json::from_value(message["content_ref"].clone())
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                let content: ContentPayload =
                    serde_json::from_slice(&present(store, &content_ref)?.body)
                        .map_err(|error| ReadError::Corrupt(error.to_string()))?;
                Ok(MessageRef {
                    message_id: content.message_id,
                    source: ContextMessageSource::Content {
                        content_event_id: event.event_id.clone(),
                        content_ref,
                    },
                })
            })
            .collect();
    }
    // Compatibility with the initial/legacy host context: identities derive
    // from its immutable creation coordinate, rather than each later transform.
    let Some(reference) = body.get("messages_ref") else {
        return Ok(Vec::new());
    };
    let reference: StoredPayloadRef =
        serde_json::from_value(reference.clone()).map_err(|e| ReadError::Corrupt(e.to_string()))?;
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&present(store, &reference)?.body)
        .map_err(|e| ReadError::Corrupt(e.to_string()))?;
    rows.iter()
        .enumerate()
        .map(|(index, _)| {
            let id = body
                .get("seed_messages")
                .and_then(serde_json::Value::as_array)
                .and_then(|seeds| {
                    seeds.iter().find(|s| {
                        s.get("source_index").and_then(serde_json::Value::as_u64)
                            == Some(index as u64)
                    })
                })
                .and_then(|s| s.get("message_id"))
                .cloned();
            let message_id = if let Some(id) = id {
                serde_json::from_value(id).map_err(|e| ReadError::Corrupt(e.to_string()))?
            } else {
                MessageId::parse(format!("context-message-{}-{index}", event.event_id))
                    .map_err(|error| ReadError::Corrupt(error.to_string()))?
            };
            Ok(MessageRef {
                message_id,
                source: ContextMessageSource::Conversation {
                    payload_ref: reference.clone(),
                    item_index: index,
                },
            })
        })
        .collect()
}

/// Decode known conversation variants; future variants are skipped.
pub fn decode_context_item(
    row: serde_json::Value,
) -> Result<Option<ConversationMessage>, ReadError> {
    if let Some(kind) = row.get("type") {
        if !matches!(
            kind.as_str(),
            Some("Chat" | "AssistantToolCalls" | "ToolResults")
        ) {
            return Ok(None);
        }
        serde_json::from_value(row)
            .map(Some)
            .map_err(|error| ReadError::Corrupt(error.to_string()))
    } else {
        serde_json::from_value(row)
            .map(|chat| Some(ConversationMessage::Chat(chat)))
            .map_err(|error| ReadError::Corrupt(error.to_string()))
    }
}
