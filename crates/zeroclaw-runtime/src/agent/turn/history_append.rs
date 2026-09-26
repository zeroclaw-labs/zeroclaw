//! History append for one tool round: the assistant message plus per-call
//! `role=tool` messages (native) or a `[Tool results]` user message (prompt
//! mode).
//!
//! Attachments ride the carrier grammar from `zeroclaw_api::tool_carrier` at
//! a fixed position the parsers reach without scanning body text: a sibling
//! `attachments` array in each native envelope (always present, `[]` for
//! none) and a `[Tool attachments: N]` count line plus N marker lines in the
//! prompt-mode carrier (N aggregated across the round's tools).

use zeroclaw_api::tool_carrier::{render_native_attachments, render_prompt_tool_carrier};
use zeroclaw_providers::{ChatMessage, ToolCall};

use super::results_collect::ToolRoundResult;

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
                history.push(ChatMessage::tool(native_tool_carrier(
                    result.tool_call_id.as_deref(),
                    result,
                )));
            }
        } else {
            let round_attachments: Vec<_> = individual_results
                .iter()
                .flat_map(|result| result.attachments.iter().cloned())
                .collect();
            history.push(ChatMessage::user(render_prompt_tool_carrier(
                tool_results,
                &round_attachments,
            )));
        }
    } else {
        // `zip` would drop trailing results on any length divergence,
        // leaving a native tool_use id with no matching tool_result.
        // Pair on each result's own id instead.
        for (idx, result) in individual_results.iter().enumerate() {
            let resolved_id = result
                .tool_call_id
                .clone()
                .or_else(|| native_tool_calls.get(idx).map(|call| call.id.clone()));
            history.push(ChatMessage::tool(native_tool_carrier(
                resolved_id.as_deref(),
                result,
            )));
        }
    }
}

/// Serialize one native tool-result envelope with its attachments sibling.
/// `content` stays the verbatim result text; the array is always present so
/// a reader can tell a zero-attachment carrier from a legacy one.
fn native_tool_carrier(tool_call_id: Option<&str>, result: &ToolRoundResult) -> String {
    let tool_msg = serde_json::json!({
        "tool_call_id": tool_call_id,
        "content": result.output,
        "attachments": render_native_attachments(&result.attachments),
    });
    tool_msg.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::media::{MarkerKind, RenderedMarker};

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
