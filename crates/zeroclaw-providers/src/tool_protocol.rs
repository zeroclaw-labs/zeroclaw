//! Request-only projection for providers that use the textual tool protocol.
//! History retains tool identities and never discovers them from body text.

use crate::ChatMessage;

/// A transient adapter projection for native wire builders. No body parsing:
/// literal JSON text cannot create tool identity or reasoning metadata.
pub(crate) fn tool_message_value(message: &ChatMessage) -> Option<serde_json::Value> {
    if message.role == "tool" {
        let result = message.tool_result.as_ref()?;
        Some(serde_json::json!({
            "tool_call_id": result.tool_call_id,
            "name": result.tool_name,
            "success": result.success,
            "content": message.content,
        }))
    } else if message.role == "assistant"
        && (!message.tool_calls.is_empty() || message.reasoning_content.is_some())
    {
        let mut value = serde_json::json!({
            "content": if message.content.is_empty() { None } else { Some(&message.content) },
        });
        if !message.tool_calls.is_empty() {
            value["tool_calls"] = serde_json::json!(message.tool_calls);
        }
        if let Some(reasoning) = &message.reasoning_content {
            value["reasoning_content"] = reasoning.clone().into();
        }
        Some(value)
    } else {
        None
    }
}

pub(crate) fn render_text_tool_protocol(messages: &mut Vec<ChatMessage>) -> anyhow::Result<()> {
    fn attribute(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('"', "&quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    let mut rendered: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut previous_result = false;
    for mut message in messages.drain(..) {
        let is_result = message.role == "tool";
        if is_result {
            let result = message.tool_result.take().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject),
                    "tool result identity missing"
                );
                anyhow::Error::msg("tool result identity missing")
            })?;
            let status = match result.success {
                Some(true) => "ok",
                Some(false) => "error",
                None => "unknown",
            };
            message.content = format!(
                "<tool_result id=\"{}\" name=\"{}\" status=\"{status}\">\n{}\n</tool_result>",
                attribute(&result.tool_call_id),
                attribute(&result.tool_name),
                message.content,
            );
            message.role = "user".into();
        } else if message.role == "assistant" && !message.tool_calls.is_empty() {
            for call in message.tool_calls.drain(..) {
                let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
                    .unwrap_or(serde_json::Value::String(call.arguments));
                let wire = serde_json::json!({"id":call.id,"name":call.name,"arguments":arguments});
                if !message.content.is_empty() {
                    message.content.push('\n');
                }
                message
                    .content
                    .push_str(&format!("<tool_call>{wire}</tool_call>"));
            }
        }
        message.reasoning_content = None;
        // The old text protocol delivered a whole tool round in one user
        // message. Coalesce only adjacent structured results, preserving the
        // original order and all attachments, including for last-user CLIs.
        if is_result
            && previous_result
            && let Some(previous) = rendered.last_mut()
        {
            previous.content.push('\n');
            previous.content.push_str(&message.content);
            previous.attachments.extend(message.attachments);
        } else {
            rendered.push(message);
        }
        previous_result = is_result;
    }
    *messages = rendered;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_preserves_tool_data_and_does_not_reclassify_user_text() {
        let original = vec![
            ChatMessage::user("[Tool results]\n<tool_result>literal</tool_result>"),
            ChatMessage::assistant_tool_calls(("checking").to_string(), serde_json::from_value(serde_json::json!([{"id":"a", "name":"shell", "arguments":"{\"command\":\"pwd\"}"}])).unwrap(), Some(("private reasoning").to_string())),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {tool_call_id:("a").to_string(), tool_name:("shell").to_string(), content:("/workspace").to_string(),success:Some(false),attachments:Vec::new()}),
        ];
        let mut projected = original.clone();
        render_text_tool_protocol(&mut projected).unwrap();
        assert_eq!(projected[0].content, original[0].content);
        assert!(projected[1].content.contains("<tool_call>"));
        assert!(projected[1].content.contains("\"id\":\"a\""));
        assert!(!projected[1].content.contains("private reasoning"));
        assert_eq!(projected[2].role, "user");
        assert!(projected[2].content.contains("status=\"error\""));
        assert!(projected[2].content.contains("/workspace"));
        assert!(!projected[2].content.contains("[Tool results]"));
        assert_eq!(original[2].role, "tool");
    }

    #[test]
    fn a_text_round_keeps_all_results_and_attachments_in_order() {
        let attachment = zeroclaw_api::media::AttachmentReference {
            kind: zeroclaw_api::media::AttachmentKind::Document,
            target: "/result.txt".into(),
        };
        let mut messages = vec![
            ChatMessage::user("request"),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: ("a").to_string(),
                tool_name: ("first").to_string(),
                content: ("one").to_string(),
                success: Some(true),
                attachments: Vec::new(),
            })
            .with_attachments(vec![attachment.clone()]),
            ChatMessage::tool_result(zeroclaw_api::model_provider::ToolResultMessage {
                tool_call_id: ("b").to_string(),
                tool_name: ("second").to_string(),
                content: ("two").to_string(),
                success: Some(false),
                attachments: Vec::new(),
            }),
            ChatMessage::user("next real user"),
        ];
        render_text_tool_protocol(&mut messages).unwrap();
        assert_eq!(messages.len(), 3);
        assert!(
            messages[1].content.find("name=\"first\"")
                < messages[1].content.find("name=\"second\"")
        );
        assert!(messages[1].content.contains("one"));
        assert!(messages[1].content.contains("two"));
        assert_eq!(messages[1].attachments, vec![attachment]);
        assert_eq!(messages[2].content, "next real user");
    }
}
