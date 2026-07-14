pub(crate) fn non_empty_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .map(ToString::to_string)
}

/// Normalize native tool-call `arguments` for OpenAI-format wire export.
///
/// Providers require each `function.arguments` field to be a parseable JSON
/// value string. Malformed model output is replaced with `{}` so the outbound
/// request is not rejected with HTTP 400.
pub(crate) fn normalize_native_tool_arguments(raw: &str, function_name: &str) -> String {
    let arguments = if raw.trim().is_empty() {
        "{}".to_string()
    } else {
        raw.to_string()
    };

    if serde_json::from_str::<serde_json::Value>(&arguments).is_ok() {
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
        "Invalid JSON in native tool-call arguments, using empty object"
    );
    "{}".to_string()
}

#[cfg(test)]
mod tests {
    use super::normalize_native_tool_arguments;

    #[test]
    fn normalize_native_tool_arguments_empty_to_object() {
        assert_eq!(normalize_native_tool_arguments("", "shell"), "{}");
        assert_eq!(normalize_native_tool_arguments("   ", "shell"), "{}");
    }

    #[test]
    fn normalize_native_tool_arguments_valid_object_passthrough() {
        let raw = r#"{"path":"foo.txt"}"#;
        assert_eq!(
            normalize_native_tool_arguments(raw, "file_read"),
            raw.to_string()
        );
    }

    #[test]
    fn normalize_native_tool_arguments_valid_array_passthrough() {
        let raw = r#"[1,2]"#;
        assert_eq!(normalize_native_tool_arguments(raw, "shell"), raw.to_string());
    }

    #[test]
    fn normalize_native_tool_arguments_malformed_to_empty_object() {
        assert_eq!(
            normalize_native_tool_arguments(r#"{"path": "unclosed"#, "file_write"),
            "{}"
        );
        assert_eq!(
            normalize_native_tool_arguments("not json at all", "shell"),
            "{}"
        );
    }
}
