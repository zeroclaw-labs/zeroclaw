//! Conversion of pre-event-store media syntax. Live messages never use this parser.
use zeroclaw_api::media::{AttachmentKind, AttachmentReference};
use zeroclaw_api::model_provider::{ChatMessage, ConversationMessage};

/// Convert the old prompt carrier only while importing the legacy flat
/// transcript. Live history never classifies body text this way.
pub(crate) fn upgrade_legacy_tool_carrier(message: &mut ChatMessage, ordinal: usize) {
    use zeroclaw_api::model_provider::{ToolCall, ToolResultMessage};
    if message.role == "user" {
        if let Some(body) = message.content.strip_prefix("[Tool results]\n") {
            *message = ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: format!("legacy-tool-{ordinal}"),
                tool_name: String::new(),
                success: None,
                content: body.to_owned(),
                attachments: std::mem::take(&mut message.attachments),
            });
        }
        return;
    }
    if message.tool_result.is_some() || !message.tool_calls.is_empty() {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&message.content) else {
        return;
    };
    if message.role == "tool" {
        if let (Some(id), Some(body)) = (value["tool_call_id"].as_str(), value.get("content")) {
            *message = ChatMessage::tool_result(ToolResultMessage {
                tool_call_id: id.into(),
                content: body
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| body.to_string()),
                tool_name: value["name"].as_str().unwrap_or_default().into(),
                success: value["success"].as_bool(),
                attachments: std::mem::take(&mut message.attachments),
            });
        }
    } else if message.role == "assistant"
        && let Some(calls) = value.get("tool_calls")
        && let Ok(calls) = serde_json::from_value::<Vec<ToolCall>>(calls.clone())
    {
        message.content = value["content"].as_str().unwrap_or_default().into();
        message.tool_calls = calls;
        message.reasoning_content = value["reasoning_content"].as_str().map(str::to_owned);
    }
}

pub(super) fn upgrade_conversation(message: &mut ConversationMessage, ordinal: usize) {
    if let ConversationMessage::Chat(chat) = message {
        upgrade_chat(chat);
        upgrade_legacy_tool_carrier(chat, ordinal);
        *message = chat.clone().into_conversation();
    }
}

pub(crate) fn upgrade_chat(message: &mut ChatMessage) {
    // A structured declaration already owns attachment identity. Never infer a
    // second copy from body text.
    if !message.attachments.is_empty() {
        return;
    }
    if message.role == "tool" {
        if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&message.content)
            && value.get("tool_call_id").is_some()
            && let Some(raw) = value
                .as_object_mut()
                .and_then(|object| object.remove("attachments"))
            && let Ok(attachments) = serde_json::from_value(raw)
        {
            message.attachments = attachments;
            message.content = value.to_string();
        }
        return;
    }
    if message.role == "assistant"
        && let Ok(mut envelope) = serde_json::from_str::<serde_json::Value>(&message.content)
        && envelope
            .get("tool_calls")
            .is_some_and(serde_json::Value::is_array)
    {
        // Only the model-facing text owned the old media syntax. Never scan
        // signed reasoning, tool arguments, or provider extension fields.
        if let Some(content) = envelope.get("content").and_then(serde_json::Value::as_str) {
            let (text, references) = parse_text(content);
            if !references.is_empty() {
                envelope["content"] = text.into();
                message.content = envelope.to_string();
                message.attachments = references;
            }
        }
        return;
    }
    if !matches!(message.role.as_str(), "user" | "assistant") {
        return;
    }
    if let Some(body) = message.content.strip_prefix("[Tool results]\n") {
        let mut lines = body.split('\n');
        let Some(count) = lines.next().and_then(|header| {
            header
                .strip_prefix("[Tool attachments: ")?
                .strip_suffix(']')?
                .parse::<usize>()
                .ok()
        }) else {
            return;
        };
        let mut references = Vec::new();
        for _ in 0..count {
            let Some(reference) = lines.next().and_then(parse_line) else {
                return;
            };
            references.push(reference);
        }
        message.attachments = references;
        message.content = format!("[Tool results]\n{}", lines.collect::<Vec<_>>().join("\n"));
        return;
    }
    let (text, references) = parse_text(&message.content);
    message.content = text;
    message.attachments = references;
}

fn kind(name: &str) -> Option<AttachmentKind> {
    match name.to_ascii_uppercase().as_str() {
        "IMAGE" | "PHOTO" | "IMG" => Some(AttachmentKind::Image),
        "AUDIO" | "VOICE" => Some(AttachmentKind::Audio),
        "VIDEO" => Some(AttachmentKind::Video),
        "DOCUMENT" | "FILE" | "DOC" => Some(AttachmentKind::Document),
        _ => None,
    }
}

fn parse_line(line: &str) -> Option<AttachmentReference> {
    let body = line.strip_prefix('[')?.strip_suffix(']')?;
    let (name, target) = body.split_once(':')?;
    let kind = kind(name)?;
    // The old loader accepted line-wrapped references. Normalize that syntax
    // once during import so live readers only receive the structured target.
    let mut normalized = String::with_capacity(target.len());
    let mut after_break = false;
    for ch in target.chars() {
        if matches!(ch, '\r' | '\n') {
            after_break = true;
        } else if !after_break || !ch.is_whitespace() {
            after_break = false;
            normalized.push(ch);
        }
    }
    let target = normalized.trim();
    if target.is_empty() {
        return None;
    }
    Some(AttachmentReference {
        kind,
        target: target.to_string(),
    })
}

fn parse_text(text: &str) -> (String, Vec<AttachmentReference>) {
    let mut output = String::new();
    let mut references = Vec::new();
    let mut cursor = 0;
    while let Some(offset) = text[cursor..].find('[') {
        let start = cursor + offset;
        let Some(end) = text[start..].find(']').map(|offset| start + offset + 1) else {
            break;
        };
        output.push_str(&text[cursor..start]);
        let span = &text[start..end];
        if let Some(reference) = parse_line(span).filter(|reference| {
            let target = reference.target.as_str();
            target.starts_with('/')
                || target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("data:")
                || target.starts_with(r"\\")
                || target.as_bytes().get(1) == Some(&b':')
        }) {
            references.push(reference);
        } else if let Some(name) = span
            .strip_prefix("[Document: ")
            .and_then(|value| value.strip_suffix(']'))
        {
            let tail = &text[end..];
            let leading = tail.len() - tail.trim_start_matches([' ', '\t']).len();
            let path_start = end + leading;
            let path_end = text[path_start..]
                .find('\n')
                .map_or(text.len(), |n| path_start + n);
            let path = text[path_start..path_end].trim_end();
            if path.starts_with('/') || path.starts_with("http://") || path.starts_with("https://")
            {
                references.push(AttachmentReference {
                    kind: AttachmentKind::Document,
                    target: path.to_string(),
                });
                output.push_str(name);
                cursor = path_end;
                continue;
            }
            output.push_str(span);
        } else {
            output.push_str(span);
        }
        cursor = end;
    }
    output.push_str(&text[cursor..]);
    (output, references)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn converts_legacy_media_without_loading_sources() {
        let mut message = ChatMessage::user("before [IMAGE:/missing/file.png] after");
        upgrade_chat(&mut message);
        assert_eq!(message.content, "before  after");
        assert_eq!(
            message.attachments,
            vec![AttachmentReference {
                kind: AttachmentKind::Image,
                target: "/missing/file.png".into()
            }]
        );
    }

    #[test]
    fn converts_only_assistant_content_without_touching_signed_or_tool_data() {
        let reasoning = r#"{"thinking":"literal [IMAGE:/reasoning]","signature":"opaque"}"#;
        let envelope = serde_json::json!({
            "content":"result [IMAGE:/real.png]",
            "reasoning_content":reasoning,
            "tool_calls":[{"id":"a", "name":"shell", "arguments":"[IMAGE:/argument]"}],
        });
        let mut message = ChatMessage::assistant(envelope.to_string());
        upgrade_chat(&mut message);
        let upgraded: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        assert_eq!(upgraded["reasoning_content"], reasoning);
        assert_eq!(upgraded["tool_calls"], envelope["tool_calls"]);
        assert_eq!(upgraded["content"], "result ");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].target, "/real.png");
        let mut system = ChatMessage::system("literal [IMAGE:/example]");
        upgrade_chat(&mut system);
        assert_eq!(system.content, "literal [IMAGE:/example]");
        assert!(system.attachments.is_empty());
    }

    #[test]
    fn legacy_tool_envelopes_become_typed_fields_without_changing_owned_text() {
        let reasoning = r#"{"thinking":"[IMAGE:/reason]","signature":"opaque"}"#;
        let arguments = r#"{"text":"[IMAGE:/argument]"}"#;
        let mut assistant = ConversationMessage::Chat(ChatMessage::assistant(
            serde_json::json!({
                "content": "checking",
                "tool_calls": [{"id":"call-1","name":"shell","arguments":arguments}],
                "reasoning_content": reasoning,
            })
            .to_string(),
        ));
        upgrade_conversation(&mut assistant, 0);
        let ConversationMessage::AssistantToolCalls {
            text,
            tool_calls,
            reasoning_content,
        } = assistant
        else {
            panic!("expected typed assistant calls");
        };
        assert_eq!(text.as_deref(), Some("checking"));
        assert_eq!(tool_calls[0].arguments, arguments);
        assert_eq!(reasoning_content.as_deref(), Some(reasoning));

        let body = r#"{"tool_call_id":"literal","text":"[IMAGE:/private]"}"#;
        let mut result = ConversationMessage::Chat(ChatMessage {
            role: "tool".into(),
            content: serde_json::json!({
                "tool_call_id":"call-1","name":"shell","success":false,"content":body,
            })
            .to_string(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
        });
        upgrade_conversation(&mut result, 1);
        let ConversationMessage::ToolResults(results) = result else {
            panic!("expected typed tool result");
        };
        assert_eq!(results[0].tool_call_id, "call-1");
        assert_eq!(results[0].content, body);
        assert_eq!(results[0].success, Some(false));
        assert!(results[0].attachments.is_empty());
    }
    #[test]
    fn tool_body_never_grants_an_attachment() {
        let mut message = ChatMessage {
            role: "tool".into(),
            content: r#"{"tool_call_id":"t","content":"[IMAGE:/private]"}"#.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_result: None,
            reasoning_content: None,
        };
        let original = message.content.clone();
        upgrade_chat(&mut message);
        assert_eq!(message.content, original);
        assert!(message.attachments.is_empty());
    }
    #[test]
    fn prompt_declaration_does_not_scan_the_body() {
        let mut message = ChatMessage::user(
            "[Tool results]\n[Tool attachments: 1]\n[IMAGE:/real]\n[IMAGE:/example]",
        );
        upgrade_chat(&mut message);
        assert_eq!(message.content, "[Tool results]\n[IMAGE:/example]");
        assert_eq!(message.attachments.len(), 1);
        assert_eq!(message.attachments[0].target, "/real");
    }

    #[test]
    fn prompt_result_upgrade_keeps_the_whole_batch_without_inventing_calls() {
        let body = "<tool_result name=\"first\">a</tool_result>\n<tool_result name=\"second\">b [IMAGE:/literal]</tool_result>";
        let mut message = ConversationMessage::Chat(ChatMessage::user(format!(
            "[Tool results]\n[Tool attachments: 1]\n[IMAGE:/real]\n{body}"
        )));
        upgrade_conversation(&mut message, 7);
        let ConversationMessage::ToolResults(results) = message else {
            panic!("tool results expected");
        };
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tool_call_id, "legacy-tool-7");
        assert_eq!(results[0].content, body);
        assert_eq!(results[0].success, None);
        assert_eq!(results[0].attachments[0].target, "/real");
        assert!(results[0].tool_name.is_empty());
    }
    #[test]
    fn unknown_incomplete_and_prose_markers_survive() {
        for text in ["[UNKNOWN:/a]", "[IMAGE:example]", "[IMAGE:/missing"] {
            let mut message = ChatMessage::user(text);
            upgrade_chat(&mut message);
            assert_eq!(message.content, text);
            assert!(message.attachments.is_empty());
        }
    }
    #[test]
    fn converts_wrapped_references_without_removing_spaces_within_paths() {
        let mut message = ChatMessage::user(
            "look [IMAGE:/tmp/my photo/\n  picture.png] and [IMAGE:data:image/png;base64,\r\n  AQID]",
        );
        upgrade_chat(&mut message);
        assert_eq!(message.content, "look  and ");
        assert_eq!(message.attachments[0].target, "/tmp/my photo/picture.png");
        assert_eq!(message.attachments[1].target, "data:image/png;base64,AQID");
    }
    #[test]
    fn legacy_document_path_preserves_spaces_and_following_lines() {
        let mut message = ChatMessage::user("[Document: report.pdf] /tmp/a b.pdf\nnext line");
        upgrade_chat(&mut message);
        assert_eq!(message.content, "report.pdf\nnext line");
        assert_eq!(message.attachments[0].target, "/tmp/a b.pdf");
        assert_eq!(message.attachments[0].kind, AttachmentKind::Document);
    }
}
