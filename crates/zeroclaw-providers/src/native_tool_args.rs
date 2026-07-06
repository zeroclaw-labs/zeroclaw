//! Shared normalization for `assistant.tool_calls[].function.arguments` strings
//! produced by the OpenAI-wire-format family of providers (openrouter, openai,
//! azure_openai, copilot, openai_codex).
//!
//! Smaller / reasoning models occasionally emit arguments strings that are
//! not well-formed JSON. Strict upstreams (Cohere via OpenRouter, OpenInference,
//! Nvidia, etc.) then reject the entire outbound request with HTTP 400 and the
//! runtime surfaces an empty "try again" fallback to the user.
//!
//! [`normalize_native_tool_call_arguments`] applies the same JSON guard
//! [`crate::compatible`] uses inline (around line 1383 of `compatible.rs`),
//! so all five OpenAI-wire providers can produce parity instead of each
//! re-implementing the same 12-line block. The guard is intentionally
//! conservative: malformed arguments are replaced with `"{}"` so the rest of
//! the tool-call (id + function name) still flows through and the provider
//! returns a structured 400 the agent can recover from, rather than a silent
//! empty turn.

/// Normalize a native tool-call `arguments` string before it is serialized
/// into the outbound request body.
///
/// - An empty or whitespace-only `arguments` becomes `"{}"` so a provider
///   that requires a stringified JSON object never sees `""` (which strict
///   upstreams reject with "tool arguments must be a stringified JSON
///   object").
/// - A well-formed JSON value is returned unchanged.
/// - A malformed value is logged at WARN with the function name and the
///   original payload (for post-incident auditing) and replaced with `"{}"`,
///   matching the existing [`crate::compatible`] inline guard.
///
/// The function is `pub(crate)` because the only callers are the OpenAI-wire
/// provider parsers in this crate. Promoting it to `pub` would require
/// stability guarantees this internal helper does not need.
pub(crate) fn normalize_native_tool_call_arguments(
    function_name: &str,
    arguments: String,
) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return arguments;
    }

    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "function": function_name,
                "arguments": arguments,
            })),
        "Invalid JSON in streamed native tool-call arguments, using empty object"
    );

    "{}".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arguments_become_empty_object() {
        assert_eq!(
            normalize_native_tool_call_arguments("shell", String::new()),
            "{}"
        );
    }

    #[test]
    fn whitespace_only_arguments_become_empty_object() {
        assert_eq!(
            normalize_native_tool_call_arguments("shell", "   \t\n".to_string()),
            "{}"
        );
    }

    #[test]
    fn valid_json_object_preserved() {
        let raw = r#"{"command":"ls -la"}"#.to_string();
        assert_eq!(
            normalize_native_tool_call_arguments("shell", raw.clone()),
            raw
        );
    }

    #[test]
    fn valid_json_array_preserved() {
        // Some providers (Cohere, OpenInference) emit arguments that parse
        // as non-object JSON. We keep anything that parses; downstream
        // schema validation is the tool's responsibility.
        let raw = r#"[1,2,3]"#.to_string();
        assert_eq!(
            normalize_native_tool_call_arguments("tool", raw.clone()),
            raw
        );
    }

    #[test]
    fn valid_json_scalars_preserved() {
        // Strings / numbers parse as JSON. Edge case worth pinning because
        // `from_str::<Value>` accepts anything that starts as valid JSON.
        assert_eq!(
            normalize_native_tool_call_arguments("tool", r#""plain""#.to_string()),
            r#""plain""#
        );
    }

    #[test]
    fn malformed_json_falls_back_to_empty_object() {
        // The realistic malformed case from #8675: trailing comma, unescaped
        // quote, truncated object, model emitting Python-style `None`.
        for malformed in [
            r#"{"command": "ls""#,                // missing closing brace
            r#"{"command": 'ls'}"#,               // Python-style single quotes
            r#"{"command": None}"#,               // Python None
            r#"Expecting ',' delimiter: line 1"#, // server error echoed back
        ] {
            assert_eq!(
                normalize_native_tool_call_arguments("shell", malformed.to_string()),
                "{}",
                "malformed input {malformed:?} should be replaced with {{}}"
            );
        }
    }

    #[test]
    fn leading_and_trailing_whitespace_does_not_disqualify_valid_json() {
        // `trim().is_empty()` is what we use for the empty check, so a
        // well-formed JSON payload with surrounding whitespace must still
        // round-trip. (Some stream chunkers append newlines.)
        let raw = "  {\"k\":1}\n".to_string();
        assert_eq!(
            normalize_native_tool_call_arguments("tool", raw.clone()),
            raw
        );
    }
}
