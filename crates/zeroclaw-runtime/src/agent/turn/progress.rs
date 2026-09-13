//! Human-facing progress lines for the streamed turn status surface.
//!
//! The raw tool call remains the source of truth. These helpers derive a short
//! display-only summary at emission time so start and completion lines stay
//! correlated without storing duplicate call metadata.

use super::redact::scrub_credentials;
use crate::util::truncate_with_ellipsis;
use serde_json::Value;

const MAX_PROGRESS_VALUE_CHARS: usize = 60;

pub(crate) fn render_tool_start_progress(tool: &str, args: &Value) -> String {
    match tool_argument_hint(tool, args) {
        Some(hint) => format!("\u{23f3} {tool}: {hint}\n"),
        None => format!("\u{23f3} {tool}\n"),
    }
}

pub(crate) fn render_tool_completion_progress(
    tool: &str,
    _args: &Value,
    secs: u64,
    success: bool,
    error_reason: Option<&str>,
) -> String {
    if success {
        format!("\u{2705} {tool} ({secs}s)\n")
    } else if let Some(reason) = error_reason {
        format!(
            "\u{274c} {tool} ({secs}s): {}\n",
            truncate_with_ellipsis(&scrub_and_collapse_display(reason), 200)
        )
    } else {
        format!("\u{274c} {tool} ({secs}s)\n")
    }
}

/// Preserve the historical non-Matrix progress contract: start lines expose
/// one tool-specific string hint, while completion lines identify only the
/// finished tool and outcome.
fn tool_argument_hint(tool: &str, args: &Value) -> Option<String> {
    let Value::Object(map) = args else {
        return None;
    };

    let value = match tool {
        "shell" => map.get("command"),
        "file_read" | "file_write" => map.get("path"),
        _ => map.get("action").or_else(|| map.get("query")),
    }?;
    let Value::String(value) = value else {
        return None;
    };
    let value = scrub_and_collapse_display(value);
    (!value.is_empty()).then(|| truncate_with_ellipsis(&value, MAX_PROGRESS_VALUE_CHARS))
}

fn scrub_and_collapse_display(value: &str) -> String {
    // Progress is a chat-visible sink. Catch standalone credential prefixes
    // with the canonical detector before preserving key/value context below.
    scrub_credentials(&crate::security::scrub(value))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{render_tool_completion_progress, render_tool_start_progress};
    use serde_json::json;

    #[test]
    fn legacy_progress_uses_one_tool_specific_start_hint_and_no_completion_hint() {
        let args = json!({
            "command": "pwd",
            "path": "/tmp/file",
            "action": "status",
            "query": "health",
            "prompt": "private",
        });
        let start = render_tool_start_progress("delegate", &args);
        let completion = render_tool_completion_progress("delegate", &args, 3, true, None);
        assert_eq!(start, "\u{23f3} delegate: status\n");
        assert_eq!(completion, "\u{2705} delegate (3s)\n");
    }

    #[test]
    fn legacy_progress_ignores_composite_and_non_applicable_hint_fields() {
        let args = json!({
            "command": "deploy",
            "path": "/private",
            "query": ["internal"],
        });

        assert_eq!(
            render_tool_start_progress("custom", &args),
            "\u{23f3} custom\n"
        );
        assert_eq!(
            render_tool_completion_progress("custom", &args, 1, false, None),
            "\u{274c} custom (1s)\n"
        );
    }

    #[test]
    fn legacy_progress_preserves_shell_and_file_hint_selection() {
        let args = json!({
            "command": "pwd",
            "path": "/private",
            "action": "status",
            "query": "health",
        });

        assert_eq!(
            render_tool_start_progress("shell", &args),
            "\u{23f3} shell: pwd\n"
        );
        assert_eq!(
            render_tool_start_progress("file_read", &args),
            "\u{23f3} file_read: /private\n"
        );
    }

    #[test]
    fn completion_progress_failure_scrubs_credential_error_reason() {
        let line = render_tool_completion_progress(
            "config_read",
            &json!({"path": "/tmp/config.toml"}),
            2,
            false,
            Some("api_key = \"sk-live-abcd1234efgh5678\""),
        );
        assert!(line.contains("[REDACTED]"));
        assert!(!line.contains("abcd1234efgh5678"));
    }

    #[test]
    fn progress_scrubs_standalone_credentials() {
        let token = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let start = render_tool_start_progress("shell", &json!({"command": token}));
        let completion = render_tool_completion_progress(
            "shell",
            &json!({"command": "echo"}),
            1,
            false,
            Some(token),
        );

        assert!(!start.contains(token));
        assert!(!completion.contains(token));
    }
}
