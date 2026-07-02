//! Human-facing progress lines for the streamed turn status surface.
//!
//! The raw tool call remains the source of truth. These helpers derive a short
//! display-only summary at emission time so start and completion lines stay
//! correlated without storing duplicate call metadata.

use super::redact::scrub_credentials;
use crate::util::truncate_with_ellipsis;
use serde_json::Value;

const MAX_PROGRESS_FIELDS: usize = 4;
const MAX_PROGRESS_VALUE_CHARS: usize = 96;
const MAX_PROGRESS_HINT_CHARS: usize = 240;

/// Render the start line for a tool call using the same argument hint that
/// completion lines use.
pub(crate) fn render_tool_start_progress(tool: &str, args: &Value) -> String {
    match tool_argument_hint(args) {
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
) -> String {
    let subject = match tool_argument_hint(args) {
        Some(hint) => format!("{tool}: {hint}"),
        None => tool.to_string(),
    };

    if success {
        format!("\u{2705} {subject} ({secs}s)\n")
    } else if let Some(reason) = error_reason {
        format!(
            "\u{274c} {subject} ({secs}s): {}\n",
            truncate_with_ellipsis(&scrub_credentials(reason), 200)
        )
    } else {
        format!("\u{274c} {subject} ({secs}s)\n")
    }
}

/// Build a compact, tool-agnostic argument summary. This deliberately avoids
/// tool-name branching: every tool gets the same object-key rendering path.
fn tool_argument_hint(args: &Value) -> Option<String> {
    let hint = match args {
        Value::Object(map) => {
            let mut parts = Vec::new();
            let mut omitted = 0usize;
            for (key, value) in map {
                // `approved` is runtime control metadata injected after the
                // model call; showing it would add noise and would not help
                // operators distinguish parallel calls.
                if key == "approved" {
                    continue;
                }
                let Some(value) = render_argument_value(value) else {
                    omitted += 1;
                    continue;
                };
                if parts.len() >= MAX_PROGRESS_FIELDS {
                    omitted += 1;
                    continue;
                }
                parts.push(format!(
                    "{key}={}",
                    truncate_with_ellipsis(&value, MAX_PROGRESS_VALUE_CHARS)
                ));
            }
            if parts.is_empty() {
                return None;
            }
            if omitted > 0 {
                parts.push(format!("+{omitted} more"));
            }
            parts.join(", ")
        }
        Value::Null => return None,
        value => {
            let value = render_argument_value(value)?;
            format!("args={value}")
        }
    };

    Some(truncate_with_ellipsis(&hint, MAX_PROGRESS_HINT_CHARS))
}

/// Convert one JSON argument value to compact display text while scrubbing
/// credential-shaped data before it reaches the progress stream.
fn render_argument_value(value: &Value) -> Option<String> {
    let rendered = match value {
        Value::Null => return None,
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
        }
    };
    let rendered = scrub_credentials(&rendered);
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

#[cfg(test)]
mod tests {
    use super::{render_tool_completion_progress, render_tool_start_progress};
    use serde_json::json;

    /// Delegate has no bespoke formatter; generic argument rendering is enough
    /// to make the target and task visible in streamed progress.
    #[test]
    fn start_progress_reports_generic_tool_arguments() {
        let line = render_tool_start_progress(
            "delegate",
            &json!({
                "agent": "sysadmin",
                "prompt": "Check **service**\nthen report",
                "background": true
            }),
        );

        assert_eq!(
            line,
            "\u{23f3} delegate: agent=sysadmin, background=true, prompt=Check **service**\nthen report\n"
        );
    }

    #[test]
    fn completion_progress_reuses_argument_hint() {
        let line = render_tool_completion_progress(
            "web_fetch",
            &json!({"url": "https://example.test/a?b=c", "method": "GET"}),
            3,
            true,
            None,
        );

        assert_eq!(
            line,
            "\u{2705} web_fetch: method=GET, url=https://example.test/a?b=c (3s)\n"
        );
    }

    #[test]
    fn completion_progress_failure_scrubs_error_reason() {
        let line = render_tool_completion_progress(
            "config_read",
            &json!({"path": "/tmp/config.toml"}),
            2,
            false,
            Some("api_key = \"sk-live-abcd1234efgh5678\""),
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
    fn progress_omits_runtime_approved_control_arg() {
        let line =
            render_tool_start_progress("shell", &json!({"command": "uname -a", "approved": true}));

        assert_eq!(line, "\u{23f3} shell: command=uname -a\n");
    }
}
