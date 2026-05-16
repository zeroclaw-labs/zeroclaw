//! `ObserverEvent` → `StatusUpdate` translation helpers.
//!
//! Pure functions, no I/O. Easy to unit-test exhaustively.

use zeroclaw_api::channel::{StatusPhase, StatusUpdate};
use zeroclaw_api::observability_traits::ObserverEvent;

use crate::toggles::ProgressEventToggles;

const ARG_SNIPPET_MAX_CHARS: usize = 120;

pub(crate) fn summarize_tool_args(args_json: Option<&str>) -> Option<String> {
    let raw = args_json?;
    if raw.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["command", "query", "path", "url"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return Some(truncate_chars(s, ARG_SNIPPET_MAX_CHARS));
            }
        }
        return Some(truncate_chars(raw, ARG_SNIPPET_MAX_CHARS));
    }
    Some(truncate_chars(raw, ARG_SNIPPET_MAX_CHARS))
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

pub(crate) fn event_to_status(
    execution_id: &str,
    event: &ObserverEvent,
    toggles: &ProgressEventToggles,
) -> Option<StatusUpdate> {
    match event {
        ObserverEvent::AgentStart { provider, model } if toggles.agent_start => {
            Some(make(
                execution_id,
                StatusPhase::AgentStart,
                "agent",
                format!("Agent 启动（{}/{}）", provider, model),
            ))
        }
        ObserverEvent::AgentEnd { .. } if toggles.agent_end => {
            Some(make(execution_id, StatusPhase::AgentEnd, "agent", "处理完成".into()))
        }
        ObserverEvent::LlmRequest { messages_count, .. } if toggles.llm_thinking => {
            Some(make(
                execution_id,
                StatusPhase::LlmThinking,
                "llm",
                format!("正在调用大模型推理（{} 条消息）", messages_count),
            ))
        }
        ObserverEvent::ToolCallStart { tool, arguments } if toggles.tool_call_start => {
            let snippet = summarize_tool_args(arguments.as_deref());
            let desc = format_tool_start_desc(tool, snippet.as_deref());
            Some(make(execution_id, StatusPhase::ToolStart, tool, desc))
        }
        ObserverEvent::ToolCall { tool, duration, success } if toggles.tool_call => {
            let elapsed_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
            let desc = if *success {
                format!("{} 执行完成（{}ms）", tool, elapsed_ms)
            } else {
                format!("{} 执行失败", tool)
            };
            Some(make(
                execution_id,
                StatusPhase::ToolDone { success: *success, elapsed_ms },
                tool,
                desc,
            ))
        }
        ObserverEvent::Error { component, message } if toggles.error => {
            let trimmed = truncate_chars(message, 200);
            Some(make(
                execution_id,
                StatusPhase::Error,
                "error",
                format!("{} 出现错误：{}", component, trimmed),
            ))
        }
        _ => None,
    }
}

fn format_tool_start_desc(tool: &str, snippet: Option<&str>) -> String {
    match (tool, snippet) {
        ("shell", Some(s)) => format!("执行命令：{}", s),
        ("web_search", Some(s)) => format!("搜索：{}", s),
        ("read_file", Some(s)) => format!("读取文件：{}", s),
        ("http", Some(s)) => format!("HTTP 请求：{}", s),
        (other, _) => format!("调用工具：{}", other),
    }
}

fn make(execution_id: &str, phase: StatusPhase, name: &str, desc: String) -> StatusUpdate {
    StatusUpdate {
        execution_id: execution_id.to_owned(),
        phase,
        name: name.to_owned(),
        desc,
    }
}

#[cfg(test)]
mod summarize_tests {
    use super::*;

    #[test]
    fn none_input_returns_none() {
        assert!(summarize_tool_args(None).is_none());
    }

    #[test]
    fn empty_input_returns_none() {
        assert!(summarize_tool_args(Some("")).is_none());
    }

    #[test]
    fn extracts_command_key() {
        let arg = r#"{"command": "grep -c TODO README.md"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("grep -c TODO README.md"),
        );
    }

    #[test]
    fn extracts_query_key() {
        let arg = r#"{"query": "rust async runtime"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("rust async runtime"),
        );
    }

    #[test]
    fn extracts_path_key() {
        let arg = r#"{"path": "./README.md"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("./README.md"));
    }

    #[test]
    fn extracts_url_key() {
        let arg = r#"{"url": "https://example.com/x"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("https://example.com/x"));
    }

    #[test]
    fn prefers_command_over_others_when_multiple_keys_present() {
        let arg = r#"{"command": "ls", "query": "ignored"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("ls"));
    }

    #[test]
    fn falls_back_to_truncated_json_when_no_known_key() {
        let arg = r#"{"random": "x"}"#;
        let out = summarize_tool_args(Some(arg)).unwrap();
        assert_eq!(out, arg);
    }

    #[test]
    fn falls_back_to_truncated_raw_when_not_valid_json() {
        let arg = "garbage not-json";
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some(arg));
    }

    #[test]
    fn truncates_long_command_with_ellipsis() {
        let long_cmd = "x".repeat(200);
        let arg = format!(r#"{{"command":"{}"}}"#, long_cmd);
        let out = summarize_tool_args(Some(&arg)).unwrap();
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), ARG_SNIPPET_MAX_CHARS + 1);
    }

    #[test]
    fn truncate_handles_multibyte_utf8_safely() {
        let s = "中".repeat(200);
        let result = truncate_chars(&s, 10);
        assert_eq!(result.chars().count(), 11);
        assert!(result.ends_with('…'));
    }
}

#[cfg(test)]
mod event_to_status_tests {
    use super::*;
    use std::time::Duration;

    fn all_on() -> ProgressEventToggles {
        ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        }
    }

    #[test]
    fn agent_start_emits_when_enabled() {
        let ev = ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "sonnet".into(),
        };
        let out = event_to_status("exec-1", &ev, &all_on()).unwrap();
        assert_eq!(out.execution_id, "exec-1");
        assert_eq!(out.phase, StatusPhase::AgentStart);
        assert_eq!(out.name, "agent");
        assert_eq!(out.desc, "Agent 启动（anthropic/sonnet）");
    }

    #[test]
    fn agent_start_returns_none_when_disabled() {
        let toggles = ProgressEventToggles::default();
        let ev = ObserverEvent::AgentStart {
            provider: "p".into(), model: "m".into(),
        };
        assert!(event_to_status("e", &ev, &toggles).is_none());
    }

    #[test]
    fn agent_end_emits_processing_complete() {
        let ev = ObserverEvent::AgentEnd {
            provider: "p".into(), model: "m".into(),
            duration: Duration::from_secs(1),
            tokens_used: None, cost_usd: None,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::AgentEnd);
        assert_eq!(out.desc, "处理完成");
    }

    #[test]
    fn llm_request_includes_message_count() {
        let ev = ObserverEvent::LlmRequest {
            provider: "p".into(), model: "m".into(), messages_count: 8,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::LlmThinking);
        assert_eq!(out.desc, "正在调用大模型推理（8 条消息）");
    }

    #[test]
    fn tool_call_start_shell_uses_command_template() {
        let ev = ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command":"grep -c TODO README.md"}"#.into()),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::ToolStart);
        assert_eq!(out.name, "shell");
        assert_eq!(out.desc, "执行命令：grep -c TODO README.md");
    }

    #[test]
    fn tool_call_start_unknown_tool_uses_generic_template() {
        let ev = ObserverEvent::ToolCallStart {
            tool: "custom_tool".into(),
            arguments: Some(r#"{"foo":"bar"}"#.into()),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.desc, "调用工具：custom_tool");
    }

    #[test]
    fn tool_call_success_includes_elapsed() {
        let ev = ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(42),
            success: true,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        match out.phase {
            StatusPhase::ToolDone { success, elapsed_ms } => {
                assert!(success);
                assert_eq!(elapsed_ms, 42);
            }
            _ => panic!("wrong phase"),
        }
        assert_eq!(out.desc, "shell 执行完成（42ms）");
    }

    #[test]
    fn tool_call_failure_uses_failure_template() {
        let ev = ObserverEvent::ToolCall {
            tool: "http".into(),
            duration: Duration::from_millis(1500),
            success: false,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.desc, "http 执行失败");
    }

    #[test]
    fn error_event_includes_component_and_message() {
        let ev = ObserverEvent::Error {
            component: "provider".into(),
            message: "rate limited".into(),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::Error);
        assert_eq!(out.name, "error");
        assert_eq!(out.desc, "provider 出现错误：rate limited");
    }

    #[test]
    fn unrelated_events_return_none() {
        // TurnComplete, HeartbeatTick, and CacheHit all exist in ObserverEvent
        // (confirmed from crates/zeroclaw-api/src/observability_traits.rs).
        let toggles = all_on();
        let cases = vec![
            ObserverEvent::TurnComplete,
            ObserverEvent::HeartbeatTick,
            ObserverEvent::CacheHit {
                cache_type: "hot".into(),
                tokens_saved: 100,
            },
            ObserverEvent::CacheMiss { cache_type: "response".into() },
            ObserverEvent::ChannelMessage {
                channel: "telegram".into(),
                direction: "inbound".into(),
            },
        ];
        for ev in cases {
            assert!(
                event_to_status("e", &ev, &toggles).is_none(),
                "expected None for {:?}", ev,
            );
        }
    }

    #[test]
    fn each_desc_is_under_120_chars() {
        let cases = vec![
            ObserverEvent::AgentStart { provider: "openai".into(), model: "gpt-4".into() },
            ObserverEvent::LlmRequest { provider: "openai".into(), model: "gpt-4".into(), messages_count: 99 },
            ObserverEvent::ToolCallStart { tool: "shell".into(), arguments: Some(r#"{"command":"ls"}"#.into()) },
            ObserverEvent::ToolCall { tool: "shell".into(), duration: Duration::from_millis(1), success: true },
            ObserverEvent::Error { component: "test".into(), message: "boom".into() },
            ObserverEvent::AgentEnd { provider: "p".into(), model: "m".into(), duration: Duration::from_secs(1), tokens_used: None, cost_usd: None },
        ];
        for ev in cases {
            if let Some(s) = event_to_status("e", &ev, &all_on()) {
                assert!(
                    s.desc.chars().count() <= 120,
                    "desc too long: {:?} ({} chars)", s.desc, s.desc.chars().count(),
                );
                assert!(!s.desc.is_empty(), "desc must not be empty");
            }
        }
    }
}
