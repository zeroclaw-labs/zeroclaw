//! Human-facing progress lines for the streamed turn status surface.
//!
//! The raw tool call remains the source of truth. These helpers derive a short
//! display-only summary at emission time so start and completion lines stay
//! correlated without storing duplicate call metadata.

use super::redact::scrub_credentials;
use crate::util::truncate_with_ellipsis;
use serde_json::{Map, Value};
use zeroclaw_config::schema::{
    DEFAULT_STREAM_TOOL_ARGUMENT_CHARS, StreamToolArgumentBase, StreamToolArgumentEntry,
};

const LEGACY_TOOL_ARGUMENT_HINT_KEYS: &[&str] = &["command", "path", "action", "query"];
const RUNTIME_ONLY_ARGUMENT_KEYS: &[&str] = &["approved", "__config"];

/// Render the start line for a tool call using the same argument hint that
/// completion lines use.
pub(crate) fn render_tool_start_progress(
    tool: &str,
    args: &Value,
    stream_tool_arguments: Option<&[StreamToolArgumentEntry]>,
) -> String {
    match tool_argument_hint(tool, args, stream_tool_arguments) {
        Some(hint) => format!("\u{23f3} {tool}: {hint}\n"),
        None => format!("\u{23f3} {tool}\n"),
    }
}

/// Render the completion line for a tool call. Failures include the same
/// argument hint as successes, plus scrubbed error text when available.
pub(crate) fn render_tool_completion_progress(
    tool: &str,
    args: &Value,
    secs: u64,
    success: bool,
    error_reason: Option<&str>,
    stream_tool_arguments: Option<&[StreamToolArgumentEntry]>,
) -> String {
    let subject = match tool_argument_hint(tool, args, stream_tool_arguments) {
        Some(hint) => format!("{tool}: {hint}"),
        None => tool.to_string(),
    };

    if success {
        format!("\u{2705} {subject} ({secs}s)\n")
    } else if let Some(reason) = error_reason {
        format!(
            "\u{274c} {subject} ({secs}s): {}\n",
            truncate_with_ellipsis(&scrub_and_collapse_display(reason), 200)
        )
    } else {
        format!("\u{274c} {subject} ({secs}s)\n")
    }
}

/// Build a compact argument summary from either the existing non-Matrix policy
/// or the Matrix single-message policy supplied for this turn.
fn tool_argument_hint(
    tool: &str,
    args: &Value,
    stream_tool_arguments: Option<&[StreamToolArgumentEntry]>,
) -> Option<String> {
    let Value::Object(map) = args else {
        return None;
    };

    let (keys, max_chars) = stream_tool_arguments.map_or_else(
        || {
            (
                legacy_argument_keys(map),
                DEFAULT_STREAM_TOOL_ARGUMENT_CHARS,
            )
        },
        |settings| configured_argument_display(tool, map, settings),
    );
    let parts: Vec<String> = keys
        .into_iter()
        .filter_map(|key| render_argument(key, map.get(key)?, max_chars))
        .collect();

    (!parts.is_empty()).then(|| parts.join(", "))
}

fn legacy_argument_keys(map: &Map<String, Value>) -> Vec<&str> {
    LEGACY_TOOL_ARGUMENT_HINT_KEYS
        .iter()
        .copied()
        .filter(|key| map.contains_key(*key))
        .collect()
}

fn configured_argument_display<'a>(
    tool: &str,
    map: &'a Map<String, Value>,
    settings: &'a [StreamToolArgumentEntry],
) -> (Vec<&'a str>, usize) {
    let (default_base, default_chars) = settings
        .iter()
        .find_map(|entry| match entry {
            StreamToolArgumentEntry::Defaults {
                default_base,
                argument_chars,
            } => Some((
                *default_base,
                argument_chars.unwrap_or(DEFAULT_STREAM_TOOL_ARGUMENT_CHARS),
            )),
            StreamToolArgumentEntry::Tool { .. } => None,
        })
        .unwrap_or((
            StreamToolArgumentBase::default(),
            DEFAULT_STREAM_TOOL_ARGUMENT_CHARS,
        ));
    let rule = settings.iter().find_map(|entry| match entry {
        StreamToolArgumentEntry::Tool {
            tool: rule_tool,
            base,
            include,
            exclude,
            argument_chars,
        } if rule_tool == tool => Some((
            *base,
            include.as_slice(),
            exclude.as_slice(),
            *argument_chars,
        )),
        _ => None,
    });
    let (base, include, exclude, max_chars) = rule.map_or(
        (default_base, &[][..], &[][..], default_chars),
        |(base, include, exclude, argument_chars)| {
            (
                base.unwrap_or(default_base),
                include,
                exclude,
                argument_chars.unwrap_or(default_chars),
            )
        },
    );

    let mut keys: Vec<&str> = match base {
        StreamToolArgumentBase::None => Vec::new(),
        StreamToolArgumentBase::Safe => safe_argument_keys(tool)
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|key| map.contains_key(*key))
            .collect(),
        StreamToolArgumentBase::All => map.keys().map(String::as_str).collect(),
    };

    for key in include {
        if map.contains_key(key) && !keys.contains(&key.as_str()) {
            keys.push(key);
        }
    }
    keys.retain(|key| !exclude.iter().any(|excluded| excluded == key));
    keys.retain(|key| !RUNTIME_ONLY_ARGUMENT_KEYS.contains(key));
    (keys, max_chars)
}

/// The sole source of truth for built-in `safe` argument visibility. Every
/// standard tool is intentionally classified; an empty slice means that the
/// reviewed safe representation is the tool name alone. Names absent from this
/// match are extension tools and therefore also resolve to name-only.
#[allow(clippy::too_many_lines)]
fn safe_argument_keys(tool: &str) -> Option<&'static [&'static str]> {
    match tool {
        // Local execution and discovery.
        "shell" => Some(&["command"]),
        "file_read" => Some(&["path", "offset", "limit", "encoding"]),
        "file_write" => Some(&["path", "encoding"]),
        "file_edit" => Some(&["path"]),
        "glob_search" => Some(&["pattern"]),
        "content_search" => Some(&[
            "pattern",
            "path",
            "output_mode",
            "include",
            "case_sensitive",
            "max_results",
        ]),
        "git_operations" => Some(&["action", "path", "branch", "remote"]),

        // Scheduling, delegation, sessions, and operator interaction.
        "cron_add" => Some(&["name", "job_type"]),
        "cron_remove" | "cron_run" => Some(&["job_id"]),
        "cron_runs" => Some(&["job_id", "limit"]),
        "cron_update" => Some(&["job_id"]),
        "schedule" => Some(&["action", "expression", "delay", "run_at", "id"]),
        "delegate" => Some(&["action", "agent", "background", "task_id", "timeout_ms"]),
        "send_message_to_peer" => Some(&["channel", "target"]),
        "send_via" => Some(&["channel", "to"]),
        "ask_user" => Some(&["channel", "timeout_secs"]),
        "escalate_to_human" => Some(&["urgency", "wait_for_response", "timeout_secs"]),
        "reaction" => Some(&["action", "channel", "emoji"]),
        "poll" => Some(&["channel", "duration_minutes", "multi_select"]),
        "channel_room" => Some(&["action", "channel", "name", "visibility", "encryption"]),
        "sessions_list" => Some(&["agent", "limit"]),
        "sessions_history" => Some(&["session_id", "limit"]),
        "sessions_send" => Some(&["session_id"]),
        "sessions_reset" | "sessions_delete" => Some(&["session_id"]),

        // Memory, model control, skills, and SOPs.
        "memory_store" => Some(&["category", "session_id"]),
        "memory_recall" => Some(&["query", "category", "limit", "session_id"]),
        "memory_forget" => Some(&["id", "category", "session_id"]),
        "memory_export" | "memory_purge" => {
            Some(&["namespace", "session_id", "category", "since", "until"])
        }
        "model_routing_config" | "model_switch" | "proxy_config" => {
            Some(&["action", "model_provider", "model", "scope", "service"])
        }
        "read_skill" | "skill_view" => Some(&["name"]),
        "skills_list" => Some(&["source"]),
        "skill_manage" => Some(&["action", "name"]),
        "sop_execute" | "sop_advance" | "sop_approve" | "sop_status" => {
            Some(&["sop_id", "run_id", "step_id", "action"])
        }
        "sop_workshop" => Some(&["action", "name"]),

        // Network, browser, data movement, and first-party integrations.
        "browser" | "browser_delegate" | "text_browser" => Some(&["action"]),
        "http_request" => Some(&["method"]),
        "web_search_tool" | "tool_search" => Some(&["query", "max_results"]),
        "file_upload" | "file_upload_bundle" | "file_download" | "image_info" => Some(&["path"]),
        "canvas" | "backup" | "data_management" | "security_ops" => Some(&["action"]),
        "image_gen" => Some(&["model", "size", "quality"]),
        "cloud_ops" | "cloud_patterns" | "project_intel" | "report_template" => {
            Some(&["action", "provider", "path", "name", "format"])
        }
        "notion" | "jira" | "microsoft365" | "google_workspace" | "linkedin" | "composio" => {
            Some(&["action"])
        }
        "email_search" => Some(&["folder", "limit"]),
        "email_read" => Some(&["message_id", "folder"]),
        "discord_search" => Some(&["channel_id", "limit"]),
        "hardware_board_info" => Some(&["board"]),
        "hardware_memory_map" | "hardware_memory_read" => Some(&["board", "address", "length"]),
        "mcp_resources" | "mcp_prompts" => Some(&["action"]),

        // These standard tools carry only sensitive/free-form payloads or have
        // no useful arguments. Their safe representation is intentionally the
        // tool name alone.
        "cron_list" | "spawn_subagent" | "sessions_current" | "sop_list" | "browser_open"
        | "web_fetch" | "execute_pipeline" | "knowledge" | "llm_task" | "vi_verify"
        | "screenshot" | "weather" | "pushover" | "calculator" | "claude_code"
        | "claude_code_runner" | "codex_cli" | "gemini_cli" | "opencode_cli" => Some(&[]),
        _ => None,
    }
}

/// Whether a name is reserved by the built-in Matrix stream policy, including
/// built-ins that are disabled in the current runtime configuration.
#[cfg(feature = "plugins-wasm")]
pub(crate) fn has_builtin_stream_tool_policy(tool: &str) -> bool {
    safe_argument_keys(tool).is_some()
}

/// Convert one JSON argument value to compact display text while applying the
/// shared secret-key heuristic and both credential scrubbers.
fn render_argument(key: &str, value: &Value, max_chars: usize) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let rendered = if crate::approval::looks_like_secret_key(key) {
        "[redacted]".to_string()
    } else {
        let rendered = match value {
            Value::String(s) => s.clone(),
            Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
                serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
            }
            Value::Null => return None,
        };
        let prefix = format!("{key}=");
        let scrubbed = scrub_and_collapse_display(&format!("{prefix}{rendered}"));
        scrubbed
            .strip_prefix(&prefix)
            .unwrap_or(&scrubbed)
            .to_string()
    };
    if rendered.is_empty() {
        None
    } else {
        let rendered = if max_chars == 0 {
            rendered
        } else {
            truncate_with_ellipsis(&rendered, max_chars)
        };
        Some(format!("{key}={rendered}"))
    }
}

fn scrub_and_collapse_display(value: &str) -> String {
    scrub_credentials(&crate::security::scrub(value))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{render_tool_completion_progress, render_tool_start_progress};
    use serde_json::json;
    use zeroclaw_config::schema::{StreamToolArgumentBase, StreamToolArgumentEntry};

    #[test]
    fn start_progress_reports_only_allowlisted_arguments() {
        let line = render_tool_start_progress(
            "delegate",
            &json!({
                "agent": "sysadmin",
                "prompt": "Check **service**\nthen report",
                "query": "current state",
                "background": true
            }),
            None,
        );

        assert_eq!(line, "\u{23f3} delegate: query=current state\n");
    }

    #[test]
    fn completion_progress_reuses_argument_hint() {
        let line = render_tool_completion_progress(
            "web_fetch",
            &json!({"url": "https://example.test/a?b=c", "method": "GET", "path": "/tmp/out"}),
            3,
            true,
            None,
            None,
        );

        assert_eq!(line, "\u{2705} web_fetch: path=/tmp/out (3s)\n");
    }

    #[test]
    fn progress_omits_arguments_without_allowlisted_keys() {
        let line = render_tool_start_progress(
            "delegate",
            &json!({"agent": "sysadmin", "prompt": "private details"}),
            None,
        );

        assert_eq!(line, "\u{23f3} delegate\n");
    }

    #[test]
    fn completion_progress_failure_scrubs_error_reason() {
        let line = render_tool_completion_progress(
            "config_read",
            &json!({"path": "/tmp/config.toml"}),
            2,
            false,
            Some("api_key = \"sk-live-abcd1234efgh5678\""),
            None,
        );

        assert!(
            line.contains("[REDACTED]"),
            "expected scrubbed line: {line}"
        );
        assert!(
            !line.contains("abcd1234efgh5678"),
            "raw secret leaked: {line}"
        );
        assert!(line.contains("path=/tmp/config.toml"));
    }

    #[test]
    fn completion_progress_failure_keeps_error_reason_on_one_line() {
        let line = render_tool_completion_progress(
            "shell",
            &json!({"command": "printf fail"}),
            1,
            false,
            Some("first line\nsecond line"),
            None,
        );

        assert_eq!(
            line,
            "\u{274c} shell: command=printf fail (1s): first line second line\n"
        );
    }

    #[test]
    fn progress_uses_sixty_char_cap_when_not_overridden() {
        let args =
            json!({"command": "012345678901234567890123456789012345678901234567890123456789tail"});
        let defaults = [StreamToolArgumentEntry::Defaults {
            default_base: StreamToolArgumentBase::Safe,
            argument_chars: None,
        }];
        let expected = "\u{23f3} shell: command=012345678901234567890123456789012345678901234567890123456789...\n";

        assert_eq!(render_tool_start_progress("shell", &args, None), expected);
        assert_eq!(
            render_tool_start_progress("shell", &args, Some(&defaults)),
            expected
        );
    }

    #[test]
    fn progress_omits_runtime_approved_control_arg() {
        let line = render_tool_start_progress(
            "shell",
            &json!({"command": "uname -a", "approved": true}),
            None,
        );

        assert_eq!(line, "\u{23f3} shell: command=uname -a\n");
    }

    #[test]
    fn safe_policy_uses_tool_specific_defaults() {
        let line = render_tool_start_progress(
            "delegate",
            &json!({
                "action": "delegate",
                "agent": "sysadmin",
                "prompt": "inspect private context",
                "context": "private context",
                "background": true,
                "parallel": ["sysadmin", "reviewer"],
                "timeout_ms": 30_000
            }),
            Some(&[]),
        );

        assert_eq!(
            line,
            "\u{23f3} delegate: action=delegate, agent=sysadmin, background=true, timeout_ms=30000\n"
        );
    }

    #[test]
    fn safe_policy_keeps_unknown_tools_name_only() {
        let line = render_tool_start_progress(
            "server__unknown",
            &json!({"path": "/tmp/private", "query": "secret query"}),
            Some(&[]),
        );

        assert_eq!(line, "\u{23f3} server__unknown\n");
    }

    #[test]
    fn rule_can_add_to_or_replace_safe_defaults() {
        let include_prompt = [StreamToolArgumentEntry::Tool {
            tool: "delegate".into(),
            base: Some(StreamToolArgumentBase::None),
            include: vec!["agent".into(), "prompt".into()],
            exclude: vec![],
            argument_chars: None,
        }];
        let line = render_tool_start_progress(
            "delegate",
            &json!({"agent": "reviewer", "prompt": "check\nformatting", "background": true}),
            Some(&include_prompt),
        );

        assert_eq!(
            line,
            "\u{23f3} delegate: agent=reviewer, prompt=check formatting\n"
        );
    }

    #[test]
    fn default_all_honors_excludes_and_runtime_internal_fields() {
        let settings = [
            StreamToolArgumentEntry::Tool {
                tool: "mock_tool".into(),
                base: None,
                include: vec![],
                exclude: vec!["token".into()],
                argument_chars: None,
            },
            StreamToolArgumentEntry::Defaults {
                default_base: StreamToolArgumentBase::All,
                argument_chars: None,
            },
        ];
        let line = render_tool_start_progress(
            "mock_tool",
            &json!({"action": "get", "approved": true, "token": "short-secret"}),
            Some(&settings),
        );

        assert_eq!(line, "\u{23f3} mock_tool: action=get\n");
    }

    #[test]
    fn default_none_disables_arguments_for_every_tool() {
        let settings = [StreamToolArgumentEntry::Defaults {
            default_base: StreamToolArgumentBase::None,
            argument_chars: None,
        }];
        let line = render_tool_start_progress(
            "file_read",
            &json!({"path": "/tmp/data", "limit": 10}),
            Some(&settings),
        );

        assert_eq!(line, "\u{23f3} file_read\n");
    }

    #[test]
    fn explicit_default_all_enables_unknown_tool_arguments() {
        let settings = [StreamToolArgumentEntry::Defaults {
            default_base: StreamToolArgumentBase::All,
            argument_chars: None,
        }];
        let line = render_tool_start_progress(
            "server__unknown",
            &json!({"path": "/tmp/data", "approved": true}),
            Some(&settings),
        );

        assert_eq!(line, "\u{23f3} server__unknown: path=/tmp/data\n");
    }

    #[test]
    fn explicitly_selected_secret_fields_are_redacted() {
        let settings = [StreamToolArgumentEntry::Tool {
            tool: "mock_tool".into(),
            base: Some(StreamToolArgumentBase::None),
            include: vec!["api_key".into(), "prompt".into()],
            exclude: vec![],
            argument_chars: None,
        }];
        let line = render_tool_start_progress(
            "mock_tool",
            &json!({
                "api_key": "not-even-long-enough-for-pattern-detection",
                "prompt": "use ghp_abcdefghijklmnopqrstuvwxyz1234567890ABCD"
            }),
            Some(&settings),
        );

        assert!(line.contains("api_key=[redacted]"), "{line}");
        assert!(line.contains("[REDACTED"), "{line}");
        assert!(!line.contains("ghp_"), "{line}");
    }

    #[test]
    fn default_entry_overrides_argument_value_cap() {
        let settings = [StreamToolArgumentEntry::Defaults {
            default_base: StreamToolArgumentBase::Safe,
            argument_chars: Some(8),
        }];
        let line = render_tool_start_progress(
            "shell",
            &json!({"command": "abcdefghijkl"}),
            Some(&settings),
        );

        assert_eq!(line, "\u{23f3} shell: command=abcdefgh...\n");
    }

    #[test]
    fn tool_rule_overrides_default_argument_value_cap() {
        let settings = [
            StreamToolArgumentEntry::Defaults {
                default_base: StreamToolArgumentBase::Safe,
                argument_chars: Some(8),
            },
            StreamToolArgumentEntry::Tool {
                tool: "shell".into(),
                base: None,
                include: vec![],
                exclude: vec![],
                argument_chars: Some(0),
            },
        ];
        let line = render_tool_start_progress(
            "shell",
            &json!({"command": "abcdefghijkl"}),
            Some(&settings),
        );

        assert_eq!(line, "\u{23f3} shell: command=abcdefghijkl\n");
    }
}
