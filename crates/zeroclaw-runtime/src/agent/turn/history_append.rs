//! History append for one tool round: the assistant message plus per-call
//! `role=tool` messages (native) or a `[Tool results]` user message (prompt
//! mode).
//!
//! Attachments ride the carrier grammar from `zeroclaw_api::tool_carrier` at
//! a fixed position the parsers reach without scanning body text: a sibling
//! `attachments` array in each native envelope (always present, `[]` for
//! none) and a `[Tool attachments: N]` count line plus N marker lines in the
//! prompt-mode carrier (N aggregated across the round's tools).

use zeroclaw_api::tool_carrier::{
    marker_line, parse_marker_line, render_native_attachments, render_prompt_tool_carrier,
};
use zeroclaw_providers::{ChatMessage, ToolCall};

use super::results_collect::ToolRoundResult;
use crate::agent::prompt::{SESSION_PROMPT_HISTORY_RESULT_KEY, SESSION_PROMPT_TEXT_RESULT_MARKER};

pub(crate) fn append_tool_round_to_history(
    history: &mut Vec<ChatMessage>,
    assistant_history_content: String,
    native_tool_calls: &[ToolCall],
    individual_results: &[ToolRoundResult],
    tool_results: &str,
    use_native_tools: bool,
) {
    history.push(ChatMessage::assistant(assistant_history_content));
    if native_tool_calls.is_empty() {
        let all_results_have_ids = use_native_tools
            && !individual_results.is_empty()
            && individual_results
                .iter()
                .all(|result| result.tool_call_id.is_some());
        if all_results_have_ids {
            for result in individual_results {
                history.push(native_result_message(
                    result,
                    result.tool_call_id.as_deref(),
                ));
            }
        } else {
            let sensitive = individual_results
                .iter()
                .any(|result| result.sensitive_session_prompt);
            // Keep the attachment declaration at its fixed header position.
            // Sensitivity travels in the body, which media rewrites preserve,
            // so sliced exports remain private without breaking media parsing.
            let marker = if sensitive {
                SESSION_PROMPT_TEXT_RESULT_MARKER
            } else {
                ""
            };
            let round_attachments: Vec<_> = individual_results
                .iter()
                .flat_map(|result| result.attachments.iter())
                // The line-based carrier writes attachment targets verbatim.
                // An LF or invalid marker demotes the whole
                // round to legacy text, hiding its sensitivity marker below.
                // Until the shared grammar can encode these targets, sensitive
                // rounds omit only unrepresentable text-mode attachments, not
                // the tool body. Ordinary rounds retain upstream behavior;
                // native JSON attachments are unaffected. Parsing the unsplit
                // marker alone is insufficient because it accepts embedded LF.
                .filter(|marker| {
                    if !sensitive {
                        return true;
                    }
                    let line = marker_line(marker);
                    !line.contains('\n') && parse_marker_line(&line).as_ref() == Some(*marker)
                })
                .cloned()
                .collect();
            let content =
                render_prompt_tool_carrier(&format!("{marker}{tool_results}"), &round_attachments);
            history.push(ChatMessage::user(content));
        }
    } else {
        // `zip` would drop trailing results on any length divergence,
        // leaving a native tool_use id with no matching tool_result.
        // Pair on each result's own id instead.
        for (idx, result) in individual_results.iter().enumerate() {
            let resolved_id = result
                .tool_call_id
                .as_deref()
                .or_else(|| native_tool_calls.get(idx).map(|call| call.id.as_str()));
            history.push(native_result_message(result, resolved_id));
        }
    }
}

fn native_result_message(result: &ToolRoundResult, tool_call_id: Option<&str>) -> ChatMessage {
    let mut envelope = serde_json::json!({
        "tool_call_id": tool_call_id,
        "content": result.output,
        "attachments": render_native_attachments(&result.attachments),
    });
    if result.sensitive_session_prompt {
        envelope[SESSION_PROMPT_HISTORY_RESULT_KEY] = serde_json::json!(true);
    }
    ChatMessage::tool(envelope.to_string())
}

#[cfg(test)]
mod tests {
    use super::ToolRoundResult;
    use super::append_tool_round_to_history;
    use crate::agent::prompt::redact_session_prompt_tool_exchanges_for_export;
    use zeroclaw_api::media::{MarkerKind, RenderedMarker};
    use zeroclaw_providers::{ChatMessage, ToolCall};

    #[test]
    fn json_tool_calls_text_fallback_result_is_redacted_only_in_export_copies() {
        let marker = "session-prompt-private-marker";
        let assistant =
            format!(r#"{{"tool_calls":[{{"name":"session_prompt_list","arguments":{{}}}}]}}"#);
        let result = format!("<tool_result name=\"session_prompt_list\">{marker}</tool_result>");
        let mut history: Vec<ChatMessage> = Vec::new();

        append_tool_round_to_history(
            &mut history,
            assistant,
            &[],
            &[ToolRoundResult {
                tool_call_id: None,
                output: result.clone(),
                sensitive_session_prompt: true,
                attachments: Vec::new(),
            }],
            &result,
            false,
        );

        assert!(
            history[1].content.contains(marker),
            "the provider's working history keeps the explicit list result"
        );
        let exported = redact_session_prompt_tool_exchanges_for_export(&history);
        assert!(
            exported
                .iter()
                .all(|message| !message.content.contains(marker)),
            "generic exports must not retain the opaque attachment body"
        );
    }

    #[test]
    fn session_prompt_executed_identity_redaction_preserves_ordinary_results_and_native_ids() {
        let calls = [zeroclaw_providers::ToolCall {
            id: "fallback-id".into(),
            name: "ordinary_tool".into(),
            arguments: "{}".into(),
            extra_content: None,
        }];
        let results = [
            ToolRoundResult {
                tool_call_id: None,
                output: "synthetic-private-prompt".into(),
                sensitive_session_prompt: true,
                attachments: Vec::new(),
            },
            ToolRoundResult {
                tool_call_id: Some("ordinary-id".into()),
                output: "ordinary-output".into(),
                sensitive_session_prompt: false,
                attachments: Vec::new(),
            },
        ];
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            r#"{"tool_calls":[{"name":"ordinary_tool","arguments":{}}]}"#.into(),
            &calls,
            &results,
            "",
            true,
        );
        for (message, id, body) in [
            (&history[1], "fallback-id", "synthetic-private-prompt"),
            (&history[2], "ordinary-id", "ordinary-output"),
        ] {
            let value: serde_json::Value = serde_json::from_str(&message.content).unwrap();
            assert_eq!(value["tool_call_id"], id);
            assert_eq!(value["content"], body);
        }
        let exported = redact_session_prompt_tool_exchanges_for_export(&history);
        assert!(exported[1].content.contains("omitted from export"));
        assert_eq!(exported[2].role, history[2].role);
        assert_eq!(exported[2].content, history[2].content);

        let mut text_history = Vec::new();
        append_tool_round_to_history(
            &mut text_history,
            "ordinary assistant call".into(),
            &[],
            &results[1..],
            "ordinary-output",
            false,
        );
        assert_eq!(
            text_history[1].content,
            "[Tool results]\n[Tool attachments: 0]\nordinary-output"
        );
        let exported_text = redact_session_prompt_tool_exchanges_for_export(&text_history);
        assert_eq!(exported_text.len(), text_history.len());
        for (exported, original) in exported_text.iter().zip(&text_history) {
            assert_eq!(exported.role, original.role);
            assert_eq!(exported.content, original.content);
        }
    }

    #[test]
    fn session_prompt_text_result_survives_whole_turn_trim_and_slice_export() {
        let marker = "synthetic-trim-private-prompt";
        let mut history = vec![
            ChatMessage::system("host"),
            ChatMessage::user("older turn"),
            ChatMessage::assistant("older answer"),
            ChatMessage::user("current turn"),
        ];
        append_tool_round_to_history(
            &mut history,
            r#"{"tool_calls":[{"name":"ordinary_tool","arguments":{}}]}"#.into(),
            &[],
            &[ToolRoundResult {
                tool_call_id: None,
                output: marker.into(),
                sensitive_session_prompt: true,
                attachments: Vec::new(),
            }],
            marker,
            false,
        );
        let trimmed = crate::agent::history_trim::trim_to_recent_turns(history, 1);
        assert_eq!(trimmed.dropped_turns, 1);
        assert_eq!(trimmed.kept_turns, 1);
        assert_eq!(trimmed.history[1].content, "current turn");
        assert_eq!(trimmed.history.len(), 4);
        let result = trimmed.history.last().unwrap();
        assert!(result.content.starts_with("[Tool results]"));
        assert!(result.content.contains(marker));
        for slice in [trimmed.history.as_slice(), std::slice::from_ref(result)] {
            let exported = redact_session_prompt_tool_exchanges_for_export(slice);
            assert!(exported.iter().all(|row| !row.content.contains(marker)));
        }
        // Durable replay and the owner's user-role trim breadcrumb must not
        // discard or reinterpret the carried sensitivity fact.
        let serialized = serde_json::to_string(&trimmed.history).unwrap();
        let mut restored: Vec<ChatMessage> = serde_json::from_str(&serialized).unwrap();
        restored.insert(1, ChatMessage::user("older history was trimmed"));
        let exported = redact_session_prompt_tool_exchanges_for_export(&restored);
        assert!(exported.iter().all(|row| !row.content.contains(marker)));
        assert_eq!(exported[1].content, "older history was trimmed");
        assert_eq!(exported[2].content, "current turn");
        let persisted = serde_json::to_string(&exported).unwrap();
        let replayed: Vec<ChatMessage> = serde_json::from_str(&persisted).unwrap();
        let replayed =
            crate::agent::history_trim::trim_to_recent_turns_with_crumb(replayed, 1, true);
        assert_eq!(replayed.kept_turns, 1);
        assert!(
            !replayed.trimmed,
            "the hidden result is not a new user turn"
        );
        assert_eq!(replayed.history[2].content, "current turn");
    }

    fn image(target: &str) -> RenderedMarker {
        RenderedMarker {
            target: target.to_string(),
            kind: MarkerKind::Image,
        }
    }

    fn result(id: Option<&str>, output: &str, attachments: Vec<RenderedMarker>) -> ToolRoundResult {
        ToolRoundResult {
            tool_call_id: id.map(str::to_string),
            output: output.to_string(),
            attachments,
            sensitive_session_prompt: false,
        }
    }

    #[test]
    fn native_carrier_writes_attachments_sibling_always_present() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "assistant text".to_string(),
            &[],
            &[result(
                Some("call-1"),
                "File: /tmp/shot.png",
                vec![image("/tmp/shot.png")],
            )],
            "ignored in native mode",
            true,
        );

        assert_eq!(history.len(), 2);
        assert_eq!(history[1].role, "tool");
        let value: serde_json::Value = serde_json::from_str(&history[1].content).unwrap();
        assert_eq!(value["tool_call_id"], "call-1");
        assert_eq!(value["content"], "File: /tmp/shot.png");
        assert_eq!(
            value["attachments"],
            serde_json::json!([{"kind": "image", "target": "/tmp/shot.png"}])
        );
    }

    #[test]
    fn sensitive_carrier_keeps_media_identity_and_redacts_after_rebuild_and_slice() {
        use crate::agent::prompt::redact_session_prompt_history_for_export;
        use zeroclaw_api::tool_carrier::{classify, rebuild_carrier};

        let private = "synthetic-private-prompt-with-media";
        for native in [false, true] {
            let mut sensitive = result(
                native.then_some("prompt-call"),
                private,
                vec![image("/tmp/synthetic-carrier.png")],
            );
            sensitive.sensitive_session_prompt = true;
            let mut history = Vec::new();
            append_tool_round_to_history(
                &mut history,
                "ordinary call rewritten into a session prompt".into(),
                &[],
                &[sensitive],
                private,
                native,
            );
            let message = &mut history[1];
            let mut parts = classify(&message.role, &message.content).unwrap();
            assert!(
                parts.declared,
                "sensitivity must not displace the media header"
            );
            assert_eq!(parts.attachments, vec![image("/tmp/synthetic-carrier.png")]);
            assert!(parts.text.contains(private));

            // Provider media degradation rebuilds carriers with fewer attachments;
            // retained provenance must survive that transformation and slicing.
            parts.text.push_str("\nsynthetic media-load notice");
            message.content = rebuild_carrier(&message.role, &message.content, &parts, &[]);
            assert!(message.content.contains(private));
            for input in [history.as_slice(), &history[1..]] {
                let exported = redact_session_prompt_history_for_export(input, false);
                assert!(exported.iter().all(|row| !row.content.contains(private)));
                assert!(
                    exported
                        .iter()
                        .all(|row| !row.content.contains("synthetic-carrier.png"))
                );
            }
        }
    }

    #[test]
    fn sensitive_text_carrier_filters_unrepresentable_same_round_media() {
        use crate::agent::prompt::redact_session_prompt_history_for_export;
        use zeroclaw_api::tool_carrier::{classify, rebuild_carrier};

        let private = "synthetic-private-prompt-with-malformed-media";
        let body = format!("ordinary media output\n{private}");
        for kind in [
            MarkerKind::Image,
            MarkerKind::Audio,
            MarkerKind::Video,
            MarkerKind::Document,
        ] {
            for invalid_target in ["/tmp/shot\nimage.png", ""] {
                // An internal CR does not split this LF-delimited grammar.
                let valid = RenderedMarker {
                    kind,
                    target: "/tmp/synthetic\rmedia.png".into(),
                };
                let invalid = RenderedMarker {
                    kind,
                    target: invalid_target.into(),
                };
                let media = result(None, "ordinary media output", vec![invalid, valid.clone()]);
                let mut prompt = result(None, private, Vec::new());
                prompt.sensitive_session_prompt = true;
                let mut history = Vec::new();
                append_tool_round_to_history(
                    &mut history,
                    format!("ordinary call rewritten with {private}"),
                    &[],
                    &[media, prompt],
                    &body,
                    false,
                );
                let message = &mut history[1];
                let parts = classify(&message.role, &message.content).unwrap();
                assert!(
                    parts.declared,
                    "invalid media must not damage the carrier header"
                );
                assert_eq!(parts.attachments, vec![valid]);
                assert!(
                    parts.text.contains(private),
                    "the provider retains the body"
                );
                assert!(parts.text.contains("ordinary media output"));
                message.content = rebuild_carrier(&message.role, &message.content, &parts, &[]);
                for input in [history.as_slice(), &history[1..]] {
                    let exported = redact_session_prompt_history_for_export(input, false);
                    assert!(exported.iter().all(|row| !row.content.contains(private)));
                }
            }
        }
    }

    #[test]
    fn ordinary_text_carrier_preserves_upstream_unrepresentable_media_behavior() {
        use zeroclaw_api::tool_carrier::{classify, render_prompt_tool_carrier};

        for invalid_target in ["/tmp/shot\nimage.png", ""] {
            let attachments = vec![image(invalid_target), image("/tmp/synthetic-image.png")];
            let body = "ordinary tool output";
            let mut history = Vec::new();
            append_tool_round_to_history(
                &mut history,
                "ordinary call".into(),
                &[],
                &[result(None, body, attachments.clone())],
                body,
                false,
            );
            // Pin the shared carrier's current fallback, not a broader media
            // fix: this feature's workaround protects only sensitive rounds.
            assert_eq!(
                history[1].content,
                render_prompt_tool_carrier(body, &attachments)
            );
            let parsed = classify(&history[1].role, &history[1].content).unwrap();
            assert!(!parsed.declared);
            assert!(parsed.attachments.is_empty());
        }
    }

    #[test]
    fn native_sensitive_carrier_preserves_newline_attachment_targets() {
        use crate::agent::prompt::redact_session_prompt_history_for_export;
        use zeroclaw_api::tool_carrier::classify;

        let private = "synthetic-native-private-prompt";
        let attachment = image("/tmp/shot\nimage.png");
        let mut prompt = result(Some("prompt-call"), private, vec![attachment.clone()]);
        prompt.sensitive_session_prompt = true;
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "rewritten call".into(),
            &[],
            &[prompt],
            private,
            true,
        );
        let parsed = classify(&history[1].role, &history[1].content).unwrap();
        assert!(parsed.declared);
        assert_eq!(parsed.attachments, vec![attachment]);
        assert_eq!(parsed.text, private);
        for input in [history.as_slice(), &history[1..]] {
            let exported = redact_session_prompt_history_for_export(input, false);
            assert!(exported.iter().all(|row| !row.content.contains(private)));
            assert!(
                exported
                    .iter()
                    .all(|row| !row.content.contains("image.png"))
            );
        }
    }

    #[test]
    fn native_carrier_reports_empty_array_for_text_only_results() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "assistant text".to_string(),
            &[],
            &[result(Some("call-1"), "plain output", Vec::new())],
            "ignored in native mode",
            true,
        );

        let value: serde_json::Value = serde_json::from_str(&history[1].content).unwrap();
        assert_eq!(value["content"], "plain output");
        assert_eq!(
            value["attachments"],
            serde_json::json!([]),
            "the array key is always written so zero is distinguishable from legacy"
        );
    }

    #[test]
    fn prompt_carrier_aggregates_round_attachments_in_one_count_header() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "assistant text".to_string(),
            &[],
            &[
                result(
                    None,
                    "<tool_result name=\"image_info\">\nFile: /tmp/a.png\n</tool_result>",
                    vec![image("/tmp/a.png")],
                ),
                result(
                    None,
                    "<tool_result name=\"shell\">\nls\n</tool_result>",
                    Vec::new(),
                ),
            ],
            "<tool_result name=\"image_info\">\nFile: /tmp/a.png\n</tool_result>\n<tool_result name=\"shell\">\nls\n</tool_result>\n",
            false,
        );

        assert_eq!(history.len(), 2);
        assert_eq!(history[1].role, "user");
        let content = &history[1].content;
        assert!(content.starts_with("[Tool results]\n[Tool attachments: 1]\n"));
        assert!(content.contains("[IMAGE:/tmp/a.png]\n"));
        // Body keeps both outputs verbatim after the marker lines.
        assert!(content.contains("<tool_result name=\"shell\">\nls\n</tool_result>"));
        assert!(content.contains("<tool_result name=\"image_info\">"));
    }

    #[test]
    fn prompt_carrier_zero_attachments_writes_zero_header() {
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "assistant text".to_string(),
            &[],
            &[result(None, "text only", Vec::new())],
            "text only",
            false,
        );

        let content = &history[1].content;
        assert_eq!(
            content.lines().take(2).collect::<Vec<_>>(),
            vec!["[Tool results]", "[Tool attachments: 0]"],
            "zero attachments still declare the count line"
        );
        assert!(content.ends_with("text only"));
    }

    #[test]
    fn id_fallback_pairs_result_with_native_call_id() {
        let calls = vec![ToolCall {
            id: "native-id-1".to_string(),
            name: "shell".to_string(),
            arguments: "{}".to_string(),
            extra_content: None,
        }];
        let mut history = Vec::new();
        append_tool_round_to_history(
            &mut history,
            "assistant text".to_string(),
            &calls,
            &[result(None, "output", Vec::new())],
            "unused",
            true,
        );

        let value: serde_json::Value = serde_json::from_str(&history[1].content).unwrap();
        assert_eq!(value["tool_call_id"], "native-id-1");
    }
}
