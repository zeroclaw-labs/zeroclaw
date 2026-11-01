use crate::tools::{Tool, ToolSpec};
use serde_json::Value;
use std::fmt::Write;
use zeroclaw_api::media::RenderedMarker;
use zeroclaw_api::tool_carrier::{render_native_attachments, render_prompt_tool_carrier};
use zeroclaw_providers::{ChatMessage, ChatResponse, ConversationMessage, ToolResultMessage};

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: Value,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionResult {
    pub name: String,
    pub output: String,
    pub success: bool,
    pub tool_call_id: Option<String>,
}

pub trait ToolDispatcher: Send + Sync {
    fn parse_response(&self, response: &ChatResponse) -> (String, Vec<ParsedToolCall>);
    fn format_results(&self, results: &[ToolExecutionResult]) -> ConversationMessage;
    fn prompt_instructions(&self, tools: &[Box<dyn Tool>]) -> String;
    fn to_provider_messages(&self, history: &[ConversationMessage]) -> Vec<ChatMessage>;
    fn should_send_tool_specs(&self) -> bool;
}

#[derive(Default)]
pub struct XmlToolDispatcher;

impl XmlToolDispatcher {
    fn parse_xml_tool_calls(response: &str) -> (String, Vec<ParsedToolCall>) {
        // Strip `<think>...</think>` blocks before parsing tool calls.
        // Qwen and other reasoning models may embed chain-of-thought inline.
        let cleaned = zeroclaw_tool_call_parser::strip_think_tags(response);
        let mut text_parts = Vec::new();
        let mut calls = Vec::new();
        let mut remaining = cleaned.as_str();

        while let Some(start) = remaining.find("<tool_call>") {
            let before = &remaining[..start];
            if !before.trim().is_empty() {
                text_parts.push(before.trim().to_string());
            }

            if let Some(end) = remaining[start..].find("</tool_call>") {
                let inner = &remaining[start + 11..start + end];
                match serde_json::from_str::<Value>(inner.trim()) {
                    Ok(parsed) => {
                        let name = parsed
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        if name.is_empty() {
                            remaining = &remaining[start + end + 12..];
                            continue;
                        }
                        let arguments = parsed
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
                        calls.push(ParsedToolCall {
                            name,
                            arguments,
                            tool_call_id: None,
                        });
                    }
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            )
                            .with_category(::zeroclaw_log::EventCategory::Agent)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(::serde_json::json!({"error": format!("{}", e)})),
                            "Malformed <tool_call> JSON"
                        );
                    }
                }
                remaining = &remaining[start + end + 12..];
            } else {
                break;
            }
        }

        if !remaining.trim().is_empty() {
            text_parts.push(remaining.trim().to_string());
        }

        (text_parts.join("\n"), calls)
    }

    pub fn tool_specs(tools: &[Box<dyn Tool>]) -> Vec<ToolSpec> {
        tools.iter().map(|tool| tool.spec()).collect()
    }
}

impl ToolDispatcher for XmlToolDispatcher {
    fn parse_response(&self, response: &ChatResponse) -> (String, Vec<ParsedToolCall>) {
        let text = response.text_or_empty();
        Self::parse_xml_tool_calls(text)
    }

    fn format_results(&self, results: &[ToolExecutionResult]) -> ConversationMessage {
        let mut content = String::new();
        // Attachments are declared where the tool ran, not here: this typed
        // replay path carries no declarations, so the carrier declares zero.
        let attachments: Vec<RenderedMarker> = Vec::new();
        for result in results {
            let status = if result.success { "ok" } else { "error" };
            let _ = writeln!(
                content,
                "<tool_result name=\"{}\" status=\"{}\">\n{}\n</tool_result>",
                result.name, status, result.output
            );
        }
        ConversationMessage::Chat(ChatMessage::user(render_prompt_tool_carrier(
            &content,
            &attachments,
        )))
    }

    fn prompt_instructions(&self, tools: &[Box<dyn Tool>]) -> String {
        if tools.is_empty() {
            return String::new();
        }

        // The tool-call formatting guidance has one home: `tool_call_format`.
        // Do not re-type it here — this builder and `loop_`'s had already
        // drifted once (this one was missing the `CRITICAL:` line and the
        // worked example), which made tool-use behavior depend on which
        // builder produced the prompt.
        //
        // The tool listing itself is deliberately NOT emitted here: for this
        // path `ToolsSection` in `agent::prompt` renders it, and duplicating
        // it caused double schema injection (see the dispatcher tests).
        super::tool_call_format::TOOL_CALL_PROTOCOL_INSTRUCTIONS.to_string()
    }

    fn to_provider_messages(&self, history: &[ConversationMessage]) -> Vec<ChatMessage> {
        history
            .iter()
            .flat_map(|msg| match msg {
                ConversationMessage::Chat(chat) => vec![chat.clone()],
                ConversationMessage::AssistantToolCalls { text, .. } => {
                    vec![ChatMessage::assistant(text.clone().unwrap_or_default())]
                }
                ConversationMessage::ToolResults(results) => {
                    let mut content = String::new();
                    // Typed replay carries no declarations; a bare image path
                    // in stored tool text stays text under the
                    // attachment-identity boundary: nothing in tool text is
                    // promoted unless the tool declared it.
                    let attachments: Vec<RenderedMarker> = Vec::new();
                    for result in results {
                        let _ = writeln!(
                            content,
                            "<tool_result id=\"{}\">\n{}\n</tool_result>",
                            result.tool_call_id, result.content
                        );
                    }
                    vec![ChatMessage::user(render_prompt_tool_carrier(
                        &content,
                        &attachments,
                    ))]
                }
            })
            .collect()
    }

    fn should_send_tool_specs(&self) -> bool {
        false
    }
}

pub struct NativeToolDispatcher;

impl ToolDispatcher for NativeToolDispatcher {
    fn parse_response(&self, response: &ChatResponse) -> (String, Vec<ParsedToolCall>) {
        let text = response.text.clone().unwrap_or_default();
        let calls = response
            .tool_calls
            .iter()
            .map(|tc| ParsedToolCall {
                name: tc.name.clone(),
                arguments: serde_json::from_str(&tc.arguments).unwrap_or_else(|e| {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_category(::zeroclaw_log::EventCategory::Tool).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"tool": tc.name, "error": format!("{}", e)})), "Failed to parse native tool call arguments as JSON; defaulting to empty object");
                    Value::Object(serde_json::Map::new())
                }),
                tool_call_id: Some(tc.id.clone()),
            })
            .collect();
        (text, calls)
    }

    fn format_results(&self, results: &[ToolExecutionResult]) -> ConversationMessage {
        let messages = results
            .iter()
            .map(|result| ToolResultMessage {
                tool_call_id: result
                    .tool_call_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                // Retain the producing tool name as replay metadata. It is
                // no longer read for promotion: attachments come only from
                // the producing tool's own declarations.
                tool_name: result.name.clone(),
                // The verbatim output. The body is never rewritten into
                // marker syntax, and this typed shape carries no attachment
                // declarations (a known, disclosed replay limitation).
                content: result.output.clone(),
            })
            .collect();
        ConversationMessage::ToolResults(messages)
    }

    fn prompt_instructions(&self, _tools: &[Box<dyn Tool>]) -> String {
        String::new()
    }

    fn to_provider_messages(&self, history: &[ConversationMessage]) -> Vec<ChatMessage> {
        history
            .iter()
            .flat_map(|msg| match msg {
                ConversationMessage::Chat(chat) => vec![chat.clone()],
                ConversationMessage::AssistantToolCalls {
                    text,
                    tool_calls,
                    reasoning_content,
                } => {
                    let mut payload = serde_json::json!({
                        "content": text,
                        "tool_calls": tool_calls,
                    });
                    if let Some(rc) = reasoning_content {
                        payload["reasoning_content"] = serde_json::json!(rc);
                    }
                    vec![ChatMessage::assistant(payload.to_string())]
                }
                ConversationMessage::ToolResults(results) => results
                    .iter()
                    .map(|result| {
                        // Typed replay writes the declared shape (array key
                        // always present) with no inferred attachments: a
                        // bare image path in stored tool text stays text.
                        let attachments: Vec<RenderedMarker> = Vec::new();
                        ChatMessage::tool(
                            serde_json::json!({
                                "tool_call_id": result.tool_call_id,
                                "content": result.content,
                                "attachments": render_native_attachments(&attachments),
                            })
                            .to_string(),
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    fn should_send_tool_specs(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_dispatcher_parses_tool_calls() {
        let response = ChatResponse {
            text: Some(
                "Checking\n<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call>"
                    .into(),
            ),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        let dispatcher = XmlToolDispatcher;
        let (_, calls) = dispatcher.parse_response(&response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn xml_dispatcher_strips_think_before_tool_call() {
        let response = ChatResponse {
            text: Some(
                "<think>I should list files</think>\n<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call>"
                    .into(),
            ),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        let dispatcher = XmlToolDispatcher;
        let (text, calls) = dispatcher.parse_response(&response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(
            !text.contains("<think>"),
            "think tags should be stripped from text"
        );
    }

    #[test]
    fn xml_dispatcher_think_only_returns_no_calls() {
        let response = ChatResponse {
            text: Some("<think>Just thinking</think>".into()),
            tool_calls: vec![],
            usage: None,
            reasoning_content: None,
        };
        let dispatcher = XmlToolDispatcher;
        let (_, calls) = dispatcher.parse_response(&response);
        assert!(calls.is_empty());
    }

    #[test]
    fn native_dispatcher_roundtrip() {
        let response = ChatResponse {
            text: Some("ok".into()),
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "tc1".into(),
                name: "file_read".into(),
                arguments: "{\"path\":\"a.txt\"}".into(),
                extra_content: None,
            }],
            usage: None,
            reasoning_content: None,
        };
        let dispatcher = NativeToolDispatcher;
        let (_, calls) = dispatcher.parse_response(&response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("tc1"));

        let msg = dispatcher.format_results(&[ToolExecutionResult {
            name: "file_read".into(),
            output: "hello".into(),
            success: true,
            tool_call_id: Some("tc1".into()),
        }]);
        match msg {
            ConversationMessage::ToolResults(results) => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].tool_call_id, "tc1");
            }
            _ => panic!("expected tool results"),
        }
    }

    #[test]
    fn xml_format_results_contains_tool_result_tags() {
        let dispatcher = XmlToolDispatcher;
        let msg = dispatcher.format_results(&[ToolExecutionResult {
            name: "shell".into(),
            output: "ok".into(),
            success: true,
            tool_call_id: None,
        }]);
        let rendered = match msg {
            ConversationMessage::Chat(chat) => chat.content,
            _ => String::new(),
        };
        assert!(rendered.contains("<tool_result"));
        assert!(rendered.contains("shell"));
    }

    #[test]
    fn native_format_results_keeps_tool_call_id() {
        let dispatcher = NativeToolDispatcher;
        let msg = dispatcher.format_results(&[ToolExecutionResult {
            name: "shell".into(),
            output: "ok".into(),
            success: true,
            tool_call_id: Some("tc-1".into()),
        }]);

        match msg {
            ConversationMessage::ToolResults(results) => {
                assert_eq!(results.len(), 1);
                assert_eq!(results[0].tool_call_id, "tc-1");
            }
            _ => panic!("expected ToolResults variant"),
        }
    }

    /// Write a throwaway PNG and return its absolute path string. An existing
    /// local image path is required for canonicalization to fire at all.
    fn write_temp_image(dir: &std::path::Path, name: &str) -> String {
        let image = dir.join(name);
        std::fs::write(&image, [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']).unwrap();
        image.display().to_string()
    }

    fn xml_format_results_text(
        dispatcher: &XmlToolDispatcher,
        result: ToolExecutionResult,
    ) -> String {
        match dispatcher.format_results(&[result]) {
            ConversationMessage::Chat(chat) => chat.content,
            _ => panic!("XmlToolDispatcher::format_results must return a Chat message"),
        }
    }

    fn native_format_results_content(
        dispatcher: &NativeToolDispatcher,
        result: ToolExecutionResult,
    ) -> String {
        match dispatcher.format_results(&[result]) {
            ConversationMessage::ToolResults(results) => results[0].content.clone(),
            _ => panic!("NativeToolDispatcher::format_results must return ToolResults"),
        }
    }

    #[test]
    fn xml_format_results_does_not_promote_search_tool_image_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "hit.png");
        let xml = XmlToolDispatcher;

        for tool in ["content_search", "glob_search"] {
            let rendered = xml_format_results_text(
                &xml,
                ToolExecutionResult {
                    name: tool.into(),
                    output: format!("match: {path}"),
                    success: true,
                    tool_call_id: None,
                },
            );
            assert!(
                !rendered.contains("[IMAGE:"),
                "{tool} output must not be promoted to an image marker"
            );
            assert!(
                rendered.contains(&path),
                "{tool} output must still carry the literal path text"
            );
        }
    }

    #[test]
    fn native_format_results_does_not_promote_search_tool_image_paths() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "hit.png");
        let native = NativeToolDispatcher;

        for tool in ["content_search", "glob_search"] {
            let content = native_format_results_content(
                &native,
                ToolExecutionResult {
                    name: tool.into(),
                    output: format!("found: {path}"),
                    success: true,
                    tool_call_id: Some("tc1".into()),
                },
            );
            assert!(
                !content.contains("[IMAGE:"),
                "{tool} output must not be promoted to an image marker"
            );
            assert!(content.contains(&path));
        }
    }

    #[test]
    fn format_results_does_not_promote_image_producing_tool_paths() {
        // The attachment-identity boundary applies to every tool: even a
        // genuinely image-producing tool's bare path in text is text. The
        // attachment must come from the tool's own declaration, never from a
        // read-side scan of the text.
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "generated.png");

        let xml = XmlToolDispatcher;
        let rendered = xml_format_results_text(
            &xml,
            ToolExecutionResult {
                name: "image_gen".into(),
                output: format!("saved to {path}"),
                success: true,
                tool_call_id: None,
            },
        );
        assert!(
            !rendered.contains("[IMAGE:"),
            "a bare image path must not be promoted into a marker (XML): {rendered}"
        );
        assert!(
            rendered.contains(&path),
            "the literal path text must survive (XML)"
        );
        // The prompt carrier declares zero attachments for this round.
        assert!(rendered.contains("[Tool attachments: 0]"));

        let native = NativeToolDispatcher;
        let content = native_format_results_content(
            &native,
            ToolExecutionResult {
                name: "image_gen".into(),
                output: format!("saved to {path}"),
                success: true,
                tool_call_id: Some("tc1".into()),
            },
        );
        // The stored body stays verbatim; no attachment is inferred.
        assert_eq!(
            content,
            format!("saved to {path}"),
            "native format_results stores the verbatim output"
        );
    }

    fn native_tool_message_content(message: &ChatMessage) -> String {
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        payload["content"].as_str().unwrap().to_owned()
    }

    fn native_tool_message_attachments(message: &ChatMessage) -> serde_json::Value {
        let payload: serde_json::Value = serde_json::from_str(&message.content).unwrap();
        payload["attachments"].clone()
    }

    fn native_round_trip_tool_content(
        dispatcher: &NativeToolDispatcher,
        result: ToolExecutionResult,
    ) -> String {
        let stored = dispatcher.format_results(&[result]);
        let messages = dispatcher.to_provider_messages(&[stored]);
        assert_eq!(messages.len(), 1, "one tool result -> one provider message");
        assert_eq!(messages[0].role, "tool");
        native_tool_message_content(&messages[0])
    }

    #[test]
    fn native_search_path_survives_format_then_to_provider_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "hit.png");
        let native = NativeToolDispatcher;

        for tool in ["content_search", "glob_search"] {
            let rendered = native_round_trip_tool_content(
                &native,
                ToolExecutionResult {
                    name: tool.into(),
                    output: format!("found: {path}"),
                    success: true,
                    tool_call_id: Some("tc1".into()),
                },
            );
            assert!(
                !rendered.contains("[IMAGE:"),
                "{tool} path must not be re-promoted on the read side"
            );
            assert!(
                rendered.contains(&path),
                "{tool} provider-visible content must keep the literal path"
            );
        }
    }

    #[test]
    fn native_image_gen_bare_path_yields_no_attachments() {
        // The round trip keeps the boundary: typed replay declares no
        // attachments, so a real generated image's bare path in stored text
        // yields an empty array and a verbatim body.
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "generated.png");
        let native = NativeToolDispatcher;

        let stored = native.format_results(&[ToolExecutionResult {
            name: "image_gen".into(),
            output: format!("saved to {path}"),
            success: true,
            tool_call_id: Some("tc1".into()),
        }]);
        let messages = native.to_provider_messages(&[stored]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            native_tool_message_content(&messages[0]),
            format!("saved to {path}")
        );
        assert_eq!(
            native_tool_message_attachments(&messages[0]),
            serde_json::json!([]),
            "no attachment may be inferred from a bare path in replayed text"
        );
    }

    #[test]
    fn native_unknown_provenance_bare_path_yields_no_attachments() {
        // A tool result stored WITHOUT provenance (empty `tool_name`, e.g.
        // reconstructed from a provider-wire message) still yields zero
        // attachments: provenance was never a promotion license, and the
        // stored text carries no declarations to read.
        let dir = tempfile::tempdir().unwrap();
        let path = write_temp_image(dir.path(), "history.png");
        let native = NativeToolDispatcher;

        let history = vec![ConversationMessage::ToolResults(vec![ToolResultMessage {
            tool_call_id: "tc1".into(),
            content: format!("Saved image to {path}"),
            tool_name: String::new(),
        }])];
        let messages = native.to_provider_messages(&history);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            native_tool_message_content(&messages[0]),
            format!("Saved image to {path}"),
            "the body must stay verbatim"
        );
        assert_eq!(
            native_tool_message_attachments(&messages[0]),
            serde_json::json!([]),
            "unknown-provenance text must not be promoted on the read side"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn native_to_provider_messages_includes_reasoning_content() {
        let dispatcher = NativeToolDispatcher;
        let history = vec![ConversationMessage::AssistantToolCalls {
            text: Some("answer".into()),
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "tc_1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: Some("thinking step".into()),
        }];

        let messages = dispatcher.to_provider_messages(&history);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");

        let payload: serde_json::Value = serde_json::from_str(&messages[0].content).unwrap();
        assert_eq!(payload["reasoning_content"].as_str(), Some("thinking step"));
        assert_eq!(payload["content"].as_str(), Some("answer"));
        assert!(payload["tool_calls"].is_array());
    }

    #[test]
    fn native_to_provider_messages_omits_reasoning_content_when_none() {
        let dispatcher = NativeToolDispatcher;
        let history = vec![ConversationMessage::AssistantToolCalls {
            text: Some("answer".into()),
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "tc_1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: None,
        }];

        let messages = dispatcher.to_provider_messages(&history);
        assert_eq!(messages.len(), 1);

        let payload: serde_json::Value = serde_json::from_str(&messages[0].content).unwrap();
        assert!(payload.get("reasoning_content").is_none());
    }

    #[test]
    fn xml_to_provider_messages_ignores_reasoning_content() {
        let dispatcher = XmlToolDispatcher;
        let history = vec![ConversationMessage::AssistantToolCalls {
            text: Some("answer".into()),
            tool_calls: vec![zeroclaw_providers::ToolCall {
                id: "tc_1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
                extra_content: None,
            }],
            reasoning_content: Some("should be ignored".into()),
        }];

        let messages = dispatcher.to_provider_messages(&history);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "assistant");
        // XmlToolDispatcher returns text only, not JSON payload
        assert_eq!(messages[0].content, "answer");
        assert!(!messages[0].content.contains("reasoning_content"));
    }
}
