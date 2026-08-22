//! Credential redaction for the rendering layer (logs, observer events, and
//! UI-facing turn events). This never runs on the data path: tool results fed
//! back to the model and signed by HMAC receipts always carry raw bytes.

use regex::Regex;
use std::sync::LazyLock;

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization|token|api[_-]?key|password|secret|user[_-]?key|bearer|credential|set[_-]?cookie|cookie)["']?\s*[:=]\s*(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\./+=]{8,}))"#).unwrap()
});

pub fn scrub_credentials(input: &str) -> String {
    SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let full_match = &caps[0];
            let key = &caps[1];
            let val = caps
                .get(2)
                .or(caps.get(3))
                .or(caps.get(4))
                .map(|m| m.as_str())
                .unwrap_or("");

            // Preserve first 4 chars for context, then redact.
            // Use char_indices to find the byte offset of the 4th character
            // so we never slice in the middle of a multi-byte UTF-8 sequence.
            let prefix = if val.len() > 4 {
                val.char_indices()
                    .nth(4)
                    .map(|(byte_idx, _)| &val[..byte_idx])
                    .unwrap_or(val)
            } else {
                ""
            };

            if full_match.contains(':') {
                if full_match.contains('"') {
                    format!("\"{}\": \"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}: {}*[REDACTED]", key, prefix)
                }
            } else if full_match.contains('=') {
                if full_match.contains('"') {
                    format!("{}=\"{}*[REDACTED]\"", key, prefix)
                } else {
                    format!("{}={}*[REDACTED]", key, prefix)
                }
            } else {
                format!("{}: {}*[REDACTED]", key, prefix)
            }
        })
        .to_string()
}

static SENSITIVE_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization|token|api[_-]?key|password|secret|user[_-]?key|bearer|credential|set[_-]?cookie|cookie)"#).unwrap()
});
const REDACTED_CREDENTIAL_VALUE: &str = "[REDACTED]";

/// Whether a JSON key denotes a credential value on a human-facing surface.
///
/// Keep every rendering boundary on this classifier so a top-level scalar does
/// not lose the credential context before structured redaction can inspect it.
pub fn is_credential_key(key: &str) -> bool {
    crate::approval::looks_like_secret_key(key) || SENSITIVE_KEY_REGEX.is_match(key)
}

/// Structured-aware credential scrub for a JSON value bound for a human-facing
/// surface. Object entries whose key names a credential have their entire value
/// replaced with a marker, preserving the key. Every other string leaf still
/// runs through the text [`scrub_credentials`] so inline `token=...` patterns
/// inside unrelated fields are caught too. Serialize-then-scrub would corrupt
/// key names that merely contain a sensitive word (e.g. `access_token`), so this
/// walks the value instead. Same rendering-boundary contract as
/// [`scrub_credentials`].
pub fn scrub_credentials_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let scrubbed = map
                .into_iter()
                .map(|(key, val)| {
                    if is_credential_key(&key) {
                        (
                            key,
                            serde_json::Value::String(REDACTED_CREDENTIAL_VALUE.to_string()),
                        )
                    } else {
                        (key, scrub_credentials_value(val))
                    }
                })
                .collect();
            serde_json::Value::Object(scrubbed)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(scrub_credentials_value).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(scrub_credentials(&s)),
        other => other,
    }
}

/// Log-safe string form of a tool call's `args`, for audit logs, observer
/// events, and the approval WARN record. When the tool declares a
/// source-level redaction ([`Tool::redact_args_for_log`]) — because its raw
/// arguments carry secret values the generic scrubber cannot recognize — that
/// redacted form is used; otherwise the existing generic string scrub applies.
/// The generic scrubber still runs over the redacted form as defense in depth.
pub fn loggable_args_string(
    tool: Option<&dyn zeroclaw_api::tool::Tool>,
    args: &serde_json::Value,
) -> String {
    match tool.and_then(|t| t.redact_args_for_log(args)) {
        Some(redacted) => scrub_credentials(&redacted.to_string()),
        None => scrub_credentials(&args.to_string()),
    }
}

/// Log-safe JSON form of a tool call's `args`, for structured log attributes
/// and client-facing `TurnEvent::ToolCall` frames. When the tool redacts its
/// own arguments the redacted value is returned unchanged; otherwise the
/// arguments pass through untouched, preserving prior behavior for every tool
/// that does not opt in.
pub fn loggable_args_value(
    tool: Option<&dyn zeroclaw_api::tool::Tool>,
    args: &serde_json::Value,
) -> serde_json::Value {
    tool.and_then(|t| t.redact_args_for_log(args))
        .unwrap_or_else(|| args.clone())
}

/// Presentation-safe JSON form for draft start/completion events.
///
/// Resolved tools may provide their canonical secret-aware projection. Known
/// tools without one retain their argument shape after generic credential
/// scrubbing. Unresolved tools expose no arguments: the runtime cannot know
/// which fields are safe to reflect before it resolves a trusted tool.
pub fn streamable_args_value(
    tool: Option<&dyn zeroclaw_api::tool::Tool>,
    args: &serde_json::Value,
) -> serde_json::Value {
    match tool {
        Some(tool) => tool
            .redact_args_for_log(args)
            .unwrap_or_else(|| scrub_credentials_value(args.clone())),
        None => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        is_credential_key, scrub_credentials, scrub_credentials_value, streamable_args_value,
    };
    use crate::tools::config_patch::ConfigPatchTool;
    use std::sync::Arc;
    use zeroclaw_config::policy::SecurityPolicy;

    #[test]
    fn unresolved_tool_stream_projection_hides_arbitrary_arguments() {
        let sentinel = "sentinel-progress-secret-must-not-leak";
        let projected = streamable_args_value(
            None,
            &serde_json::json!({"action": sentinel, "query": sentinel}),
        );

        assert_eq!(projected, serde_json::json!({}));
        assert!(!projected.to_string().contains(sentinel));
    }

    #[test]
    fn stream_projection_uses_the_tools_secret_aware_projection() {
        let sentinel = "sentinel-stream-secret-must-not-leak";
        let dir = tempfile::tempdir().unwrap();
        let tool = ConfigPatchTool::new(
            dir.path().join("config.toml"),
            Arc::new(SecurityPolicy::default()),
        );
        let projected = streamable_args_value(
            Some(&tool),
            &serde_json::json!({
                "ops": [{
                    "op": "add",
                    "path": "/providers/models/openai/default/api_key",
                    "value": sentinel
                }]
            }),
        );
        let rendered = projected.to_string();

        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(!rendered.contains(sentinel), "{rendered}");
    }

    #[test]
    fn credential_key_classification_combines_shared_secret_policies() {
        for key in ["private_key", "api_key", "cookie", "set-cookie", "user-key"] {
            assert!(is_credential_key(key), "expected credential key: {key}");
        }
        assert!(!is_credential_key("status"));
    }

    #[test]
    fn scrub_credentials_value_redacts_nested_secret_and_keeps_key() {
        let input = serde_json::json!({
            "body": {"access_token": "sk-live-abcdef0123456789", "status": "ok"},
            "count": 3
        });
        let out = scrub_credentials_value(input);
        let token = out["body"]["access_token"].as_str().unwrap();
        assert!(token.contains("[REDACTED]"));
        assert!(!token.contains("abcdef0123456789"));
        assert_eq!(out["body"]["status"], "ok");
        assert_eq!(out["count"], 3);
    }

    #[test]
    fn scrub_credentials_value_redacts_authorization_and_cookie_keys() {
        let input = serde_json::json!({
            "body": {
                "authorization": "Bearer sk-live-abcdef0123456789",
                "cookie": "session=deadbeefcafebabe0123",
                "set-cookie": "sid=9f8e7d6c5b4a3210feed",
                "status": "ok"
            }
        });
        let out = scrub_credentials_value(input);
        let authorization = out["body"]["authorization"].as_str().unwrap();
        assert!(authorization.contains("[REDACTED]"));
        assert!(!authorization.contains("sk-live-abcdef0123456789"));
        let cookie = out["body"]["cookie"].as_str().unwrap();
        assert!(cookie.contains("[REDACTED]"));
        assert!(!cookie.contains("deadbeefcafebabe0123"));
        let set_cookie = out["body"]["set-cookie"].as_str().unwrap();
        assert!(set_cookie.contains("[REDACTED]"));
        assert!(!set_cookie.contains("9f8e7d6c5b4a3210feed"));
        assert_eq!(out["body"]["status"], "ok");
    }

    #[test]
    fn scrub_credentials_value_matches_summary_secret_key_policy() {
        let input = serde_json::json!({"body": {"private_key": "tiny"}});
        let out = scrub_credentials_value(input);

        assert_eq!(out["body"]["private_key"], "[REDACTED]");
        assert_ne!(out["body"]["private_key"], "tiny");
    }

    #[test]
    fn scrub_credentials_value_replaces_every_credential_value_shape() {
        let input = serde_json::json!({
            "body": {
                "numeric_token": 1234,
                "credentials": {"value": "tiny"},
                "auth_enabled": true,
                "status": "ok"
            }
        });
        let out = scrub_credentials_value(input);

        for key in ["numeric_token", "credentials", "auth_enabled"] {
            assert_eq!(out["body"][key], "[REDACTED]");
        }
        assert_eq!(out["body"]["status"], "ok");
    }

    #[test]
    fn scrub_credentials_redacts_unquoted_base64_credential_values() {
        let input = "token=QWxh+GRpbjpvcGVu/IHNlc2FtZQ== next=public";
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, "token=QWxh*[REDACTED] next=public");
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }

    #[test]
    fn scrub_credentials_redacts_quoted_base64_credential_values() {
        let input = r#"secret="QWxhZGRpbjpvcGVu/IHNlc2FtZQ==""#;
        let scrubbed = scrub_credentials(input);

        assert_eq!(scrubbed, r#"secret="QWxh*[REDACTED]""#);
        assert!(!scrubbed.contains("IHNlc2FtZQ"));
        assert!(!scrubbed.contains("=="));
    }
}
