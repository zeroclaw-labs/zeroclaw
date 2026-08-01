pub(crate) fn non_empty_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .map(ToString::to_string)
}
