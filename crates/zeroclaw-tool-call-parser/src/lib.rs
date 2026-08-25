//! Tool call parsing for LLM responses.

use regex::Regex;
use std::{collections::HashSet, sync::LazyLock};
pub use zeroclaw_api::model_provider::strip_think_tags;

/// A single parsed tool call extracted from LLM output.
#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub tool_call_id: Option<String>,
}

/// Internal tool protocol envelope variants that must not be treated as
/// user-visible channel text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolProtocolEnvelopeKind {
    ToolCalls,
    ToolCallsAlias,
    FunctionCall,
    ToolResult,
    ResponsesFunctionCall,
    TaggedToolCall,
}

fn parse_arguments_value(raw: Option<&serde_json::Value>) -> serde_json::Value {
    let initial = match raw {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new())),
        Some(value) => value.clone(),
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    unwrap_nested_json_strings(initial)
}

/// Canonical vocabulary of terminal markers emitted by providers that must be
/// stripped from final response text. This is the single source of truth for
/// the marker vocabulary, shared by the non-streaming
/// [`strip_trailing_terminal_markers`] helper and the streaming state machine
/// (`zeroclaw-runtime`'s `StreamTerminalMarkerStripper`) so a vocabulary or
/// matching-rule change cannot drift one path.
///
/// Order matters: longer spellings must precede shorter ones so a suffix check
/// matches the most specific marker first.
pub const TERMINAL_MARKERS: [&str; 2] = ["<|eom|>", "<eom>"];

/// Strip trailing terminal markers (`<eom>`, `<|eom|>`) from a response string.
/// Handles stacked markers with arbitrary whitespace between them.
///
/// Terminal markers are protocol metadata that should not appear in user-visible text.
/// This function iteratively removes trailing markers and whitespace until none remain.
///
/// # Examples
///
/// ```
/// use zeroclaw_tool_call_parser::strip_trailing_terminal_markers;
///
/// assert_eq!(strip_trailing_terminal_markers("Summary<eom>"), "Summary");
/// assert_eq!(strip_trailing_terminal_markers("Summary<|eom|>"), "Summary");
/// assert_eq!(strip_trailing_terminal_markers("Summary<eom><|eom|>"), "Summary");
/// assert_eq!(strip_trailing_terminal_markers("Summary<eom>  \n"), "Summary");
/// assert_eq!(strip_trailing_terminal_markers("Summary<eom>           <|eom|>"), "Summary");
/// assert_eq!(strip_trailing_terminal_markers("Text with <eom> inline"), "Text with <eom> inline");
/// ```
pub fn strip_trailing_terminal_markers(text: &str) -> String {
    let mut result = text.to_string();

    loop {
        // Look for a recognized marker at the end of the trailing-whitespace-
        // trimmed tail. When the trimmed tail ends in a marker, remove the
        // marker AND the whitespace that followed it (the marker suffix).
        // Whitespace BEFORE the marker belongs to the response text and is
        // preserved — this matches the streaming stripper, which keeps e.g.
        // `"Answer\n<eom>"` as `"Answer\n"`. If no marker is found, unmarked
        // trailing whitespace (e.g. `"Answer\n"`) is ordinary text and is
        // preserved too, so the two paths share one policy.
        let trimmed = result.trim_end();
        let mut stripped = false;
        for marker in TERMINAL_MARKERS {
            if let Some(prefix) = trimmed.strip_suffix(marker) {
                result = prefix.to_string();
                stripped = true;
                break;
            }
        }
        if !stripped {
            break;
        }
    }

    result
}

/// Recursively unwrap stringified JSON objects/arrays nested inside tool arguments.
/// Why: Gemini (and some other model_providers) sometimes double-encode nested object/array
/// parameters as JSON strings inside the outer arguments payload, which breaks tools
/// that expect `Value::Object` / `Value::Array` at those positions.
fn unwrap_nested_json_strings(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k, unwrap_nested_json_strings(v));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(unwrap_nested_json_strings).collect())
        }
        serde_json::Value::String(s) => {
            let trimmed = s.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                match serde_json::from_str::<serde_json::Value>(&s) {
                    Ok(parsed) => unwrap_nested_json_strings(parsed),
                    Err(_) => serde_json::Value::String(s),
                }
            } else {
                serde_json::Value::String(s)
            }
        }
        other => other,
    }
}

fn parse_tool_call_id(
    root: &serde_json::Value,
    function: Option<&serde_json::Value>,
) -> Option<String> {
    function
        .and_then(|func| func.get("id"))
        .or_else(|| root.get("id"))
        .or_else(|| root.get("tool_call_id"))
        .or_else(|| root.get("call_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
}

pub fn canonicalize_json_for_tool_signature(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort_unstable();
            let mut ordered = serde_json::Map::new();
            for key in keys {
                if let Some(child) = map.get(&key) {
                    ordered.insert(key, canonicalize_json_for_tool_signature(child));
                }
            }
            serde_json::Value::Object(ordered)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(canonicalize_json_for_tool_signature)
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn parse_tool_call_value(value: &serde_json::Value) -> Option<ParsedToolCall> {
    if let Some(function) = value.get("function") {
        let tool_call_id = parse_tool_call_id(value, Some(function));
        let raw_name = function
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let name = map_tool_name_alias(raw_name).to_string();
        if !name.is_empty() {
            let arguments = parse_arguments_value(
                function
                    .get("arguments")
                    .or_else(|| function.get("parameters")),
            );
            return Some(ParsedToolCall {
                name,
                arguments,
                tool_call_id,
            });
        }
    }

    let tool_call_id = parse_tool_call_id(value, None);
    let raw_name = value
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let name = map_tool_name_alias(raw_name).to_string();

    if name.is_empty() {
        return None;
    }

    let arguments =
        parse_arguments_value(value.get("arguments").or_else(|| value.get("parameters")));
    Some(ParsedToolCall {
        name,
        arguments,
        tool_call_id,
    })
}

fn parse_tool_calls_from_json_value(value: &serde_json::Value) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    if let Some(tool_calls) = value.get("tool_calls").and_then(|v| v.as_array()) {
        for call in tool_calls {
            if let Some(parsed) = parse_tool_call_value(call) {
                calls.push(parsed);
            }
        }

        if !calls.is_empty() {
            return calls;
        }
    }

    if let Some(array) = value.as_array() {
        for item in array {
            if let Some(parsed) = parse_tool_call_value(item) {
                calls.push(parsed);
            }
        }
        return calls;
    }

    if let Some(parsed) = parse_tool_call_value(value) {
        calls.push(parsed);
    }

    calls
}

fn has_non_empty_string(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|s| !s.trim().is_empty())
}

fn has_arguments_signal(value: &serde_json::Value) -> bool {
    value.get("arguments").is_some() || value.get("parameters").is_some()
}

fn looks_like_tool_call_object(value: &serde_json::Value) -> bool {
    if let Some(function) = value.get("function").and_then(serde_json::Value::as_object) {
        let function = serde_json::Value::Object(function.clone());
        return has_non_empty_string(&function, "name") && has_arguments_signal(&function);
    }

    has_non_empty_string(value, "name") && has_arguments_signal(value)
}

fn tool_call_array_has_protocol_shape(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty() && items.iter().any(looks_like_tool_call_object))
}

fn has_tool_protocol_object_signal(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };

    let has_args = has_arguments_signal(value);
    let has_call_id = has_non_empty_string(value, "id")
        || has_non_empty_string(value, "call_id")
        || has_non_empty_string(value, "tool_call_id");

    object
        .get("function")
        .and_then(serde_json::Value::as_object)
        .is_some()
        || (has_non_empty_string(value, "name") && has_args)
        || (has_args && has_call_id)
}

fn tool_call_array_has_malformed_protocol_signal(value: &serde_json::Value, key: &str) -> bool {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| !items.is_empty() && items.iter().any(has_tool_protocol_object_signal))
}

fn classify_tool_protocol_json_value(
    value: &serde_json::Value,
) -> Option<ToolProtocolEnvelopeKind> {
    if value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|ty| ty == "function_call")
        && has_non_empty_string(value, "name")
        && (has_arguments_signal(value) || has_non_empty_string(value, "call_id"))
    {
        return Some(ToolProtocolEnvelopeKind::ResponsesFunctionCall);
    }

    if tool_call_array_has_protocol_shape(value, "tool_calls") {
        return Some(ToolProtocolEnvelopeKind::ToolCalls);
    }

    if tool_call_array_has_protocol_shape(value, "toolcalls") {
        return Some(ToolProtocolEnvelopeKind::ToolCallsAlias);
    }

    if value
        .get("function_call")
        .is_some_and(looks_like_tool_call_object)
    {
        return Some(ToolProtocolEnvelopeKind::FunctionCall);
    }

    if has_non_empty_string(value, "tool_call_id")
        && (value.get("content").is_some()
            || value.get("result").is_some()
            || value.get("output").is_some())
    {
        return Some(ToolProtocolEnvelopeKind::ToolResult);
    }

    None
}

fn json_value_mentions_known_tool(
    value: &serde_json::Value,
    known_tool_names: &HashSet<String>,
) -> bool {
    if known_tool_names.is_empty() {
        return false;
    }

    let Some(object) = value.as_object() else {
        return value.as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| json_value_mentions_known_tool(item, known_tool_names))
        });
    };

    let name_matches = |candidate: Option<&serde_json::Value>| {
        candidate
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .is_some_and(|name| known_tool_names.contains(&name.to_ascii_lowercase()))
    };

    if name_matches(object.get("name")) {
        return true;
    }

    if let Some(function) = object
        .get("function")
        .and_then(serde_json::Value::as_object)
    {
        let function = serde_json::Value::Object(function.clone());
        if json_value_mentions_known_tool(&function, known_tool_names) {
            return true;
        }
    }

    if let Some(function_call) = object.get("function_call")
        && json_value_mentions_known_tool(function_call, known_tool_names)
    {
        return true;
    }

    ["tool_calls", "toolcalls"].iter().any(|key| {
        object
            .get(*key)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| json_value_mentions_known_tool(item, known_tool_names))
            })
    })
}

pub fn tool_protocol_envelope_mentions_known_tool(
    text: &str,
    known_tool_names: &HashSet<String>,
) -> bool {
    if known_tool_names.is_empty() {
        return false;
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if let Some(body) = json_fence_body(trimmed) {
        return tool_protocol_envelope_mentions_known_tool(body, known_tool_names);
    }

    if starts_with_tool_protocol_tag_or_fence(trimmed) || contains_tool_protocol_tag_marker(trimmed)
    {
        let (_, calls) = parse_tool_calls(trimmed);
        if calls
            .iter()
            .any(|call| known_tool_names.contains(&call.name.to_ascii_lowercase()))
        {
            return true;
        }
    }

    serde_json::from_str::<serde_json::Value>(trimmed)
        .is_ok_and(|value| json_value_mentions_known_tool(&value, known_tool_names))
}

fn has_malformed_tool_protocol_json_signal(value: &serde_json::Value) -> bool {
    // Empty `tool_calls: []` is a valid strict-provider compatibility case;
    // similar business JSON must also carry protocol-shaped fields before it
    // is withheld from user-visible output.
    tool_call_array_has_malformed_protocol_signal(value, "tool_calls")
        || tool_call_array_has_malformed_protocol_signal(value, "toolcalls")
        || value
            .get("function_call")
            .is_some_and(has_tool_protocol_object_signal)
        || (value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|ty| ty == "function_call")
            && (has_non_empty_string(value, "name")
                || has_non_empty_string(value, "call_id")
                || has_arguments_signal(value)))
        || (has_non_empty_string(value, "tool_call_id")
            && (value.get("content").is_some()
                || value.get("result").is_some()
                || value.get("output").is_some()))
}

fn starts_with_tool_protocol_tag_or_fence(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    lower.starts_with("<tool_call")
        || lower.starts_with("<toolcall")
        || lower.starts_with("<tool-call")
        // `<tools>` only, not a `<tools` prefix: the prefix would also swallow
        // `<toolsomething>`, and unlike the aliases above this tag has a second
        // legitimate meaning (the Hermes tool *declaration* block), so it is
        // matched exactly and left to the example-guard below.
        || lower.starts_with("<tools>")
        || lower.starts_with("<invoke")
        || lower.starts_with("<functioncall")
        || lower.starts_with("<function_call")
        || starts_with_tool_protocol_fence_lower(&lower)
        || lower.starts_with("[tool_call]")
}

fn starts_with_tool_protocol_fence(text: &str) -> bool {
    let lower = text.trim_start().to_ascii_lowercase();
    starts_with_tool_protocol_fence_lower(&lower)
}

fn starts_with_tool_protocol_fence_lower(lower: &str) -> bool {
    lower.starts_with("```tool_call")
        || lower.starts_with("```toolcall")
        || lower.starts_with("```tool-call")
        || lower.starts_with("```invoke")
        || starts_with_tool_name_fence_lower(lower)
}

fn starts_with_tool_name_fence_lower(lower: &str) -> bool {
    let Some(rest) = lower.strip_prefix("```tool") else {
        return false;
    };
    matches!(rest.chars().next(), Some(c) if c.is_whitespace() && c != '\n' && c != '\r')
}

fn contains_tool_protocol_tag_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("<tool_call")
        || lower.contains("<toolcall")
        || lower.contains("<tool-call")
        || lower.contains("<tools>")
        || lower.contains("<invoke")
        || lower.contains("<functioncall")
        || lower.contains("<function_call")
        || lower.contains("```tool_call")
        || lower.contains("```toolcall")
        || lower.contains("```tool-call")
        || lower.contains("```invoke")
        || lower.contains("```tool ")
        || lower.contains("[tool_call]")
}

pub fn looks_like_tool_protocol_example(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if let Some((body, visible_text)) = leading_json_fence_body_and_trailing_text(trimmed)
        && classify_tool_protocol_envelope(body).is_some()
        && has_example_context(visible_text)
    {
        return true;
    }

    if starts_with_tool_protocol_fence(trimmed) || contains_tool_protocol_tag_marker(trimmed) {
        let (visible_text, calls) = parse_tool_calls(trimmed);
        if !calls.is_empty() && has_example_context(&visible_text) {
            return true;
        }
    }

    false
}

fn has_example_context(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("example")
        || lower.contains("sample")
        || lower.contains("示例")
        // Common Chinese "for example" / "sample" markers. We keep this list
        // intentionally small to avoid accidentally exempting real protocol leaks.
        || lower.contains("例如")
        || lower.contains("比如")
        || lower.contains("举例")
        || lower.contains("例子")
        || lower.contains("比方说")
        || lower.contains("譬如")
}

fn leading_json_fence_body_and_trailing_text(trimmed: &str) -> Option<(&str, &str)> {
    let rest = trimmed.strip_prefix("```")?;
    let first_newline = rest.find('\n')?;
    let language = rest[..first_newline].trim().trim_end_matches('\r');
    if !language.eq_ignore_ascii_case("json") {
        return None;
    }

    let body_with_close = &rest[first_newline + 1..];
    let close_start = body_with_close.find("```")?;
    let body = body_with_close[..close_start].trim();
    let trailing = body_with_close[close_start + 3..].trim();
    (!body.is_empty() && !trailing.is_empty()).then_some((body, trailing))
}

pub fn contains_tool_protocol_tag_call(text: &str) -> bool {
    if !contains_tool_protocol_tag_marker(text) || looks_like_tool_protocol_example(text) {
        return false;
    }

    let (_, calls) = parse_tool_calls(text);
    !calls.is_empty()
}

fn classify_tagged_tool_protocol_envelope(text: &str) -> Option<ToolProtocolEnvelopeKind> {
    if !starts_with_tool_protocol_tag_or_fence(text) {
        return None;
    }
    if looks_like_tool_protocol_example(text) {
        return None;
    }

    let is_fence = starts_with_tool_protocol_fence(text);
    let (visible_text, calls) = parse_tool_calls(text);
    (!calls.is_empty() && (is_fence || visible_text.trim().is_empty()))
        .then_some(ToolProtocolEnvelopeKind::TaggedToolCall)
}

fn looks_like_malformed_tagged_tool_protocol_envelope(text: &str) -> bool {
    if !starts_with_tool_protocol_tag_or_fence(text) {
        return false;
    }
    if looks_like_tool_protocol_example(text) {
        return false;
    }

    let (visible_text, calls) = parse_tool_calls(text);
    if !calls.is_empty() || !visible_text.trim().is_empty() {
        return false;
    }

    let lower = text.to_ascii_lowercase();
    lower.contains("arguments")
        || lower.contains("parameters")
        || lower.contains("function")
        || lower.contains("name")
        || lower.contains("call_id")
        || lower.contains("tool_call_id")
}

/// JSON keys naming a tool-call container. Business JSON does not carry these.
const TOOL_PROTOCOL_CONTAINER_KEYS: [&str; 3] =
    ["\"tool_calls\"", "\"toolcalls\"", "\"function_call\""];

/// JSON keys carrying a tool call's correlation id.
const TOOL_PROTOCOL_CALL_ID_KEYS: [&str; 2] = ["\"call_id\"", "\"tool_call_id\""];

/// Every key that identifies a payload as tool protocol on its own.
fn tool_protocol_json_identifying_keys() -> impl Iterator<Item = &'static str> {
    TOOL_PROTOCOL_CONTAINER_KEYS
        .into_iter()
        .chain(TOOL_PROTOCOL_CALL_ID_KEYS)
}

fn has_malformed_tool_protocol_text_signal(text: &str) -> bool {
    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let json_like =
        trimmed.starts_with('{') || trimmed.starts_with('[') || lower.starts_with("```json");
    if !json_like {
        return false;
    }

    // Malformed text cannot be parsed into a Value, so keep the tool-result
    // signal close to the valid-envelope shape to avoid business JSON false positives.
    let has_tool_result_shape = text.contains("\"tool_call_id\"")
        && (text.contains("\"content\"")
            || text.contains("\"result\"")
            || text.contains("\"output\""));
    let has_protocol_container = TOOL_PROTOCOL_CONTAINER_KEYS
        .iter()
        .any(|key| text.contains(key));
    let has_arguments = text.contains("\"arguments\"") || text.contains("\"parameters\"");
    let has_call_id = TOOL_PROTOCOL_CALL_ID_KEYS
        .iter()
        .any(|key| text.contains(key));

    has_tool_result_shape || (has_protocol_container && has_arguments && has_call_id)
}

/// Whether `text` is a tool-protocol JSON payload that has not finished
/// arriving.
///
/// The completed-envelope classifiers need the whole value: they parse it, or
/// they look for a corroborating second key. A streaming consumer cannot wait
/// for either — the frame it is deciding about is on screen now, and the key
/// that gives the payload away may be the only one that has arrived. So this
/// deliberately trips on a *single* protocol key.
///
/// Being eager is the safe direction here and not in the completed case: a
/// held-back partial costs one frame and is re-rendered by the next delta,
/// whereas a rendered protocol envelope stays visible until the turn ends, or
/// indefinitely if the turn fails first. `false` for anything that already
/// parses as a complete JSON value, which the ordinary classifiers then judge
/// on their own terms.
pub fn looks_like_incomplete_tool_protocol_json(text: &str) -> bool {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    let json_like =
        trimmed.starts_with('{') || trimmed.starts_with('[') || lower.starts_with("```json");
    if !json_like {
        return false;
    }

    if let Some(body) = json_fence_body(trimmed) {
        return looks_like_incomplete_tool_protocol_json(body);
    }

    // A complete value is not this function's business.
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return false;
    }

    tool_protocol_json_identifying_keys().any(|key| trimmed.contains(key))
}

fn malformed_text_mentions_known_tool(text: &str, known_tool_names: &HashSet<String>) -> bool {
    if known_tool_names.is_empty() {
        return false;
    }

    static JSON_NAME_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""name"\s*:\s*"([^"]+)""#).expect("JSON_NAME_FIELD_RE regex must compile")
    });

    JSON_NAME_FIELD_RE.captures_iter(text).any(|cap| {
        cap.get(1)
            .map(|name| name.as_str().trim().to_ascii_lowercase())
            .is_some_and(|name| known_tool_names.contains(&name))
    })
}

fn has_malformed_tool_protocol_text_signal_for_known_tools(
    text: &str,
    known_tool_names: &HashSet<String>,
) -> bool {
    if has_malformed_tool_protocol_text_signal(text) {
        return true;
    }

    let trimmed = text.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    let json_like =
        trimmed.starts_with('{') || trimmed.starts_with('[') || lower.starts_with("```json");
    if !json_like {
        return false;
    }

    let has_protocol_container = text.contains("\"tool_calls\"")
        || text.contains("\"toolcalls\"")
        || text.contains("\"function_call\"");
    let has_arguments = text.contains("\"arguments\"") || text.contains("\"parameters\"");

    has_protocol_container
        && has_arguments
        && malformed_text_mentions_known_tool(text, known_tool_names)
}

fn json_fence_body(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("```")?;
    let first_newline = rest.find('\n')?;
    let language = rest[..first_newline].trim().trim_end_matches('\r');
    if !language.eq_ignore_ascii_case("json") {
        return None;
    }

    let body_with_close = &rest[first_newline + 1..];
    let close_start = body_with_close.rfind("```")?;
    if !body_with_close[close_start + 3..].trim().is_empty() {
        return None;
    }
    Some(body_with_close[..close_start].trim())
}

pub fn classify_tool_protocol_envelope(text: &str) -> Option<ToolProtocolEnvelopeKind> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(kind) = classify_tagged_tool_protocol_envelope(trimmed) {
        return Some(kind);
    }

    if let Some(body) = json_fence_body(trimmed) {
        return classify_tool_protocol_envelope(body);
    }

    let value = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    classify_tool_protocol_json_value(&value)
}

pub fn looks_like_tool_protocol_envelope(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    if classify_tool_protocol_envelope(trimmed).is_some() {
        return true;
    }

    if let Some(body) = json_fence_body(trimmed) {
        return looks_like_tool_protocol_envelope(body);
    }

    serde_json::from_str::<serde_json::Value>(trimmed)
        .is_ok_and(|value| has_malformed_tool_protocol_json_signal(&value))
}

pub fn looks_like_malformed_tool_protocol_envelope(text: &str) -> bool {
    let trimmed = text.trim();
    if looks_like_tool_protocol_example(trimmed) {
        return false;
    }

    if looks_like_malformed_tagged_tool_protocol_envelope(trimmed) {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    let json_like =
        trimmed.starts_with('{') || trimmed.starts_with('[') || lower.starts_with("```json");
    if trimmed.is_empty() || !json_like {
        return false;
    }

    if let Some(body) = json_fence_body(trimmed) {
        return looks_like_malformed_tool_protocol_envelope(body);
    }

    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return false;
    }

    has_malformed_tool_protocol_text_signal(trimmed)
}

pub fn looks_like_malformed_tool_protocol_envelope_for_known_tools(
    text: &str,
    known_tool_names: &HashSet<String>,
) -> bool {
    let trimmed = text.trim();
    if looks_like_tool_protocol_example(trimmed) {
        return false;
    }

    if looks_like_malformed_tool_protocol_envelope(trimmed) {
        return true;
    }

    let lower = trimmed.to_ascii_lowercase();
    let json_like =
        trimmed.starts_with('{') || trimmed.starts_with('[') || lower.starts_with("```json");
    if trimmed.is_empty() || !json_like {
        return false;
    }

    if let Some(body) = json_fence_body(trimmed) {
        return looks_like_malformed_tool_protocol_envelope_for_known_tools(body, known_tool_names);
    }

    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return false;
    }

    has_malformed_tool_protocol_text_signal_for_known_tools(trimmed, known_tool_names)
}

fn is_xml_meta_tag(tag: &str) -> bool {
    let normalized = tag.to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "tool_call"
            | "toolcall"
            | "tool-call"
            | "invoke"
            | "thinking"
            | "thought"
            | "analysis"
            | "reasoning"
            | "reflection"
    )
}

/// Match opening XML tags: `<tag_name>`.  Does NOT use backreferences.
static XML_OPEN_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<([a-zA-Z_][a-zA-Z0-9_-]*)>").expect("XML_OPEN_TAG_RE regex must compile")
});

/// MiniMax XML invoke format:
/// `<invoke name="shell"><parameter name="command">pwd</parameter></invoke>`
static MINIMAX_INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<invoke\b[^>]*\bname\s*=\s*(?:"([^"]+)"|'([^']+)')[^>]*>(.*?)</invoke>"#)
        .expect("MINIMAX_INVOKE_RE regex must compile")
});

static MINIMAX_PARAMETER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?is)<parameter\b[^>]*\bname\s*=\s*(?:"([^"]+)"|'([^']+)')[^>]*>(.*?)</parameter>"#,
    )
    .expect("MINIMAX_PARAMETER_RE regex must compile")
});

/// Extracts all `<tag>…</tag>` pairs from `input`, returning `(tag_name, inner_content)`.
/// Handles matching closing tags without regex backreferences.
fn extract_xml_pairs(input: &str) -> Vec<(&str, &str)> {
    let mut results = Vec::new();
    let mut search_start = 0;
    while let Some(open_cap) = XML_OPEN_TAG_RE.captures(&input[search_start..]) {
        let Some(full_open) = open_cap.get(0) else {
            break;
        };
        let Some(tag_name) = open_cap.get(1) else {
            search_start += full_open.end();
            continue;
        };
        let tag_name = tag_name.as_str();
        let open_end = search_start + full_open.end();

        let closing_tag = format!("</{tag_name}>");
        if let Some(close_pos) = input[open_end..].find(&closing_tag) {
            let inner = &input[open_end..open_end + close_pos];
            results.push((tag_name, inner.trim()));
            search_start = open_end + close_pos + closing_tag.len();
        } else {
            search_start = open_end;
        }
    }
    results
}

/// Parse XML-style tool calls in `<tool_call>` bodies.
/// Supports both nested argument tags and JSON argument payloads:
/// - `<memory_recall><query>...</query></memory_recall>`
/// - `<shell>{"command":"pwd"}</shell>`
fn parse_xml_tool_calls(xml_content: &str) -> Option<Vec<ParsedToolCall>> {
    let mut calls = Vec::new();
    let trimmed = xml_content.trim();

    if !trimmed.starts_with('<') || !trimmed.contains('>') {
        return None;
    }

    for (tool_name_str, inner_content) in extract_xml_pairs(trimmed) {
        let tool_name = tool_name_str.to_string();
        if is_xml_meta_tag(&tool_name) {
            continue;
        }

        if inner_content.is_empty() {
            continue;
        }

        let mut args = serde_json::Map::new();

        if let Some(first_json) = extract_json_values(inner_content).into_iter().next() {
            match first_json {
                serde_json::Value::Object(object_args) => {
                    args = object_args;
                }
                other => {
                    args.insert("value".to_string(), other);
                }
            }
        } else {
            for (key_str, value) in extract_xml_pairs(inner_content) {
                let key = key_str.to_string();
                if is_xml_meta_tag(&key) {
                    continue;
                }
                if !value.is_empty() {
                    args.insert(key, serde_json::Value::String(value.to_string()));
                }
            }

            if args.is_empty() {
                args.insert(
                    "content".to_string(),
                    serde_json::Value::String(inner_content.to_string()),
                );
            }
        }

        calls.push(ParsedToolCall {
            name: tool_name,
            arguments: serde_json::Value::Object(args),
            tool_call_id: None,
        });
    }

    if calls.is_empty() { None } else { Some(calls) }
}

/// Parse MiniMax-style XML tool calls with attributed invoke/parameter tags.
fn parse_minimax_invoke_calls(response: &str) -> Option<(String, Vec<ParsedToolCall>)> {
    let mut calls = Vec::new();
    let mut text_parts = Vec::new();
    let mut last_end = 0usize;

    for cap in MINIMAX_INVOKE_RE.captures_iter(response) {
        let Some(full_match) = cap.get(0) else {
            continue;
        };

        let before = response[last_end..full_match.start()].trim();
        if !before.is_empty() {
            text_parts.push(before.to_string());
        }

        let name = cap
            .get(1)
            .or_else(|| cap.get(2))
            .map(|m| m.as_str().trim())
            .filter(|v| !v.is_empty());
        let body = cap.get(3).map(|m| m.as_str()).unwrap_or("").trim();
        last_end = full_match.end();

        let Some(name) = name else {
            continue;
        };

        let mut args = serde_json::Map::new();
        for param_cap in MINIMAX_PARAMETER_RE.captures_iter(body) {
            let key = param_cap
                .get(1)
                .or_else(|| param_cap.get(2))
                .map(|m| m.as_str().trim())
                .unwrap_or_default();
            if key.is_empty() {
                continue;
            }
            let value = param_cap
                .get(3)
                .map(|m| m.as_str().trim())
                .unwrap_or_default();
            if value.is_empty() {
                continue;
            }

            let parsed = extract_json_values(value).into_iter().next();
            args.insert(
                key.to_string(),
                parsed.unwrap_or_else(|| serde_json::Value::String(value.to_string())),
            );
        }

        if args.is_empty() {
            if let Some(first_json) = extract_json_values(body).into_iter().next() {
                match first_json {
                    serde_json::Value::Object(obj) => args = obj,
                    other => {
                        args.insert("value".to_string(), other);
                    }
                }
            } else if !body.is_empty() {
                args.insert(
                    "content".to_string(),
                    serde_json::Value::String(body.to_string()),
                );
            }
        }

        calls.push(ParsedToolCall {
            name: name.to_string(),
            arguments: serde_json::Value::Object(args),
            tool_call_id: None,
        });
    }

    if calls.is_empty() {
        return None;
    }

    let after = response[last_end..].trim();
    if !after.is_empty() {
        text_parts.push(after.to_string());
    }

    let text = text_parts
        .join("\n")
        .replace("<minimax:tool_call>", "")
        .replace("</minimax:tool_call>", "")
        .replace("<minimax:toolcall>", "")
        .replace("</minimax:toolcall>", "")
        .trim()
        .to_string();

    Some((text, calls))
}

const TOOL_CALL_OPEN_TAGS: [&str; 8] = [
    "<tool_call>",
    "<tool_calls>",
    "<toolcall>",
    "<tool-call>",
    // Hermes-family models sometimes emit the tool *declaration* tag around an
    // invocation. Qwen2.5-Coder-32B does this deterministically: the payload is
    // well-formed Hermes JSON, only the wrapper is wrong.
    "<tools>",
    "<invoke>",
    "<minimax:tool_call>",
    "<minimax:toolcall>",
];

const TOOL_CALL_CLOSE_TAGS: [&str; 8] = [
    "</tool_call>",
    "</tool_calls>",
    "</toolcall>",
    "</tool-call>",
    "</tools>",
    "</invoke>",
    "</minimax:tool_call>",
    "</minimax:toolcall>",
];

fn find_first_tag<'a>(haystack: &str, tags: &'a [&'a str]) -> Option<(usize, &'a str)> {
    tags.iter()
        .filter_map(|tag| haystack.find(tag).map(|idx| (idx, *tag)))
        .min_by_key(|(idx, _)| *idx)
}

fn extract_first_json_value_with_end(input: &str) -> Option<(serde_json::Value, usize)> {
    let trimmed = input.trim_start();
    let trim_offset = input.len().saturating_sub(trimmed.len());

    for (byte_idx, ch) in trimmed.char_indices() {
        if ch != '{' && ch != '[' {
            continue;
        }

        let slice = &trimmed[byte_idx..];
        let mut stream = serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
        if let Some(Ok(value)) = stream.next() {
            let consumed = stream.byte_offset();
            if consumed > 0 {
                return Some((value, trim_offset + byte_idx + consumed));
            }
        }
    }

    None
}

fn strip_leading_close_tags(mut input: &str) -> &str {
    loop {
        let trimmed = input.trim_start();
        if !trimmed.starts_with("</") {
            return trimmed;
        }

        let Some(close_end) = trimmed.find('>') else {
            return "";
        };
        input = &trimmed[close_end + 1..];
    }
}

fn extract_json_values(input: &str) -> Vec<serde_json::Value> {
    let mut values = Vec::new();
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return values;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        values.push(value);
        return values;
    }

    let char_positions: Vec<(usize, char)> = trimmed.char_indices().collect();
    let mut idx = 0;
    while idx < char_positions.len() {
        let (byte_idx, ch) = char_positions[idx];
        if ch == '{' || ch == '[' {
            let slice = &trimmed[byte_idx..];
            let mut stream =
                serde_json::Deserializer::from_str(slice).into_iter::<serde_json::Value>();
            if let Some(Ok(value)) = stream.next() {
                let consumed = stream.byte_offset();
                if consumed > 0 {
                    values.push(value);
                    let next_byte = byte_idx + consumed;
                    while idx < char_positions.len() && char_positions[idx].0 < next_byte {
                        idx += 1;
                    }
                    continue;
                }
            }
        }
        idx += 1;
    }

    values
}

fn skip_json_ws(input: &str, mut idx: usize) -> usize {
    while let Some(ch) = input[idx..].chars().next() {
        if !ch.is_whitespace() {
            break;
        }
        idx += ch.len_utf8();
    }
    idx
}

fn find_json_field_value_start(input: &str, field: &str, start: usize) -> Option<usize> {
    let pattern = format!("\"{field}\"");
    let mut search_start = start;
    while let Some(relative) = input[search_start..].find(&pattern) {
        let key_start = search_start + relative;
        let after_key = key_start + pattern.len();
        let colon = skip_json_ws(input, after_key);
        if input[colon..].starts_with(':') {
            return Some(colon + 1);
        }
        search_start = after_key;
    }
    None
}

fn find_json_string_end(input: &str, quote_start: usize) -> Option<usize> {
    if !input[quote_start..].starts_with('"') {
        return None;
    }

    let mut escaped = false;
    for (relative, ch) in input[quote_start + 1..].char_indices() {
        let idx = quote_start + 1 + relative;
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return Some(idx),
            _ => {}
        }
    }

    None
}

fn parse_json_string_field_after(
    input: &str,
    field: &str,
    start: usize,
) -> Option<(String, usize)> {
    let value_start = skip_json_ws(input, find_json_field_value_start(input, field, start)?);
    let value_end = find_json_string_end(input, value_start)?;
    let value = serde_json::from_str::<String>(&input[value_start..=value_end]).ok()?;
    Some((value, value_end + 1))
}

// Narrow recovery for malformed file_write calls whose content string contains
// model-emitted unescaped quotes. This is deliberately not a general JSON
// repair path: content must be the final argument field and the remaining tail
// must only close the surrounding tool-call protocol envelope.
fn decode_recovered_json_string_fragment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }

        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('b') => out.push('\u{0008}'),
            Some('f') => out.push('\u{000c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let mut value = 0u32;
                let mut valid = true;
                let mut consumed = String::with_capacity(4);
                for _ in 0..4 {
                    let Some(hex) = chars.next() else {
                        valid = false;
                        break;
                    };
                    consumed.push(hex);
                    if let Some(digit) = hex.to_digit(16) {
                        value = (value << 4) | digit;
                    } else {
                        valid = false;
                    }
                }
                if valid && consumed.len() == 4 {
                    if let Some(decoded) = char::from_u32(value) {
                        out.push(decoded);
                    } else {
                        out.push_str("\\u");
                        out.push_str(&consumed);
                    }
                } else {
                    out.push_str("\\u");
                    out.push_str(&consumed);
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }

    out
}

fn file_write_content_tail_is_unambiguous(input: &str, after_quote: usize) -> bool {
    let mut idx = skip_json_ws(input, after_quote);
    if !input[idx..].starts_with('}') {
        return false;
    }
    idx += '}'.len_utf8();
    idx = skip_json_ws(input, idx);

    while let Some(ch) = input[idx..].chars().next() {
        match ch {
            '}' | ']' => {
                idx += ch.len_utf8();
                idx = skip_json_ws(input, idx);
            }
            _ => break,
        }
    }

    let tail = input[idx..].trim_start();
    tail.is_empty()
        || tail.starts_with("</tool_call>")
        || tail.starts_with("</tool_calls>")
        || tail.starts_with("</tools>")
        || tail.starts_with("</toolcall>")
        || tail.starts_with("</tool-call>")
        || tail.starts_with("</invoke>")
        || tail.starts_with("</minimax:tool_call>")
        || tail.starts_with("</minimax:toolcall>")
        || tail.starts_with("```")
}

fn file_write_content_quote_starts_additional_final_field(input: &str, after_quote: usize) -> bool {
    let mut idx = skip_json_ws(input, after_quote);
    if !input[idx..].starts_with(',') {
        return false;
    }

    idx += ','.len_utf8();
    idx = skip_json_ws(input, idx);

    let Some(field_end) = find_json_string_end(input, idx) else {
        return false;
    };

    idx = skip_json_ws(input, field_end + 1);
    if !input[idx..].starts_with(':') {
        return false;
    }

    idx += ':'.len_utf8();
    idx = skip_json_ws(input, idx);

    let mut stream =
        serde_json::Deserializer::from_str(&input[idx..]).into_iter::<serde_json::Value>();
    let Some(Ok(_)) = stream.next() else {
        return false;
    };

    let consumed = stream.byte_offset();
    consumed > 0 && file_write_content_tail_is_unambiguous(input, idx + consumed)
}

fn parse_malformed_file_write_content_after(input: &str, start: usize) -> Option<String> {
    let value_start = skip_json_ws(input, find_json_field_value_start(input, "content", start)?);
    if !input[value_start..].starts_with('"') {
        return None;
    }

    let mut escaped = false;
    for (relative, ch) in input[value_start + 1..].char_indices() {
        let idx = value_start + 1 + relative;
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' if file_write_content_tail_is_unambiguous(input, idx + 1) => {
                let raw = &input[value_start + 1..idx];
                return Some(decode_recovered_json_string_fragment(raw));
            }
            '"' if file_write_content_quote_starts_additional_final_field(input, idx + 1) => {
                return None;
            }
            '"' => {}
            _ => {}
        }
    }

    None
}

fn parse_malformed_file_write_arguments(input: &str) -> Option<serde_json::Value> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let object_start = skip_json_ws(trimmed, 0);
    if !trimmed[object_start..].starts_with('{') {
        return None;
    }

    let (path, path_end) = parse_json_string_field_after(trimmed, "path", object_start)?;
    if path.trim().is_empty() {
        return None;
    }

    let content = parse_malformed_file_write_content_after(trimmed, path_end)?;
    Some(serde_json::json!({
        "path": path,
        "content": content,
    }))
}

fn parse_malformed_file_write_call(input: &str) -> Option<ParsedToolCall> {
    let trimmed = input.trim();
    let body = json_fence_body(trimmed).unwrap_or(trimmed).trim();
    if body.is_empty() || !(body.starts_with('{') || body.starts_with('[')) {
        return None;
    }

    let (name, name_end) = parse_json_string_field_after(body, "name", 0)?;
    if map_tool_name_alias(name.trim()) != "file_write" {
        return None;
    }

    let arguments_start = find_json_field_value_start(body, "arguments", name_end)
        .or_else(|| find_json_field_value_start(body, "parameters", name_end))?;
    let arguments = parse_malformed_file_write_arguments(&body[arguments_start..])?;

    Some(ParsedToolCall {
        name: "file_write".to_string(),
        arguments,
        tool_call_id: None,
    })
}

/// Find the end position of a JSON object by tracking balanced braces.
fn find_json_end(input: &str) -> Option<usize> {
    let trimmed = input.trim_start();
    let offset = input.len() - trimmed.len();

    if !trimmed.starts_with('{') {
        return None;
    }

    let mut depth = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in trimmed.char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }

        match ch {
            '\\' if in_string => escape_next = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + i + ch.len_utf8());
                }
            }
            _ => {}
        }
    }

    None
}

fn parse_xml_attribute_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find <invoke name="toolname">...</invoke> blocks
    static INVOKE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s)<invoke\s+name="([^"]+)"[^>]*>(.*?)</invoke>"#)
            .expect("INVOKE_RE regex must compile")
    });

    // Regex to find <parameter name="paramname">value</parameter>
    static PARAM_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"<parameter\s+name="([^"]+)"[^>]*>([^<]*)</parameter>"#)
            .expect("PARAM_RE regex must compile")
    });

    for cap in INVOKE_RE.captures_iter(response) {
        let tool_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let inner = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        let mut arguments = serde_json::Map::new();

        for param_cap in PARAM_RE.captures_iter(inner) {
            let param_name = param_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let param_value = param_cap.get(2).map(|m| m.as_str()).unwrap_or("");

            if !param_name.is_empty() {
                arguments.insert(
                    param_name.to_string(),
                    serde_json::Value::String(param_value.to_string()),
                );
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

fn parse_perl_style_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find TOOL_CALL blocks - handle double closing braces }}
    // Matches both `TOOL_CALL { ... }} /TOOL_CALL` and `[TOOL_CALL]{ ... }}[/TOOL_CALL]`
    static PERL_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)(?:\[TOOL_CALL\]|TOOL_CALL)\s*\{(.+?)\}\}\s*(?:\[/TOOL_CALL\]|/TOOL_CALL)")
            .expect("PERL_RE regex must compile")
    });

    // Regex to find tool => "name" in the content
    static TOOL_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"tool\s*=>\s*"([^"]+)""#).expect("TOOL_NAME_RE regex must compile")
    });

    // Regex to find args => { ... } block.
    // The closing brace is optional: in the square bracket variant [TOOL_CALL]{...}}[/TOOL_CALL]
    // the outer regex may consume the inner closing brace, so the args content may run to end of string.
    static ARGS_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)args\s*=>\s*\{(.+?)(?:\}|$)").expect("ARGS_BLOCK_RE regex must compile")
    });

    // Regex to find --key "value" pairs
    static ARGS_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"--(\w+)\s+"([^"]+)""#).expect("ARGS_RE regex must compile"));

    for cap in PERL_RE.captures_iter(response) {
        let content = cap.get(1).map(|m| m.as_str()).unwrap_or("");

        // Extract tool name
        let tool_name = TOOL_NAME_RE
            .captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        // Extract args block
        let args_block = ARGS_BLOCK_RE
            .captures(content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        let mut arguments = serde_json::Map::new();

        for arg_cap in ARGS_RE.captures_iter(args_block) {
            let key = arg_cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let value = arg_cap.get(2).map(|m| m.as_str()).unwrap_or("");

            if !key.is_empty() {
                arguments.insert(
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                );
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

fn parse_function_call_tool_calls(response: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();

    // Regex to find <FunctionCall> blocks
    static FUNC_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<FunctionCall>\s*(\w+)\s*<code>([^<]+)</code>\s*</FunctionCall>")
            .expect("FUNC_RE regex must compile")
    });

    for cap in FUNC_RE.captures_iter(response) {
        let tool_name = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let args_text = cap.get(2).map(|m| m.as_str()).unwrap_or("");

        if tool_name.is_empty() {
            continue;
        }

        // Parse key>value pairs (e.g., path>/Users/.../file.txt)
        let mut arguments = serde_json::Map::new();
        for line in args_text.lines() {
            let line = line.trim();
            if let Some(pos) = line.find('>') {
                let key = line[..pos].trim();
                let value = line[pos + 1..].trim();
                if !key.is_empty() && !value.is_empty() {
                    arguments.insert(
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
            }
        }

        if !arguments.is_empty() {
            calls.push(ParsedToolCall {
                name: map_tool_name_alias(tool_name).to_string(),
                arguments: serde_json::Value::Object(arguments),
                tool_call_id: None,
            });
        }
    }

    calls
}

/// Parse GLM-style tool calls from response text.
/// Map tool name aliases from various LLM model_providers to ZeroClaw tool names.
/// This handles variations like "fileread" -> "file_read", "bash" -> "shell", etc.
fn map_tool_name_alias(tool_name: &str) -> &str {
    let tool_name = tool_name
        .rsplit_once('.')
        .map(|(_, suffix)| suffix)
        .unwrap_or(tool_name);
    match tool_name {
        // Shell variations (including GLM aliases that map to shell)
        "shell" | "bash" | "sh" | "exec" | "command" | "cmd" | "browser_open" | "browser"
        | "web_search" => "shell",
        // Messaging variations
        "send_message" | "sendmessage" => "message_send",
        // File tool variations
        "fileread" | "file_read" | "readfile" | "read_file" | "file" => "file_read",
        "filewrite" | "file_write" | "writefile" | "write_file" => "file_write",
        "filelist" | "file_list" | "listfiles" | "list_files" => "file_list",
        // Memory variations
        "memoryrecall" | "memory_recall" | "recall" | "memrecall" => "memory_recall",
        "memorystore" | "memory_store" | "store" | "memstore" => "memory_store",
        "memoryforget" | "memory_forget" | "forget" | "memforget" => "memory_forget",
        // HTTP variations
        "http_request" | "http" | "fetch" | "curl" | "wget" => "http_request",
        _ => tool_name,
    }
}

fn build_curl_command(url: &str) -> Option<String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return None;
    }

    if url.chars().any(char::is_whitespace) {
        return None;
    }

    let escaped = url.replace('\'', r#"'"'"'"#);
    Some(format!("curl -s '{}'", escaped))
}

fn parse_glm_style_tool_calls(text: &str) -> Vec<(String, serde_json::Value, Option<String>)> {
    let mut calls = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: tool_name/param>value or tool_name/{json}
        if let Some(pos) = line.find('/') {
            let tool_part = &line[..pos];
            let rest = &line[pos + 1..];

            if tool_part.chars().all(|c| c.is_alphanumeric() || c == '_') {
                let tool_name = map_tool_name_alias(tool_part);

                if let Some(gt_pos) = rest.find('>') {
                    let param_name = rest[..gt_pos].trim();
                    let value = rest[gt_pos + 1..].trim();

                    let arguments = match tool_name {
                        "shell" => {
                            if param_name == "url" {
                                let Some(command) = build_curl_command(value) else {
                                    continue;
                                };
                                serde_json::json!({ "command": command })
                            } else if value.starts_with("http://") || value.starts_with("https://")
                            {
                                if let Some(command) = build_curl_command(value) {
                                    serde_json::json!({ "command": command })
                                } else {
                                    serde_json::json!({ "command": value })
                                }
                            } else {
                                serde_json::json!({ "command": value })
                            }
                        }
                        "http_request" => {
                            serde_json::json!({"url": value, "method": "GET"})
                        }
                        _ => serde_json::json!({ param_name: value }),
                    };

                    calls.push((tool_name.to_string(), arguments, Some(line.to_string())));
                    continue;
                }

                if rest.starts_with('{')
                    && let Ok(json_args) = serde_json::from_str::<serde_json::Value>(rest)
                {
                    calls.push((tool_name.to_string(), json_args, Some(line.to_string())));
                }
            }
        }
    }

    calls
}

fn default_param_for_tool(tool: &str) -> &'static str {
    match tool {
        "shell" | "bash" | "sh" | "exec" | "command" | "cmd" => "command",
        // All file tools default to "path"
        "file_read" | "fileread" | "readfile" | "read_file" | "file" | "file_write"
        | "filewrite" | "writefile" | "write_file" | "file_edit" | "fileedit" | "editfile"
        | "edit_file" | "file_list" | "filelist" | "listfiles" | "list_files" => "path",
        // Memory recall/forget and web search tools all default to "query"
        "memory_recall" | "memoryrecall" | "recall" | "memrecall" | "memory_forget"
        | "memoryforget" | "forget" | "memforget" | "web_search_tool" | "web_search"
        | "websearch" | "search" => "query",
        "memory_store" | "memorystore" | "store" | "memstore" => "content",
        // HTTP and browser tools default to "url"
        "http_request" | "http" | "fetch" | "curl" | "wget" | "browser_open" | "browser" => "url",
        _ => "input",
    }
}

fn parse_glm_shortened_body(body: &str) -> Option<ParsedToolCall> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let function_style = body.find('(').and_then(|open| {
        if body.ends_with(')') && open > 0 {
            Some((body[..open].trim(), body[open + 1..body.len() - 1].trim()))
        } else {
            None
        }
    });

    // Check attribute-style FIRST: `tool_name key="value" />`
    // Must come before `>` check because `/>` contains `>` and would
    // misparse the tool name in the first branch.
    let (tool_raw, value_part) = if let Some((tool, args)) = function_style {
        (tool, args)
    } else if body.contains("=\"") {
        // Attribute-style: split at first whitespace to get tool name
        let split_pos = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
        let tool = body[..split_pos].trim();
        let attrs = body[split_pos..]
            .trim()
            .trim_end_matches("/>")
            .trim_end_matches('>')
            .trim_end_matches('/')
            .trim();
        (tool, attrs)
    } else {
        let gt_pos = body.find('>')?;
        // GLM shortened: `tool_name>value`
        let tool = body[..gt_pos].trim();
        let value = body[gt_pos + 1..].trim();
        // Strip trailing self-close markers that some models emit
        let value = value.trim_end_matches("/>").trim_end_matches('/').trim();
        (tool, value)
    };

    // Validate tool name: must be alphanumeric + underscore only
    let tool_raw = tool_raw.trim_end_matches(|c: char| c.is_whitespace());
    if tool_raw.is_empty() || !tool_raw.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let tool_name = map_tool_name_alias(tool_raw);

    // Try attribute-style: `key="value" key2="value2"`
    if value_part.contains("=\"") {
        let mut args = serde_json::Map::new();
        // Simple attribute parser: key="value" pairs
        let mut rest = value_part;
        while let Some(eq_pos) = rest.find("=\"") {
            let key_start = rest[..eq_pos]
                .rfind(|c: char| c.is_whitespace())
                .map(|p| p + 1)
                .unwrap_or(0);
            let key = rest[key_start..eq_pos]
                .trim()
                .trim_matches(|c: char| c == ',' || c == ';');
            let after_quote = &rest[eq_pos + 2..];
            if let Some(end_quote) = after_quote.find('"') {
                let value = &after_quote[..end_quote];
                if !key.is_empty() {
                    args.insert(
                        key.to_string(),
                        serde_json::Value::String(value.to_string()),
                    );
                }
                rest = &after_quote[end_quote + 1..];
            } else {
                break;
            }
        }
        if !args.is_empty() {
            return Some(ParsedToolCall {
                name: tool_name.to_string(),
                arguments: serde_json::Value::Object(args),
                tool_call_id: None,
            });
        }
    }

    // Try YAML-style multi-line: each line is `key: value`
    if value_part.contains('\n') {
        let mut args = serde_json::Map::new();
        for line in value_part.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(colon_pos) = line.find(':') {
                let key = line[..colon_pos].trim();
                let value = line[colon_pos + 1..].trim();
                if !key.is_empty() && !value.is_empty() {
                    // Normalize boolean-like values
                    let json_value = match value {
                        "true" | "yes" => serde_json::Value::Bool(true),
                        "false" | "no" => serde_json::Value::Bool(false),
                        _ => serde_json::Value::String(value.to_string()),
                    };
                    args.insert(key.to_string(), json_value);
                }
            }
        }
        if !args.is_empty() {
            return Some(ParsedToolCall {
                name: tool_name.to_string(),
                arguments: serde_json::Value::Object(args),
                tool_call_id: None,
            });
        }
    }

    // Single-value shortened: `tool>value`
    if !value_part.is_empty() {
        let param = default_param_for_tool(tool_raw);
        let arguments = match tool_name {
            "shell" => {
                if value_part.starts_with("http://") || value_part.starts_with("https://") {
                    if let Some(cmd) = build_curl_command(value_part) {
                        serde_json::json!({ "command": cmd })
                    } else {
                        serde_json::json!({ "command": value_part })
                    }
                } else {
                    serde_json::json!({ "command": value_part })
                }
            }
            "http_request" => serde_json::json!({"url": value_part, "method": "GET"}),
            _ => serde_json::json!({ param: value_part }),
        };
        return Some(ParsedToolCall {
            name: tool_name.to_string(),
            arguments,
            tool_call_id: None,
        });
    }

    None
}

fn malformed_tool_block_event(payload_len: usize) -> ::zeroclaw_log::Event {
    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
        .with_attrs(::serde_json::json!({
            "payload_len": payload_len,
        }))
}

/// Is this JSON value an *invocation* rather than a tool *declaration*?
///
/// Only consulted for the `<tools>` wrapper, which is overloaded: in the Hermes
/// prompt format `<tools>` DECLARES the available tools, while `<tool_call>`
/// invokes one. Every other alias in [`TOOL_CALL_OPEN_TAGS`] has exactly one
/// meaning, so this narrowing does not apply to them.
///
/// The distinction cannot be left to [`has_arguments_signal`], which counts
/// `parameters` as an arguments marker: a declaration carries `name` +
/// `parameters` and is therefore indistinguishable from a call by that test.
/// A declaration is rejected here on three signals a real invocation never has
/// -- a JSON array of entries, a `description`, or a `parameters` schema in
/// place of concrete `arguments`.
fn looks_like_tools_wrapper_invocation(value: &serde_json::Value) -> bool {
    // A declaration block is an ARRAY of tool schemas. An invocation is one call.
    let serde_json::Value::Object(map) = value else {
        return false;
    };
    // `description` and `parameters` describe a tool; they never appear in a call.
    if map.contains_key("description") || map.contains_key("parameters") {
        return false;
    }
    // OpenAI-shaped `{"type":"function","function":{...}}` declarations.
    if let Some(inner) = map.get("function").and_then(serde_json::Value::as_object)
        && (inner.contains_key("description") || inner.contains_key("parameters"))
    {
        return false;
    }
    // Only `arguments` is admitted. `args` was accepted here previously, but the
    // canonical parser reads `arguments`/`parameters` and never `args`, so an
    // `args`-only body was admitted as an invocation and then dispatched with
    // EMPTY arguments -- a corrupted call rather than an inert one. Admitting a
    // shape the parser cannot honour is worse than not admitting it: if a model
    // is found to emit `args`, implement it in the canonical parser first.
    //
    // THE ADMITTED VALUE MUST BE THE EXECUTED VALUE. `parse_tool_calls_from_json_value`
    // gives precedence to a nested `function` object, and `tool_calls` can expand one
    // value into several. A body carrying BOTH a benign top level and a nested
    // envelope therefore passed this predicate on the top level while dispatching the
    // nested content:
    //
    //   {"name":"benign","arguments":{},
    //    "function":{"name":"shell","arguments":{"command":"rm -rf /tmp/x"}}}
    //
    // The discriminator has to authorize the same representation that crosses the
    // parser boundary, so an envelope member disqualifies the body outright rather
    // than being validated on a level the parser will ignore.
    if map.contains_key("function") || map.contains_key("tool_calls") {
        return false;
    }
    has_non_empty_string(value, "name") && map.contains_key("arguments")
}

/// The extent of one `<tools>` span, and the single value it is allowed to carry.
///
/// `<tools>` is the only overloaded wrapper in [`TOOL_CALL_OPEN_TAGS`]: in the
/// Hermes prompt format it DECLARES the available tools, while some models also
/// use it to wrap an invocation. Because it is overloaded, its contract is
/// narrower than every other alias:
///
/// 1. The body is delimited STRUCTURALLY -- see [`tools_span`].
/// 2. It carries exactly one canonical JSON invocation, or nothing.
/// 3. Whatever it does not carry is inert, and stays inert everywhere else.
struct ToolsSpan {
    /// Byte offset in `after_open` where the body ends.
    body_end: usize,
    /// Byte length of the close tag that terminated the span; 0 when unclosed.
    close_len: usize,
    /// The one JSON value the body carried, if the body was exactly one value.
    /// `None` means nothing in this span may ever be dispatched.
    value: Option<serde_json::Value>,
}

/// Delimit a `<tools>` span by PARSING it, never by substring search.
///
/// A textual scan for the close tag is not JSON-string aware, so tag-shaped
/// bytes inside argument content act as a delimiter: the wrapper truncates
/// mid-string, the valid call is lost, and the remainder is exposed to the
/// other recovery parsers as if the model had emitted it at top level. Tool
/// arguments routinely carry markup, so quoted tag text must never delimit.
///
/// Parsing first makes the distinction structural: bytes inside the parsed
/// value belong to the value, and only a close alias that FOLLOWS the value can
/// end the span. This holds for the matching close, a foreign close alias, and
/// no close at all -- the three ways a span can end -- so all three share one
/// rule rather than each re-deriving a boundary.
///
/// When the body is not exactly one JSON value it can never be admitted, so the
/// only remaining job is to bound the inert region. That search starts AFTER any
/// value that did parse, which keeps a quoted close from truncating the span.
/// A body that looks like JSON but does not parse gets no textual search at all:
/// a close alias may be quoted inside an unterminated string, and there is no
/// valid call in a malformed body to lose by consuming the remainder.
///
/// The FIRST close alias at or after that point ends the span; bytes beyond it
/// are outside the wrapper and are parsed normally. This is deliberate. A model
/// that echoes its declaration block and then invokes a tool is the common real
/// shape, and swallowing everything to a later close would make that invocation
/// unreachable. It also concedes nothing: content after the close is content the
/// model could have emitted with no wrapper at all, so treating it as ordinary
/// output grants no capability. What the wrapper policy owns is the body it
/// delimits -- that a declaration, an echoed prompt, or a prose example inside
/// the span never becomes a call -- not the whole remainder of the response.
fn tools_span(after_open: &str) -> ToolsSpan {
    let lead = after_open.len() - after_open.trim_start().len();
    let mut de = serde_json::Deserializer::from_str(after_open.trim_start())
        .into_iter::<serde_json::Value>();

    if let Some(Ok(value)) = de.next() {
        let body_end = lead + de.byte_offset();
        if let Some(rest) = after_open.get(body_end..) {
            let ws = rest.len() - rest.trim_start().len();
            let trailing = rest.trim_start();

            // Models mix open/close aliases, so ANY close tag can terminate the
            // wrapper. It still has to follow the value to count.
            if let Some(close) = TOOL_CALL_CLOSE_TAGS
                .iter()
                .find(|tag| trailing.starts_with(**tag))
            {
                return ToolsSpan {
                    body_end: body_end + ws,
                    close_len: close.len(),
                    value: Some(value),
                };
            }

            // Unclosed, but the body IS the complete value. Truncation of the
            // wrapper must not weaken the rule the closed paths enforce, and it
            // must not strengthen it either: a complete invocation still counts.
            if trailing.is_empty() {
                return ToolsSpan {
                    body_end: after_open.len(),
                    close_len: 0,
                    value: Some(value),
                };
            }
        }

        // A value parsed, but the body holds more than that one value (trailing
        // prose, a second value). Not admissible; bound the span from the end of
        // what parsed so quoted tag text inside it cannot act as the delimiter.
        return match find_first_tag(&after_open[body_end..], &TOOL_CALL_CLOSE_TAGS) {
            Some((idx, tag)) => ToolsSpan {
                body_end: body_end + idx,
                close_len: tag.len(),
                value: None,
            },
            None => ToolsSpan {
                body_end: after_open.len(),
                close_len: 0,
                value: None,
            },
        };
    }

    // Nothing parsed. If the body opens like JSON it is malformed JSON, and a
    // close alias could be quoted inside an unterminated string -- so do not
    // trust a textual scan to bound it. Consume the remainder instead; a
    // malformed `<tools>` body has no valid call to lose, and leaving a suffix
    // behind is exactly how refused bytes reach another executable parser.
    let trimmed = after_open.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return ToolsSpan {
            body_end: after_open.len(),
            close_len: 0,
            value: None,
        };
    }

    // Prose or a declaration block: no JSON string for a close alias to hide in,
    // so a textual bound is sound and keeps any following tags parseable.
    match find_first_tag(after_open, &TOOL_CALL_CLOSE_TAGS) {
        Some((idx, tag)) => ToolsSpan {
            body_end: idx,
            close_len: tag.len(),
            value: None,
        },
        None => ToolsSpan {
            body_end: after_open.len(),
            close_len: 0,
            value: None,
        },
    }
}

/// Does a byte RANGE overlap any `<tools>` span that was refused?
///
/// Refusing to admit a body is only half of the boundary. The other half is that
/// the refused bytes are not offered to a second executable parser. Fallbacks
/// that walk `remaining` get this for free, because the `<tools>` handler
/// advances past the span. Fallbacks that re-scan the ORIGINAL response do not,
/// and must consult this instead.
///
/// THE TEST IS OVERLAP, NOT MEMBERSHIP OF THE START OFFSET. Asking only whether
/// a match BEGINS inside a refused span leaves the boundary open from the other
/// side: a fence that opens BEFORE the span and runs through it never starts
/// inside anything, passes the check, and its body -- refused bytes included --
/// is handed to `extract_json_values`, which finds the very object the `<tools>`
/// handler rejected. Start-offset containment is not span containment.
///
/// Two ranges overlap when each begins before the other ends; empty ranges
/// cannot overlap anything.
fn range_hits_rejected_span(rejected: &[std::ops::Range<usize>], start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }
    rejected
        .iter()
        .any(|span| !span.is_empty() && start < span.end && span.start < end)
}

pub fn parse_tool_calls(response: &str) -> (String, Vec<ParsedToolCall>) {
    // Strip `<think>...</think>` blocks before parsing.  Qwen and other
    // reasoning models embed chain-of-thought inline in the response text;
    // these tags can interfere with `<tool_call>` extraction and must be
    // removed first.
    let cleaned = strip_think_tags(response);
    let response = cleaned.as_str();

    let mut text_parts = Vec::new();
    let mut calls = Vec::new();
    let mut remaining = response;
    // Byte ranges of `<tools>` spans this loop refused. Consulted by the global
    // fallbacks that re-scan `response` instead of walking `remaining`.
    let mut rejected_tools_spans: Vec<std::ops::Range<usize>> = Vec::new();

    // First, try to parse as OpenAI-style JSON response with tool_calls array
    // This handles model_providers like Minimax that return tool_calls in native JSON format
    if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(response.trim()) {
        calls = parse_tool_calls_from_json_value(&json_value);
        if !calls.is_empty() {
            // If we found tool_calls, extract any content field as text
            if let Some(content) = json_value.get("content").and_then(|v| v.as_str())
                && !content.trim().is_empty()
            {
                text_parts.push(content.trim().to_string());
            }
            return (text_parts.join("\n"), calls);
        }
    }
    if let Some(call) = parse_malformed_file_write_call(response.trim()) {
        return (String::new(), vec![call]);
    }

    // This scan searches the WHOLE response for executable `<invoke>` syntax, so
    // running it ahead of the tag loop lets legacy markup nested inside a
    // `<tools>` wrapper execute before the body is ever classified. Gating the
    // wrapper-local sites cannot protect a scan that runs first. When a `<tools>`
    // span is present the tag loop owns the text; responses without one are
    // unaffected.
    if !response.contains("<tools>")
        && let Some((minimax_text, minimax_calls)) = parse_minimax_invoke_calls(response)
        && !minimax_calls.is_empty()
    {
        return (minimax_text, minimax_calls);
    }

    // Fall back to XML-style tool-call tag parsing.
    while let Some((start, open_tag)) = find_first_tag(remaining, &TOOL_CALL_OPEN_TAGS) {
        // Everything before the tag is text
        let before = &remaining[..start];
        if !before.trim().is_empty() {
            text_parts.push(before.trim().to_string());
        }

        // `<tools>` is handled HERE, in full, and never reaches the generic
        // recovery paths below. Those paths exist to rescue malformed output from
        // unambiguous tags: they scan through prose for JSON, retry the body
        // against XML and GLM shorthand, and treat a foreign or missing close as
        // a reason to try harder. Every one of those behaviours is wrong for an
        // overloaded tag that also declares tools, and threading a guard through
        // each of them means the guard can be forgotten at the next path added.
        // Handling the tag once, at the top, makes that structurally impossible.
        if open_tag == "<tools>" {
            let after_open = &remaining[start + open_tag.len()..];
            let span = tools_span(after_open);
            let consumed = span.body_end + span.close_len;

            // Exactly one canonical invocation is the entire admissible set.
            let admitted = span
                .value
                .as_ref()
                .filter(|value| looks_like_tools_wrapper_invocation(value))
                .map(parse_tool_calls_from_json_value)
                .filter(|parsed| !parsed.is_empty());

            if let Some(parsed) = admitted {
                calls.extend(parsed);
            } else {
                // Refused. The body becomes visible text, and the span is
                // recorded so no later parser can execute the bytes this
                // boundary just declined.
                let body = after_open[..span.body_end].trim();
                if !body.is_empty() {
                    text_parts.push(body.to_string());
                }
                let span_start = response.len() - remaining.len() + start;
                rejected_tools_spans.push(span_start..span_start + open_tag.len() + consumed);
            }

            remaining = &after_open[consumed..];
            continue;
        }

        let Some(close_tag) = (match open_tag {
            "<tool_call>" => Some("</tool_call>"),
            "<tool_calls>" => Some("</tool_calls>"),
            "<toolcall>" => Some("</toolcall>"),
            "<tool-call>" => Some("</tool-call>"),
            "<invoke>" => Some("</invoke>"),
            "<minimax:tool_call>" => Some("</minimax:tool_call>"),
            "<minimax:toolcall>" => Some("</minimax:toolcall>"),
            _ => None,
        }) else {
            break;
        };

        let after_open = &remaining[start + open_tag.len()..];
        if let Some(close_idx) = after_open.find(close_tag) {
            let inner = &after_open[..close_idx];
            let mut parsed_any = false;

            let json_values = extract_json_values(inner);
            for value in json_values {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                if !parsed_calls.is_empty() {
                    parsed_any = true;
                    calls.extend(parsed_calls);
                }
            }

            if !parsed_any && let Some(call) = parse_malformed_file_write_call(inner) {
                calls.push(call);
                parsed_any = true;
            }

            // If JSON parsing failed, try XML format (DeepSeek/GLM style)
            if !parsed_any && let Some(xml_calls) = parse_xml_tool_calls(inner) {
                calls.extend(xml_calls);
                parsed_any = true;
            }

            if !parsed_any {
                // GLM-style shortened body: `shell>uname -a` or `shell\ncommand: date`
                if let Some(glm_call) = parse_glm_shortened_body(inner) {
                    calls.push(glm_call);
                    parsed_any = true;
                }
            }

            if !parsed_any {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "Malformed <tool_call>: expected tool-call object in tag body (JSON/XML/GLM)"
                );
            }

            remaining = &after_open[close_idx + close_tag.len()..];
        } else {
            // Matching close tag not found — try cross-alias close tags first.
            // Models sometimes mix open/close tag aliases (e.g. <tool_call>...</invoke>).
            let mut resolved = false;
            if let Some((cross_idx, cross_tag)) = find_first_tag(after_open, &TOOL_CALL_CLOSE_TAGS)
            {
                let inner = &after_open[..cross_idx];
                let mut parsed_any = false;

                // Try JSON
                let json_values = extract_json_values(inner);
                for value in json_values {
                    let parsed_calls = parse_tool_calls_from_json_value(&value);
                    if !parsed_calls.is_empty() {
                        parsed_any = true;
                        calls.extend(parsed_calls);
                    }
                }

                if !parsed_any && let Some(call) = parse_malformed_file_write_call(inner) {
                    calls.push(call);
                    parsed_any = true;
                }

                // Try XML
                if !parsed_any && let Some(xml_calls) = parse_xml_tool_calls(inner) {
                    calls.extend(xml_calls);
                    parsed_any = true;
                }

                // Try GLM shortened body
                if !parsed_any && let Some(glm_call) = parse_glm_shortened_body(inner) {
                    calls.push(glm_call);
                    parsed_any = true;
                }

                if parsed_any {
                    remaining = &after_open[cross_idx + cross_tag.len()..];
                    resolved = true;
                }
            }

            if resolved {
                continue;
            }

            // No cross-alias close tag resolved — fall back to JSON recovery
            // from unclosed tags (brace-balancing).
            if let Some(json_end) = find_json_end(after_open)
                && let Ok(value) =
                    serde_json::from_str::<serde_json::Value>(&after_open[..json_end])
            {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                if !parsed_calls.is_empty() {
                    calls.extend(parsed_calls);
                    remaining = strip_leading_close_tags(&after_open[json_end..]);
                    continue;
                }
            }

            if let Some((value, consumed_end)) = extract_first_json_value_with_end(after_open) {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                if !parsed_calls.is_empty() {
                    calls.extend(parsed_calls);
                    remaining = strip_leading_close_tags(&after_open[consumed_end..]);
                    continue;
                }
            }

            if let Some(call) = parse_malformed_file_write_call(after_open) {
                calls.push(call);
                remaining = "";
                continue;
            }

            // Last resort: try GLM shortened body on everything after the open tag.
            // The model may have emitted `<tool_call>shell>ls` with no close tag at all.
            let glm_input = after_open.trim();
            if let Some(glm_call) = parse_glm_shortened_body(glm_input) {
                calls.push(glm_call);
                remaining = "";
                continue;
            }

            remaining = &remaining[start..];
            break;
        }
    }

    // The fallbacks below re-scan the ORIGINAL response rather than walking
    // `remaining`, so the tag loop's consume-on-reject does not reach them: a
    // `<tools>` body this parser already refused is otherwise handed straight to
    // the next executable parser, which has no notion of the wrapper policy.
    // Every match therefore has to be checked against the refused spans. The
    // fallbacks further down operate on `remaining` and are covered already.

    // If XML tags found nothing, try markdown code blocks with tool_call language.
    // Models behind OpenRouter sometimes output ```tool_call ... ``` or hybrid
    // ```tool_call ... </tool_call> instead of structured API calls or XML tags.
    if calls.is_empty() {
        static MD_TOOL_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"(?s)```(?:tool[_-]?call|invoke)\s*\n(.*?)(?:```|</tool[_-]?call>|</toolcall>|</invoke>|</minimax:toolcall>)",
            )
            .expect("MD_TOOL_CALL_RE regex must compile")
        });
        let mut md_text_parts: Vec<String> = Vec::new();
        let mut last_end = 0;

        for cap in MD_TOOL_CALL_RE.captures_iter(response) {
            let Some(full_match) = cap.get(0) else {
                continue;
            };
            // Range-aware: a fence that OPENS before a refused span and runs
            // through it must be refused too, not just one that starts inside.
            if range_hits_rejected_span(&rejected_tools_spans, full_match.start(), full_match.end())
            {
                continue;
            }
            let before = &response[last_end..full_match.start()];
            if !before.trim().is_empty() {
                md_text_parts.push(before.trim().to_string());
            }
            let inner = &cap[1];
            let json_values = extract_json_values(inner);
            for value in json_values {
                let parsed_calls = parse_tool_calls_from_json_value(&value);
                calls.extend(parsed_calls);
            }
            if calls.is_empty()
                && let Some(call) = parse_malformed_file_write_call(inner)
            {
                calls.push(call);
            }
            last_end = full_match.end();
        }

        if !calls.is_empty() {
            let after = &response[last_end..];
            if !after.trim().is_empty() {
                md_text_parts.push(after.trim().to_string());
            }
            text_parts = md_text_parts;
            remaining = "";
        }
    }

    // Try ```tool <name> format used by some model_providers (e.g., xAI grok)
    // Example: ```tool file_write\n{"path": "...", "content": "..."}\n```
    if calls.is_empty() {
        static MD_TOOL_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?s)```tool\s+(\w+)\s*\n(.*?)(?:```|$)")
                .expect("MD_TOOL_NAME_RE regex must compile")
        });
        let mut md_text_parts: Vec<String> = Vec::new();
        let mut last_end = 0;

        for cap in MD_TOOL_NAME_RE.captures_iter(response) {
            let Some(full_match) = cap.get(0) else {
                continue;
            };
            // Range-aware: a fence that OPENS before a refused span and runs
            // through it must be refused too, not just one that starts inside.
            if range_hits_rejected_span(&rejected_tools_spans, full_match.start(), full_match.end())
            {
                continue;
            }
            let before = &response[last_end..full_match.start()];
            if !before.trim().is_empty() {
                md_text_parts.push(before.trim().to_string());
            }
            let tool_name = &cap[1];
            let inner = &cap[2];

            // Try to parse the inner content as JSON arguments
            let json_values = extract_json_values(inner);
            if json_values.is_empty() {
                if map_tool_name_alias(tool_name) == "file_write"
                    && let Some(arguments) = parse_malformed_file_write_arguments(inner)
                {
                    calls.push(ParsedToolCall {
                        name: "file_write".to_string(),
                        arguments,
                        tool_call_id: None,
                    });
                } else {
                    // Log a warning if we found a tool block but couldn't parse arguments
                    ::zeroclaw_log::record!(
                        WARN,
                        malformed_tool_block_event(inner.len()),
                        "Found ```tool <name> block but could not parse JSON arguments"
                    );
                }
            } else {
                for value in json_values {
                    let arguments = if value.is_object() {
                        value
                    } else {
                        serde_json::Value::Object(serde_json::Map::new())
                    };
                    calls.push(ParsedToolCall {
                        name: tool_name.to_string(),
                        arguments,
                        tool_call_id: None,
                    });
                }
            }
            last_end = full_match.end();
        }

        if !calls.is_empty() {
            let after = &response[last_end..];
            if !after.trim().is_empty() {
                md_text_parts.push(after.trim().to_string());
            }
            text_parts = md_text_parts;
            remaining = "";
        }
    }

    if calls.is_empty() {
        let xml_calls = parse_xml_attribute_tool_calls(remaining);
        if !xml_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in xml_calls {
                calls.push(call);
                // Try to remove the XML from text
                if let Some(start) = cleaned_text.find("<minimax:toolcall>")
                    && let Some(end) = cleaned_text.find("</minimax:toolcall>")
                {
                    let end_pos = end + "</minimax:toolcall>".len();
                    if end_pos <= cleaned_text.len() {
                        cleaned_text =
                            format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    if calls.is_empty() {
        let perl_calls = parse_perl_style_tool_calls(remaining);
        if !perl_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in perl_calls {
                calls.push(call);
                // Try to remove the TOOL_CALL block from text
                while let Some(start) = cleaned_text.find("TOOL_CALL") {
                    if let Some(end) = cleaned_text.find("/TOOL_CALL") {
                        let end_pos = end + "/TOOL_CALL".len();
                        if end_pos <= cleaned_text.len() {
                            cleaned_text =
                                format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                        }
                    } else {
                        break;
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // <FunctionCall>
    // file_read
    // <code>path>/Users/...</code>
    // </FunctionCall>
    if calls.is_empty() {
        let func_calls = parse_function_call_tool_calls(remaining);
        if !func_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for call in func_calls {
                calls.push(call);
                // Try to remove the FunctionCall block from text
                while let Some(start) = cleaned_text.find("<FunctionCall>") {
                    if let Some(end) = cleaned_text.find("</FunctionCall>") {
                        let end_pos = end + "</FunctionCall>".len();
                        if end_pos <= cleaned_text.len() {
                            cleaned_text =
                                format!("{}{}", &cleaned_text[..start], &cleaned_text[end_pos..]);
                        }
                    } else {
                        break;
                    }
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // GLM-style tool calls (browser_open/url>https://..., shell/command>ls, etc.)
    if calls.is_empty() {
        let glm_calls = parse_glm_style_tool_calls(remaining);
        if !glm_calls.is_empty() {
            let mut cleaned_text = remaining.to_string();
            for (name, args, raw) in &glm_calls {
                calls.push(ParsedToolCall {
                    name: name.clone(),
                    arguments: args.clone(),
                    tool_call_id: None,
                });
                if let Some(r) = raw {
                    cleaned_text = cleaned_text.replace(r, "");
                }
            }
            if !cleaned_text.trim().is_empty() {
                text_parts.push(cleaned_text.trim().to_string());
            }
            remaining = "";
        }
    }

    // Remaining text after last tool call
    if !remaining.trim().is_empty() {
        text_parts.push(remaining.trim().to_string());
    }

    (text_parts.join("\n"), calls)
}

/// Strip prompt-guided tool artifacts from visible output while preserving
/// raw model text in history for future turns.
pub fn strip_tool_result_blocks(text: &str) -> String {
    static TOOL_RESULT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<tool_result[^>]*>.*?</tool_result>")
            .expect("TOOL_RESULT_RE regex must compile")
    });
    static THINKING_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<thinking>.*?</thinking>").expect("THINKING_RE regex must compile")
    });
    static THINK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)<think>.*?</think>").expect("THINK_RE regex must compile")
    });
    static TOOL_RESULTS_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^\[Tool results\]\s*\n?")
            .expect("TOOL_RESULTS_PREFIX_RE regex must compile")
    });
    static EXCESS_BLANK_LINES_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\n{3,}").expect("EXCESS_BLANK_LINES_RE regex must compile"));

    let result = TOOL_RESULT_RE.replace_all(text, "");
    let result = THINKING_RE.replace_all(&result, "");
    let result = THINK_RE.replace_all(&result, "");
    let result = TOOL_RESULTS_PREFIX_RE.replace_all(&result, "");
    let result = EXCESS_BLANK_LINES_RE.replace_all(result.trim(), "\n\n");

    result.trim().to_string()
}

pub fn detect_tool_call_parse_issue(
    response: &str,
    parsed_calls: &[ParsedToolCall],
) -> Option<String> {
    if !parsed_calls.is_empty() {
        return None;
    }

    let trimmed = response.trim();
    if trimmed.is_empty() {
        return None;
    }

    if looks_like_tool_protocol_envelope(trimmed) {
        return Some(
            "response resembled an internal tool protocol envelope but no valid tool call could be parsed"
                .into(),
        );
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return has_malformed_tool_protocol_json_signal(&value).then(|| {
            "response resembled an internal tool protocol envelope but no valid tool call could be parsed"
                .into()
        });
    }

    if has_malformed_tool_protocol_text_signal(trimmed) {
        return Some(
            "response resembled an internal tool protocol envelope but no valid tool call could be parsed"
                .into(),
        );
    }

    let contains_tool_payload_marker = trimmed.contains("<tool_call")
        || trimmed.contains("<toolcall")
        || trimmed.contains("<tool-call")
        || trimmed.contains("```tool_call")
        || trimmed.contains("```toolcall")
        || trimmed.contains("```tool-call")
        || trimmed.contains("```tool file_")
        || trimmed.contains("```tool shell")
        || trimmed.contains("```tool web_")
        || trimmed.contains("```tool memory_")
        || trimmed.contains("```tool ") // Generic ```tool <name> pattern
        || trimmed.contains("TOOL_CALL")
        || trimmed.contains("[TOOL_CALL]")
        || trimmed.contains("<FunctionCall>");

    if contains_tool_payload_marker {
        if looks_like_tool_protocol_example(trimmed) {
            return None;
        }
        if contains_tool_protocol_tag_call(trimmed) {
            return Some(
                "response resembled a tool-call payload but no valid tool call could be parsed"
                    .into(),
            );
        }

        let (visible_text, recovered_calls) = parse_tool_calls(trimmed);
        if !recovered_calls.is_empty() && !visible_text.trim().is_empty() {
            return None;
        }
        if !recovered_calls.is_empty() || visible_text.trim().is_empty() {
            return Some(
                "response resembled a tool-call payload but no valid tool call could be parsed"
                    .into(),
            );
        }
    }

    if looks_like_malformed_tool_protocol_envelope(trimmed) {
        Some("response resembled a tool-call payload but no valid tool call could be parsed".into())
    } else {
        None
    }
}

pub fn build_native_assistant_history_from_parsed_calls(
    text: &str,
    tool_calls: &[ParsedToolCall],
    reasoning_content: Option<&str>,
) -> Option<String> {
    // Strict provider validators (DeepSeek V4, NVIDIA NIM, ...) reject
    // assistant messages that carry `tool_calls: []`. When there are no
    // parsed calls, return None so the caller falls through to a plain
    // text assistant message.
    if tool_calls.is_empty() {
        return None;
    }

    let calls_json = tool_calls
        .iter()
        .map(|tc| {
            Some(serde_json::json!({
                "id": tc.tool_call_id.clone()?,
                "name": tc.name,
                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content
        && let Some(obj) = obj.as_object_mut()
    {
        obj.insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    Some(obj.to_string())
}

#[cfg(test)]
mod tools_wrapper_body_boundary_tests {
    use super::*;

    /// REGRESSION: explanatory prose inside `<tools>` around an
    /// invocation-shaped example became an EXECUTABLE call.
    ///
    /// `extract_json_values` scans through surrounding text to find JSON, so
    /// the value predicate never saw the prose. Observed at head 274019fb5:
    ///
    /// ```text
    /// PROSE-WRAPPED EXAMPLE produced 1 call(s):
    /// [ParsedToolCall { name: "shell", arguments: {"command": "rm -rf /tmp/x"} }]
    /// ```
    #[test]
    fn tools_prose_wrapped_invocation_example_is_inert() {
        let payload = concat!(
            "<tools>\n",
            "For example, a shell invocation looks like this:\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n",
            "</tools>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert!(
            calls.is_empty(),
            "prose around an invocation-shaped example must stay inert, got {calls:?}"
        );
    }

    /// The same smuggling path via a FOREIGN close tag. The complete-body rule
    /// holds on matching, foreign and missing closes alike.
    #[test]
    fn tools_prose_wrapped_example_with_foreign_close_is_inert() {
        let payload = concat!(
            "<tools>\n",
            "Here is what a call looks like:\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n",
            "</tool_call>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert!(
            calls.is_empty(),
            "foreign close must not admit a prose-wrapped example, got {calls:?}"
        );
    }

    /// Trailing prose is disqualifying too -- the body IS the value or nothing.
    #[test]
    fn tools_invocation_with_trailing_prose_is_inert() {
        let payload = concat!(
            "<tools>\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n",
            "...that is how you would call it.\n",
            "</tools>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert!(
            calls.is_empty(),
            "trailing prose must disqualify the body, got {calls:?}"
        );
    }

    /// The motivating case must STILL work: a bare canonical invocation.
    /// Without this the fix could pass by rejecting everything.
    #[test]
    fn tools_bare_canonical_invocation_still_parses() {
        let payload = "<tools>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tools>";
        let (_text, calls) = parse_tool_calls(payload);
        assert_eq!(
            calls.len(),
            1,
            "canonical invocation must still parse, got {calls:?}"
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").and_then(|v| v.as_str()),
            Some("ls"),
            "arguments must survive intact"
        );
    }

    /// REGRESSION: a `<tools>` string
    /// INSIDE otherwise valid arguments must not be touched. The first fix used
    /// a raw-text pre-pass over the whole response, which could not tell wrapper
    /// syntax from tag-shaped bytes inside a JSON string and rewrote the
    /// `content` of a legitimate `file_write` before dispatch.
    #[test]
    fn tools_string_inside_valid_arguments_is_preserved() {
        let payload = concat!(
            "<tool_call>{\"name\":\"file_write\",\"arguments\":",
            "{\"content\":\"<tools>example</tools>\"}}</tool_call>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert_eq!(
            calls.len(),
            1,
            "the file_write call must survive, got {calls:?}"
        );
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("content").and_then(|v| v.as_str()),
            Some("<tools>example</tools>"),
            "argument content must be preserved byte-for-byte"
        );
    }

    /// Nested close-alias case under the UNAMBIGUOUS tags, pinned but not fixed.
    ///
    /// A `</tool_call>` inside JSON string content terminates the outer wrapper,
    /// and the exposed bytes reach the GLM fallback as a real call. The tag
    /// scanner for those aliases is textual, so it cannot see that the close is
    /// inside a string. `<tools>` no longer has this defect -- it is delimited by
    /// parsing -- but generalising that to every alias changes the recovery
    /// behaviour of tags this change does not otherwise touch, so it is left as a
    /// ready repro rather than folded in here.
    ///
    /// The assertions describe the DESIRED behaviour and the test is ignored,
    /// so it starts passing the moment the scanner becomes string-aware.
    #[test]
    #[ignore = "pre-existing defect of the textual tag scanner, outside the <tools> alias"]
    fn close_alias_inside_arguments_should_not_expose_nested_call() {
        let payload = concat!(
            "<tool_call>{\"name\":\"file_write\",\"arguments\":",
            "{\"content\":\"<tools>x</tool_call><tool_call>shell>pwd</tool_call>\"}}",
            "</tool_call>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        // Current behaviour without a string-aware scanner: the outer file_write
        // is lost and `shell`/`pwd` is dispatched from the exposed remainder.
        assert!(
            !calls.iter().any(|c| c.name == "shell"),
            "a close alias inside argument content must not become a shell call, got {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.name == "file_write"),
            "the outer file_write must survive, got {calls:?}"
        );
    }

    /// REGRESSION: the admitted value must be the
    /// EXECUTED value. `parse_tool_calls_from_json_value` prefers a nested
    /// `function`, so a body with a benign top level and a hostile nested
    /// envelope passed admission on one representation and dispatched another.
    #[test]
    fn tools_top_level_plus_nested_function_is_inert() {
        let payload = concat!(
            "<tools>{\"name\":\"benign\",\"arguments\":{},",
            "\"function\":{\"name\":\"shell\",\"arguments\":",
            "{\"command\":\"rm -rf /tmp/x\"}}}</tools>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert!(
            calls.is_empty(),
            "a mixed top-level + function envelope must not be admitted, got {calls:?}"
        );
    }

    /// Same divergence via `tool_calls`, which can expand one admitted value
    /// into several executed calls.
    #[test]
    fn tools_top_level_plus_tool_calls_envelope_is_inert() {
        let payload = concat!(
            "<tools>{\"name\":\"benign\",\"arguments\":{},",
            "\"tool_calls\":[{\"name\":\"shell\",\"arguments\":",
            "{\"command\":\"rm -rf /tmp/x\"}}]}</tools>"
        );
        let (_text, calls) = parse_tool_calls(payload);
        assert!(
            calls.is_empty(),
            "a mixed top-level + tool_calls envelope must not be admitted, got {calls:?}"
        );
    }

    /// Whitespace and newlines around the JSON are NOT prose.
    #[test]
    fn tools_whitespace_padded_invocation_still_parses() {
        let payload =
            "<tools>\n\n  {\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}  \n</tools>";
        let (_text, calls) = parse_tool_calls(payload);
        assert_eq!(
            calls.len(),
            1,
            "whitespace padding must not disqualify, got {calls:?}"
        );
    }
}

#[cfg(test)]
mod tests {

    // ---- <tools> boundary regressions ----
    // Each negative below is a payload that reached an executable parser before
    // the boundary was made structural. The positive controls alongside them pin
    // the boundary from the other side: a guard that simply refused every body
    // would satisfy the negatives and fail these.

    #[test]
    fn tools_unclosed_prose_prefixed_invocation_stays_inert() {
        // find_json_end / extract_first_json_value_with_end scan THROUGH text, so a
        // prose prefix under a truncated wrapper dispatched a real shell call.
        let text = "<tools>\nFor example:\n{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}";
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "prose-prefixed unclosed body must not dispatch: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tools_unclosed_trailing_suffix_stays_inert() {
        let text = "<tools>{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}} and then some prose";
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "trailing non-whitespace must disqualify: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tools_unclosed_multiple_values_stays_inert() {
        let text = "<tools>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}";
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "two values are not one body: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tools_unclosed_bare_canonical_invocation_still_parses() {
        // Positive control for the three above: a truncated wrapper around exactly
        // one canonical invocation MUST still work, or the guard is just a mute.
        let text = "<tools>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}";
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "a clean unclosed invocation must still parse"
        );
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn tools_wrapping_legacy_invoke_does_not_bypass_the_guard() {
        // parse_minimax_invoke_calls ran BEFORE the <tools> loop over the whole
        // response, so nested legacy markup executed before classification.
        let text = "<tools><invoke name=\"shell\"><parameter name=\"command\">rm -rf /tmp/x</parameter></invoke></tools>";
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "legacy <invoke> nested in <tools> must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn bare_legacy_invoke_without_tools_still_parses() {
        // Positive control for the pre-loop guard: minimax recovery must keep
        // working for every response that has no <tools> span.
        let text = "<invoke name=\"shell\"><parameter name=\"command\">ls</parameter></invoke>";
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "minimax <invoke> recovery must survive the guard"
        );
    }

    #[test]
    fn tools_literal_close_tag_inside_arguments_is_preserved() {
        // A plain substring find is not JSON-string aware: a literal </tools> in
        // argument content truncated the wrapper and LOST the valid call.
        let text = "<tools>{\"name\":\"file_write\",\"arguments\":{\"content\":\"literal </tools> markup\"}}</tools>";
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "literal </tools> in content must not delimit: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "file_write");
        let args = calls[0].arguments.to_string();
        assert!(
            args.contains("</tools>"),
            "argument content must survive intact: {args}"
        );
    }

    // ---------------------------------------------------------------------
    // Quoted close aliases must not delimit a `<tools>` span on ANY path.
    //
    // Close detection was structural only when the parsed value was followed
    // immediately by the MATCHING close. The foreign-close and missing-close
    // paths fell back to a substring scan, so a close alias inside JSON string
    // content still truncated the wrapper: the valid call was dropped and the
    // remainder after the quoted tag was exposed to another executable parser.
    // ---------------------------------------------------------------------

    #[test]
    fn tools_quoted_foreign_close_inside_arguments_is_preserved() {
        // Foreign-close path: the span really ends at `</tool_call>`, but a
        // quoted `</tools>` sits inside `content` and used to delimit first.
        let text = "<tools>{\"name\":\"file_write\",\"arguments\":{\"content\":\"literal </tools> markup\"}}</tool_call>";
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "quoted close must not delimit a foreign-closed span: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "file_write");
        assert!(calls[0].arguments.to_string().contains("</tools>"));
    }

    #[test]
    fn tools_quoted_close_inside_unclosed_arguments_is_preserved() {
        // Missing-close path: no close tag at all, and the only tag-shaped bytes
        // in the span are quoted inside argument content.
        let text = "<tools>{\"name\":\"file_write\",\"arguments\":{\"content\":\"literal </tools> markup\"}}";
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "quoted close must not delimit an unclosed span: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "file_write");
        assert!(calls[0].arguments.to_string().contains("</tools>"));
    }

    #[test]
    fn tools_nested_suffix_after_quoted_close_is_never_dispatched() {
        // The consequence of a textual delimiter, on the missing-close path: the
        // span has no real close, so the scan hit the `</tools>` quoted inside
        // `content`, truncated there, and handed the remainder back to the tag
        // loop as if the model had emitted it at top level. The nested payload
        // uses GLM shorthand deliberately -- it needs no quotes, so it survives
        // being cut out of a JSON string and reaches the legacy parser as a real
        // `shell` call. The wrapper holds ONE file_write; that is the only call
        // this response may produce.
        let text = concat!(
            "<tools>{\"name\":\"file_write\",\"arguments\":{\"content\":",
            "\"</tools><tool_call>shell>rm -rf /tmp/x</tool_call>\"}}"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            !calls.iter().any(|c| c.name == "shell"),
            "quoted nested tool_call must never be dispatched: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(
            calls.len(),
            1,
            "nested suffix must not become a second call: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "file_write");
    }

    // ---------------------------------------------------------------------
    // Consume-on-reject covers the global fallbacks too.
    //
    // The fenced-Markdown fallbacks re-scan the ORIGINAL response instead of
    // walking `remaining`, so a `<tools>` body the wrapper policy had already
    // refused was handed to a second executable parser and dispatched.
    // ---------------------------------------------------------------------

    #[test]
    fn rejected_tools_span_hiding_fenced_tool_call_stays_inert() {
        // Matching close. The body is a declaration array -- refused on shape --
        // but it contains a fenced ```tool_call the Markdown fallback would run.
        let text = concat!(
            "<tools>\n",
            "[{\"name\":\"shell\",\"description\":\"run\",\"parameters\":{}}]\n",
            "```tool_call\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n",
            "```\n",
            "</tools>"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "fenced tool_call inside a refused <tools> span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejected_unclosed_tools_span_hiding_fenced_tool_call_stays_inert() {
        // Missing close, same smuggle.
        let text = concat!(
            "<tools>\n",
            "Here is how the format works:\n",
            "```tool_call\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "fenced tool_call in a refused unclosed <tools> span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejected_tools_span_hiding_named_tool_fence_stays_inert() {
        // The second fenced fallback, ```tool <name>, has the same exposure.
        let text = concat!(
            "<tools>\n",
            "[{\"name\":\"file_write\",\"description\":\"write\",\"parameters\":{}}]\n",
            "```tool file_write\n",
            "{\"path\":\"/tmp/x\",\"content\":\"pwned\"}\n",
            "```\n",
            "</tools>"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "named-tool fence inside a refused <tools> span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejected_unclosed_tools_span_hiding_named_tool_fence_stays_inert() {
        let text = concat!(
            "<tools>\n",
            "For example:\n",
            "```tool file_write\n",
            "{\"path\":\"/tmp/x\",\"content\":\"pwned\"}\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "named-tool fence in a refused unclosed <tools> span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    /// REGRESSION: a fence that OPENS BEFORE a refused `<tools>` span and runs
    /// through it must not be parsed either.
    ///
    /// The ledger originally tested only whether a match STARTED inside a
    /// refused range. A fence opening before the span never starts inside
    /// anything, passed that check, and its body -- refused bytes included --
    /// reached `extract_json_values`, which found the very object the `<tools>`
    /// handler had rejected. The existing tests cover the inverse nesting (fence
    /// starting inside the span), so they do not exercise this direction.
    #[test]
    fn fence_opening_before_a_refused_tools_span_stays_inert() {
        let text = concat!(
            "```tool_call\n",
            "<tools>\n",
            "Here is how the format works:\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"rm -rf /tmp/x\"}}\n",
            "</tools>\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "a fence overlapping a refused <tools> span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    /// The named-tool fence has the same overlap exposure.
    #[test]
    fn named_tool_fence_opening_before_a_refused_tools_span_stays_inert() {
        let text = concat!(
            "```tool file_write\n",
            "<tools>\n",
            "For example:\n",
            "{\"path\":\"/tmp/x\",\"content\":\"pwned\"}\n",
            "</tools>\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert!(
            calls.is_empty(),
            "a named-tool fence overlapping a refused span must stay inert: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
    }

    /// The overlap test must be OVERLAP, not "touches an endpoint". A fence that
    /// ends exactly where a refused span begins shares no byte with it and must
    /// still parse, or the guard silently eats adjacent legitimate calls.
    #[test]
    fn range_overlap_is_exclusive_at_the_boundaries() {
        // Two spans, not one: a single-range vec is also a clippy trap, and
        // multiple refused spans is the real shape anyway.
        let rejected = [10usize..20usize, 40usize..50usize];
        assert!(
            !range_hits_rejected_span(&rejected, 0, 10),
            "abutting before"
        );
        assert!(
            !range_hits_rejected_span(&rejected, 20, 30),
            "abutting after"
        );
        assert!(
            range_hits_rejected_span(&rejected, 5, 15),
            "opens before, crosses in"
        );
        assert!(
            range_hits_rejected_span(&rejected, 15, 25),
            "opens inside, crosses out"
        );
        assert!(
            range_hits_rejected_span(&rejected, 0, 30),
            "spans it entirely"
        );
        assert!(range_hits_rejected_span(&rejected, 12, 15), "wholly inside");
        // An empty match cannot consume refused bytes.
        assert!(!range_hits_rejected_span(&rejected, 15, 15), "empty range");
        // The second span must be honoured too, not just the first.
        assert!(
            range_hits_rejected_span(&rejected, 45, 60),
            "overlaps the later span"
        );
        assert!(
            !range_hits_rejected_span(&rejected, 25, 35),
            "between spans"
        );
    }

    #[test]
    fn fenced_tool_call_outside_any_tools_span_still_parses() {
        // POSITIVE CONTROL. The span ledger must suppress only refused bytes.
        // A hardening change that simply stopped running the fenced fallbacks
        // would satisfy every negative above; this fails if that happens.
        let text = concat!(
            "```tool_call\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(calls.len(), 1, "fenced tool_call recovery must survive");
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn fenced_tool_call_after_a_rejected_tools_span_still_parses() {
        // POSITIVE CONTROL for the span BOUNDARY: a refused declaration must not
        // swallow the rest of the response. The fence here sits outside the span.
        let text = concat!(
            "<tools>[{\"name\":\"shell\",\"description\":\"run\",\"parameters\":{}}]</tools>\n",
            "```tool_call\n",
            "{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n",
            "```"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "a fence after a refused span must still parse: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[test]
    fn canonical_tool_call_after_a_rejected_tools_declaration_still_parses() {
        // POSITIVE CONTROL: the common real shape -- a model echoes its tool
        // declarations, then invokes one. Bounding the refused span correctly is
        // what keeps the following invocation reachable.
        let text = concat!(
            "<tools>[{\"name\":\"shell\",\"description\":\"run\",\"parameters\":{}}]</tools>\n",
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}</tool_call>"
        );
        let (_v, calls) = parse_tool_calls(text);
        assert_eq!(
            calls.len(),
            1,
            "invocation after a declaration must still parse: {:?}",
            calls.iter().map(|c| c.name.clone()).collect::<Vec<_>>()
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    use super::*;

    #[test]
    fn incomplete_protocol_json_trips_on_a_single_identifying_key() {
        // One key is enough while the value is still arriving: the corroborating
        // key may simply not have been emitted yet.
        assert!(looks_like_incomplete_tool_protocol_json(
            "{\"tool_call_id\":\"call_1\","
        ));
        assert!(looks_like_incomplete_tool_protocol_json(
            "{\"tool_calls\":[{\"name\":\"shell\""
        ));
        assert!(looks_like_incomplete_tool_protocol_json(
            "{\"function_call\":{\"arguments\":\"{\\\"a\\\":1"
        ));
        // An unclosed JSON fence is the same payload with a wrapper.
        assert!(looks_like_incomplete_tool_protocol_json(
            "```json\n{\"tool_call_id\":\"c1\","
        ));
    }

    #[test]
    fn incomplete_protocol_json_ignores_complete_values_and_business_json() {
        // Complete values belong to the ordinary classifiers, which can parse
        // them and judge them properly.
        assert!(!looks_like_incomplete_tool_protocol_json(
            "{\"tool_call_id\":\"call_1\",\"content\":\"done\"}"
        ));
        // Business JSON carries none of the identifying keys, so a half-arrived
        // config still streams.
        assert!(!looks_like_incomplete_tool_protocol_json(
            "{\"retries\": 3, \"timeout_ms\":"
        ));
        // Prose is not JSON, however much it talks about tool calls.
        assert!(!looks_like_incomplete_tool_protocol_json(
            "The \"tool_call_id\" field identifies the call."
        ));
        assert!(!looks_like_incomplete_tool_protocol_json(""));
    }

    #[test]
    fn build_native_assistant_history_returns_none_for_empty_calls() {
        // Regression: strict providers (DeepSeek V4, NVIDIA NIM) reject
        // assistant messages carrying `tool_calls: []`. Empty input must
        // not produce a serialised assistant message with an empty array.
        let result = build_native_assistant_history_from_parsed_calls("answer text", &[], None);
        assert!(
            result.is_none(),
            "expected None for empty tool_calls slice, got {result:?}"
        );
    }

    #[test]
    fn build_native_assistant_history_returns_none_for_empty_calls_with_reasoning() {
        // Even with reasoning_content set, an empty tool_calls slice must
        // collapse to None — the caller falls back to a plain assistant
        // message, and the reasoning round-trip happens through a separate
        // path that does not produce `tool_calls: []`.
        let result = build_native_assistant_history_from_parsed_calls(
            "answer text",
            &[],
            Some("deep thought"),
        );
        assert!(result.is_none());
    }

    #[test]
    fn build_native_assistant_history_emits_tool_calls_when_non_empty() {
        // No-regression check: the normal path with a real parsed call
        // still produces a serialised assistant message and the
        // `tool_calls` field is a non-empty array.
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_1".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls("answer", &calls, None);
        let s = result.expect("Some(_) for non-empty tool_calls");
        let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        let arr = parsed["tool_calls"].as_array().expect("tool_calls array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"].as_str(), Some("shell"));
    }

    #[test]
    fn parse_arguments_value_unwraps_nested_object_string() {
        let raw = serde_json::json!({
            "service": "gmail",
            "params": "{\"maxResults\":3}"
        });
        let out = parse_arguments_value(Some(&raw));
        assert_eq!(out["service"], serde_json::json!("gmail"));
        assert_eq!(out["params"], serde_json::json!({"maxResults": 3}));
    }

    #[test]
    fn parse_arguments_value_unwraps_nested_array_string() {
        let raw = serde_json::json!({ "items": "[1,2,3]" });
        let out = parse_arguments_value(Some(&raw));
        assert_eq!(out["items"], serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn parse_arguments_value_leaves_non_json_strings_alone() {
        let raw = serde_json::json!({
            "greeting": "hello",
            "answer": "42",
            "truthy": "true",
            "broken": "{not json"
        });
        let out = parse_arguments_value(Some(&raw));
        assert_eq!(out["greeting"], serde_json::json!("hello"));
        assert_eq!(out["answer"], serde_json::json!("42"));
        assert_eq!(out["truthy"], serde_json::json!("true"));
        assert_eq!(out["broken"], serde_json::json!("{not json"));
    }

    #[test]
    fn parse_arguments_value_handles_double_encoding() {
        let inner = r#"{"params":"{\"maxResults\":3}"}"#;
        let raw = serde_json::Value::String(inner.to_string());
        let out = parse_arguments_value(Some(&raw));
        assert_eq!(out["params"], serde_json::json!({"maxResults": 3}));
    }

    #[test]
    fn parse_tool_call_value_handles_gemini_double_encoded_params() {
        let inner = r#"{"service":"gmail","resource":"users","sub_resource":"messages","method":"list","params":"{\"maxResults\":3}"}"#;
        let call_json = serde_json::json!({
            "function": {
                "name": "google_workspace",
                "arguments": inner
            }
        });
        let parsed = parse_tool_call_value(&call_json).expect("expected a parsed call");
        assert_eq!(parsed.name, "google_workspace");
        assert_eq!(
            parsed.arguments["params"],
            serde_json::json!({"maxResults": 3})
        );
        assert_eq!(
            parsed.arguments["sub_resource"],
            serde_json::json!("messages")
        );
    }

    #[test]
    fn parse_tool_calls_extracts_multiple_calls() {
        let response = r#"<tool_call>
{"name": "file_read", "arguments": {"path": "a.txt"}}
</tool_call>
<tool_call>
{"name": "file_read", "arguments": {"path": "b.txt"}}
</tool_call>"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_returns_text_only_when_no_calls() {
        let response = "Just a normal response with no tools.";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Just a normal response with no tools.");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_malformed_json() {
        let response = r#"<tool_call>
not valid json
</tool_call>
Some text after."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.contains("Some text after."));
    }

    #[test]
    fn parse_tool_calls_text_before_and_after() {
        let response = r#"Before text.
<tool_call>
{"name": "shell", "arguments": {"command": "echo hi"}}
</tool_call>
After text."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Before text."));
        assert!(text.contains("After text."));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn parse_tool_calls_handles_openai_format() {
        // OpenAI-style response with tool_calls array
        let response = r#"{"content": "Let me check that for you.", "tool_calls": [{"type": "function", "function": {"name": "shell", "arguments": "{\"command\": \"ls -la\"}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that for you.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_openai_format_multiple_calls() {
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"a.txt\"}"}}, {"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"b.txt\"}"}}]}"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_openai_format_without_content() {
        // Some model_providers don't include content field with tool_calls
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "memory_recall", "arguments": "{}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty()); // No content field
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
    }

    #[test]
    fn parse_tool_calls_preserves_openai_tool_call_ids() {
        let response = r#"{"tool_calls":[{"id":"call_42","function":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}]}"#;
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_42"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_json_inside_tool_call_tag() {
        let response = r#"<tool_call>
```json
{"name": "file_write", "arguments": {"path": "test.py", "content": "print('ok')"}}
```
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "test.py"
        );
    }

    #[test]
    fn parse_tool_calls_handles_noisy_tool_call_tag_body() {
        let response = r#"<tool_call>
I will now call the tool with this payload:
{"name": "shell", "arguments": {"command": "pwd"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_inline_attributes_with_send_message_alias() {
        let response = r#"<tool_call>send_message channel="user_channel" message="Hello! How can I assist you today?"</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "user_channel"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "Hello! How can I assist you today?"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_function_style_arguments() {
        let response = r#"<tool_call>message_send(channel="general", message="test")</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "general"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "test"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_nested_tool_payload() {
        let response = r#"<tool_call>
<memory_recall>
<query>project roadmap</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "project roadmap"
        );
    }

    #[test]
    fn parse_tool_calls_handles_plural_tool_calls_wrapper() {
        // Regression: Llama 4 Scout (via Groq) emits a plural `<tool_calls>`
        // wrapper rather than the singular `<tool_call>`. The parser must
        // enter it and execute the call instead of exposing raw XML.
        let (text, calls) = parse_tool_calls(
            "<tool_calls>\n{\"name\":\"myserver__some_tool\",\"arguments\":{\"key\":\"value\"}}\n</tool_calls>",
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "myserver__some_tool");
        assert_eq!(
            calls[0].arguments.get("key").unwrap().as_str().unwrap(),
            "value"
        );
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_ignores_xml_thinking_wrapper() {
        let response = r#"<tool_call>
<thinking>Need to inspect memory first</thinking>
<memory_recall>
<query>recent deploy notes</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "recent deploy notes"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_with_json_arguments() {
        let response = r#"<tool_call>
<shell>{"command":"pwd"}</shell>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_fence() {
        let response = r#"I'll check that.
```tool_call
{"name": "shell", "arguments": {"command": "pwd"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert!(text.contains("I'll check that."));
        assert!(text.contains("Done."));
        assert!(!text.contains("```tool_call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_hybrid_close_tag() {
        let response = r#"Preface
```tool-call
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert!(!text.contains("```tool-call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_invoke_fence() {
        let response = r#"Checking.
```invoke
{"name": "shell", "arguments": {"command": "date"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Checking."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_format() {
        //: xAI grok models use ```tool <name> format
        let response = r#"I'll write a test file.
```tool file_write
{"path": "/home/user/test.txt", "content": "Hello world"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/home/user/test.txt"
        );
        assert!(text.contains("I'll write a test file."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn malformed_tool_block_log_omits_model_controlled_content() {
        let _writer_guard = zeroclaw_log::__private_test_writer_lock();
        let _hook_guard = zeroclaw_log::__private_test_hook_lock();
        zeroclaw_log::try_install_capture_subscriber();
        let mut rx = zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let secret_name = "sk_live_SECRET_IDENTIFIER";
        let secret_body = "api_key=sk_live_SECRET_BODY";
        let malformed_payload = format!("{secret_body}\n");
        let expected_payload_len = malformed_payload.len() as u64;
        let response = format!("```tool {secret_name}\n{malformed_payload}```");

        let (_, calls) = parse_tool_calls(&response);
        assert!(calls.is_empty());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let event = 'search: loop {
            while let Ok(event) = rx.try_recv() {
                let matches_message = event.get("message").and_then(|value| value.as_str())
                    == Some("Found ```tool <name> block but could not parse JSON arguments");
                let matches_source = event
                    .get("attributes")
                    .and_then(|attributes| attributes.get("_file"))
                    .and_then(|value| value.as_str())
                    .is_some_and(|file| {
                        file.replace('\\', "/")
                            .ends_with("zeroclaw-tool-call-parser/src/lib.rs")
                    });
                if matches_message && matches_source {
                    break 'search event;
                }
            }

            assert!(
                std::time::Instant::now() < deadline,
                "malformed tool block should emit the expected canonical log event"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let serialized = event.to_string();
        assert!(!serialized.contains(secret_name));
        assert!(!serialized.contains(secret_body));
        assert!(event["attributes"].get("tool_name").is_none());
        assert_eq!(
            event["attributes"]["payload_len"].as_u64(),
            Some(expected_payload_len)
        );
    }

    #[test]
    fn parse_tool_calls_recovers_malformed_file_write_content_quotes() {
        let response = r#"<tool_call>
{"name":"file_write","arguments":{"path":"index.html","content":"<section class="hero"><script>const msg = "ok";</script></section>"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "index.html"
        );
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            r#"<section class="hero"><script>const msg = "ok";</script></section>"#
        );
    }

    #[test]
    fn parse_tool_calls_recovers_malformed_file_write_tool_name_fence() {
        let response = r#"```tool file_write
{"path":"index.html","content":"<div data-kind="card">ok</div>"}
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            r#"<div data-kind="card">ok</div>"#
        );
    }

    #[test]
    fn parse_tool_calls_recovers_malformed_file_write_non_ascii_safely() {
        let response = r#"说明:
<tool_call>
{"name":"file_write","arguments":{"path":"页面.html","content":"<p title="问候">你好，世界 🌏</p>"}}
</tool_call>
完成"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("说明"));
        assert!(text.contains("完成"));
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "页面.html"
        );
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            r#"<p title="问候">你好，世界 🌏</p>"#
        );
    }

    #[test]
    fn parse_tool_calls_rejects_ambiguous_malformed_file_write() {
        let response = r#"<tool_call>
{"name":"file_write","arguments":{"path":"index.html","content":"<section class="hero">","mode":"append"}}
</tool_call>"#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_valid_file_write_json_unchanged() {
        let response = r#"{"name":"file_write","arguments":{"path":"index.html","content":"<section class=\"hero\">ok</section>"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            r#"<section class="hero">ok</section>"#
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_shell() {
        //: Test shell command in ```tool shell format
        let response = r#"```tool shell
{"command": "ls -la"}
```"#;

        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_multiple_tool_name_fences() {
        // Multiple tool calls in ```tool <name> format
        let response = r#"First, I'll write a file.
```tool file_write
{"path": "/tmp/a.txt", "content": "A"}
```
Then read it.
```tool file_read
{"path": "/tmp/a.txt"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[1].name, "file_read");
        assert!(text.contains("First, I'll write a file."));
        assert!(text.contains("Then read it."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_toolcall_tag_alias() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tools_tag_alias() {
        // Qwen2.5-Coder-32B wraps a well-formed Hermes call in the tool
        // *declaration* tag rather than the invocation tag. Observed
        // deterministically (6/6 across two independent runs, and independent
        // of how many tools are offered), so the call is recoverable and should
        // not be dropped as prose.
        let response = r#"<tools>
{"name": "get_weather", "arguments": {"city": "Paris"}}
</tools>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(
            calls[0].arguments.get("city").unwrap().as_str().unwrap(),
            "Paris"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_tools_declaration_block() {
        // `<tools>` is ALSO the Hermes tag that DECLARES the available tools.
        // A declaration is an array of schemas -- `description` / `parameters`,
        // no `arguments` -- and must never be executed as an invocation.
        let response = r#"<tools>
[{"name": "get_weather", "description": "Get the current weather for a city.", "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}]
</tools>"#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "a tool DECLARATION must not be parsed as an invocation, got {calls:?}"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_tools_block_discussed_in_prose() {
        // An assistant explaining the Hermes format must not trigger a call.
        let response = r#"In the Hermes prompt format the available tools are declared like this:

<tools>
[{"name": "shell", "description": "Run a command", "parameters": {}}]
</tools>

and the model then replies with a <tool_call> block."#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "prose describing the format must not be parsed as an invocation, got {calls:?}"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_echoed_system_prompt_tools_block() {
        // Some models echo their own system prompt. That echo contains the
        // declaration block verbatim and must stay inert.
        let response = r#"You are a helpful assistant with access to the following functions.
<tools>
[{"type": "function", "function": {"name": "file_read", "description": "Read a file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}}]
</tools>
Use them when appropriate."#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "an echoed system prompt must not be parsed as an invocation, got {calls:?}"
        );
    }

    #[test]
    fn tools_wrapper_does_not_reach_glm_shortened_body_on_matching_close() {
        // GLM shortened bodies are executable legacy syntax: `shell>cmd` becomes
        // a shell call under the unambiguous tags. A value-shaped admission rule
        // cannot see it at all, because such a body never parses as JSON -- which
        // is why `<tools>` carries canonical JSON or nothing, rather than being
        // filtered on the way out of the legacy parsers.
        let response = "<tools>shell>rm -rf /tmp/x</tools>";
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "GLM shortened body under <tools> must stay inert, got {calls:?}"
        );
    }

    #[test]
    fn tools_wrapper_does_not_reach_glm_shortened_body_on_foreign_close() {
        let response = "<tools>shell>rm -rf /tmp/x</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "GLM body under <tools> with a foreign close must stay inert, got {calls:?}"
        );
    }

    #[test]
    fn tools_wrapper_does_not_reach_glm_shortened_body_when_unclosed() {
        let response = "<tools>shell>rm -rf /tmp/x";
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "unclosed GLM body under <tools> must stay inert, got {calls:?}"
        );
    }

    #[test]
    fn glm_shortened_body_still_works_under_an_unambiguous_tag() {
        // Positive control for the restriction: gating <tools> must not disable
        // legacy recovery for the tags that are not overloaded.
        let response = "<tool_call>shell>uname -a</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "GLM shortened body must still parse under <tool_call>"
        );
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn parse_tool_calls_rejects_tools_declaration_closed_by_foreign_alias() {
        // Mismatched close. The cross-alias recovery path used to parse this
        // body without consulting the <tools> guard, so closing a declaration
        // with a FOREIGN alias was enough to turn it into a call.
        let response = r#"<tools>
[{"name": "shell", "description": "Run a command", "parameters": {}}]
</tool_call>"#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "a declaration closed by a foreign alias must stay inert, got {calls:?}"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_unclosed_tools_declaration() {
        // Missing close. Truncation mid-stream reaches the brace-balancing
        // recovery path, which was likewise unguarded.
        let response = r#"<tools>
[{"name": "shell", "description": "Run a command", "parameters": {}}]"#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "an unclosed declaration must stay inert, got {calls:?}"
        );
    }

    #[test]
    fn parse_tool_calls_rejects_tools_wrapper_with_args_key() {
        // `args` is not the canonical key: the parser reads `arguments`. When
        // the admission predicate accepted `args`, this was admitted as an
        // invocation and then dispatched with EMPTY arguments -- the runtime
        // received a different call from the one the model encoded. Staying
        // inert is correct; a corrupted call is not.
        let response = r#"<tools>
{"name": "shell", "args": {"command": "rm -rf /tmp/x"}}
</tools>"#;

        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "an `args`-shaped body must not be dispatched with empty arguments, got {calls:?}"
        );
    }

    #[test]
    fn parse_tool_calls_still_accepts_canonical_tools_invocation() {
        // Positive control: the motivating Qwen payload must keep working, so
        // the guards above cannot be satisfied by simply rejecting everything.
        let response = r#"<tools>
{"name": "shell", "arguments": {"command": "uname -a"}}
</tools>"#;

        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1, "canonical invocation must still parse");
        assert_eq!(calls[0].name, "shell");
        assert!(
            calls[0].arguments.get("command").is_some(),
            "arguments must be preserved, got {:?}",
            calls[0].arguments
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_dash_call_tag_alias() {
        let response = r#"<tool-call>
{"name": "shell", "arguments": {"command": "whoami"}}
</tool-call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "whoami"
        );
    }

    #[test]
    fn parse_tool_calls_handles_invoke_tag_alias() {
        let response = r#"<invoke>
{"name": "shell", "arguments": {"command": "uptime"}}
</invoke>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_parameter_format() {
        let response = r#"<minimax:tool_call>
<invoke name="shell">
<parameter name="command">sqlite3 /tmp/test.db ".tables"</parameter>
</invoke>
</minimax:tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"sqlite3 /tmp/test.db ".tables""#
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_with_surrounding_text() {
        let response = r#"Preface
<minimax:tool_call>
<invoke name='http_request'>
<parameter name='url'>https://example.com</parameter>
<parameter name='method'>GET</parameter>
</invoke>
</minimax:tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "http_request");
        assert_eq!(
            calls[0].arguments.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
        assert_eq!(
            calls[0].arguments.get("method").unwrap().as_str().unwrap(),
            "GET"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_toolcall_alias_and_cross_close_tag() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"date"}}
</minimax:toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_perl_style_tool_call_blocks() {
        let response = r#"TOOL_CALL
{tool => "shell", args => { --command "uname -a" }}}
/TOOL_CALL"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uname -a"
        );
    }

    #[test]
    fn parse_tool_calls_handles_square_bracket_tool_call_blocks() {
        let response =
            r#"[TOOL_CALL]{tool => "shell", args => {--command "echo hello"}}[/TOOL_CALL]"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "echo hello"
        );
    }

    #[test]
    fn parse_tool_calls_handles_square_bracket_multiline() {
        let response = r#"[TOOL_CALL]
{tool => "file_read", args => {
  --path "/tmp/test.txt"
  --description "Read test file"
}}
[/TOOL_CALL]"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/tmp/test.txt"
        );
        assert_eq!(
            calls[0]
                .arguments
                .get("description")
                .unwrap()
                .as_str()
                .unwrap(),
            "Read test file"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_unclosed_tool_call_with_json() {
        let response = r#"I will call the tool now.
<tool_call>
{"name": "shell", "arguments": {"command": "uptime -p"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I will call the tool now."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime -p"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_mismatched_close_tag() {
        let response = r#"<tool_call>
{"name": "shell", "arguments": {"command": "uptime"}}
</arg_value>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_cross_alias_closing_tags() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn parse_tool_calls_rejects_raw_tool_json_without_tags() {
        // SECURITY: Raw JSON without explicit wrappers should NOT be parsed
        // This prevents prompt injection attacks where malicious content
        // could include JSON that mimics a tool call.
        let response = r#"Sure, creating the file now.
{"name": "file_write", "arguments": {"path": "hello.py", "content": "print('hello')"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Sure, creating the file now."));
        assert_eq!(
            calls.len(),
            0,
            "Raw JSON without wrappers should not be parsed"
        );
    }

    #[test]
    fn parse_tool_calls_handles_empty_tool_result() {
        // Recovery: Empty tool_result tag should be handled gracefully
        let response = r#"I'll run that command.
<tool_result name="shell">

</tool_result>
Done."#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Done."));
        assert!(calls.is_empty());
    }

    #[test]
    fn strip_tool_result_blocks_removes_single_block() {
        let input = r#"<tool_result name="memory_recall" status="ok">
{"matches":["hello"]}
</tool_result>
Here is my answer."#;
        assert_eq!(strip_tool_result_blocks(input), "Here is my answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_multiple_blocks() {
        let input = r#"<tool_result name="memory_recall" status="ok">
{"matches":[]}
</tool_result>
<tool_result name="shell" status="ok">
done
</tool_result>
Final answer."#;
        assert_eq!(strip_tool_result_blocks(input), "Final answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_prefix() {
        let input =
            "[Tool results]\n<tool_result name=\"shell\" status=\"ok\">\nok\n</tool_result>\nDone.";
        assert_eq!(strip_tool_result_blocks(input), "Done.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_thinking() {
        let input = "<thinking>\nLet me think...\n</thinking>\nHere is the answer.";
        assert_eq!(strip_tool_result_blocks(input), "Here is the answer.");
    }

    #[test]
    fn strip_tool_result_blocks_removes_think_tags() {
        let input = "<think>\nLet me reason...\n</think>\nHere is the answer.";
        assert_eq!(strip_tool_result_blocks(input), "Here is the answer.");
    }

    #[test]
    fn parse_tool_calls_strips_think_before_tool_call() {
        // Qwen regression: <think> tags before <tool_call> tags should be
        // stripped, allowing the tool call to be parsed correctly.
        let response = "<think>I need to list files to understand the project</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"ls\"}}\n</tool_call>";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should parse tool call after stripping think tags"
        );
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls"
        );
        assert!(text.is_empty(), "think content should not appear as text");
    }

    #[test]
    fn parse_tool_calls_strips_think_only_returns_empty() {
        // When response is only <think> tags with no tool calls, should
        // return empty text and no calls.
        let response = "<think>Just thinking, no action needed</think>";
        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_qwen_think_with_multiple_tool_calls() {
        let response = "<think>I need to check two things</think>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"date\"}}\n</tool_call>\n<tool_call>\n{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\n</tool_call>";
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert_eq!(
            calls[1].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn strip_tool_result_blocks_preserves_clean_text() {
        let input = "Hello, this is a normal response.";
        assert_eq!(strip_tool_result_blocks(input), input);
    }

    #[test]
    fn strip_tool_result_blocks_returns_empty_for_only_tags() {
        let input = "<tool_result name=\"memory_recall\" status=\"ok\">\n{}\n</tool_result>";
        assert_eq!(strip_tool_result_blocks(input), "");
    }

    #[test]
    fn parse_arguments_value_handles_null() {
        // Recovery: null arguments are returned as-is (Value::Null)
        let value = serde_json::json!(null);
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_null());
    }

    #[test]
    fn parse_tool_calls_handles_empty_tool_calls_array() {
        // Recovery: Empty tool_calls array returns original response (no tool parsing)
        let response = r#"{"content": "Hello", "tool_calls": []}"#;
        let (text, calls) = parse_tool_calls(response);
        // When tool_calls is empty, the entire JSON is returned as text
        assert!(text.contains("Hello"));
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_payloads() {
        let response =
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}</tool_call>";
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(
            issue.is_some(),
            "malformed tool payload should be flagged for diagnostics"
        );
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_normal_text() {
        let issue = detect_tool_call_parse_issue("Thanks, done.", &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_empty_tool_calls_array() {
        let issue = detect_tool_call_parse_issue(r#"{"content":"Hello","tool_calls":[]}"#, &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_json_fenced_business_tool_calls() {
        let response = r#"```json
{"tool_calls":[{"service":"billing","count":2}]}
```"#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_tool_call_fenced_example() {
        let response = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;

        let issue = detect_tool_call_parse_issue(response, &[]);

        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_standalone_tool_call_fence() {
        let response = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;

        let issue = detect_tool_call_parse_issue(response, &[]);

        assert!(issue.is_some());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_tool_call_tag_example() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;

        let issue = detect_tool_call_parse_issue(response, &[]);

        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_tagged_tool_call_with_trailing_text() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
Done."#;

        let issue = detect_tool_call_parse_issue(response, &[]);

        assert!(issue.is_some());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_json_fenced_tool_protocol() {
        let response = r#"```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```"#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(issue.is_some());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_tool_result_envelope() {
        let response = r#"{"tool_call_id":"call_1","content":"raw tool output""#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(issue.is_some());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_malformed_tool_call_id_only_json() {
        let response = r#"{"tool_call_id":"support-case-1""#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_nonempty_tool_calls_array() {
        let issue = detect_tool_call_parse_issue(
            r#"{"content":null,"tool_calls":[{"call_id":"call_1","arguments":"{}"}]}"#,
            &[],
        );
        assert!(issue.is_some());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_malformed_business_tool_calls_without_call_id() {
        for response in [
            r#"{"tool_calls":[{"name":"support_case","arguments":{"id":"A1"}}"#,
            r#"{"toolcalls":[{"name":"support_case","arguments":{"id":"A1"}}"#,
        ] {
            let issue = detect_tool_call_parse_issue(response, &[]);

            assert!(
                issue.is_none(),
                "business JSON without a tool call id must not be treated as internal protocol: {response}"
            );
            assert!(
                !looks_like_malformed_tool_protocol_envelope(response),
                "business JSON without a tool call id must not be classified as malformed protocol: {response}"
            );
        }
    }

    #[test]
    fn looks_like_tool_protocol_envelope_flags_malformed_nonempty_tool_calls_array() {
        assert!(looks_like_tool_protocol_envelope(
            r#"{"content":null,"tool_calls":[{"call_id":"call_1","arguments":"{}"}]}"#
        ));
        assert!(!looks_like_tool_protocol_envelope(
            r#"{"content":"Hello","tool_calls":[]}"#
        ));
    }

    #[test]
    fn classify_tool_protocol_envelope_flags_internal_json_variants() {
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{}"}]}"#
            ),
            Some(ToolProtocolEnvelopeKind::ToolCalls)
        );
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"{"toolcalls":[{"name":"shell","arguments":{"command":"pwd"}}]}"#
            ),
            Some(ToolProtocolEnvelopeKind::ToolCallsAlias)
        );
        assert_eq!(
            classify_tool_protocol_envelope(r#"{"tool_calls":[{"name":"shell","arguments":{}}]}"#),
            Some(ToolProtocolEnvelopeKind::ToolCalls)
        );
        assert_eq!(
            classify_tool_protocol_envelope(r#"{"toolcalls":[{"name":"shell","arguments":{}}]}"#),
            Some(ToolProtocolEnvelopeKind::ToolCallsAlias)
        );
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"{"function_call":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}"#
            ),
            Some(ToolProtocolEnvelopeKind::FunctionCall)
        );
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"{"tool_call_id":"call_1","content":"command output"}"#
            ),
            Some(ToolProtocolEnvelopeKind::ToolResult)
        );
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"{"type":"function_call","call_id":"call_1","name":"shell","arguments":"{}"}"#
            ),
            Some(ToolProtocolEnvelopeKind::ResponsesFunctionCall)
        );
        assert_eq!(
            classify_tool_protocol_envelope(
                r#"```json
{"tool_calls":[{"name":"shell","arguments":{"command":"pwd"}}]}
```"#
            ),
            Some(ToolProtocolEnvelopeKind::ToolCalls)
        );
    }

    #[test]
    fn classify_tool_protocol_envelope_preserves_tool_call_examples() {
        let fenced_example = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
This is an example, not an invocation."#;
        let embedded_fenced_example = r#"Here is an example:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
        let embedded_fenced_example_cn = r#"例如：
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
        let tag_example = r#"<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>
This is an example, not an invocation."#;
        let tag_example_cn = r#"比如：
<tool_call>
{"name":"shell","arguments":{"command":"pwd"}}
</tool_call>"#;

        assert_eq!(classify_tool_protocol_envelope(fenced_example), None);
        assert!(!looks_like_tool_protocol_envelope(fenced_example));
        assert_eq!(
            classify_tool_protocol_envelope(embedded_fenced_example),
            None
        );
        assert!(!looks_like_tool_protocol_envelope(embedded_fenced_example));
        assert!(looks_like_tool_protocol_example(embedded_fenced_example));
        assert_eq!(
            classify_tool_protocol_envelope(embedded_fenced_example_cn),
            None
        );
        assert!(!looks_like_tool_protocol_envelope(
            embedded_fenced_example_cn
        ));
        assert!(looks_like_tool_protocol_example(embedded_fenced_example_cn));
        assert_eq!(classify_tool_protocol_envelope(tag_example), None);
        assert!(!looks_like_tool_protocol_envelope(tag_example));
        assert_eq!(classify_tool_protocol_envelope(tag_example_cn), None);
        assert!(!looks_like_tool_protocol_envelope(tag_example_cn));
        assert!(looks_like_tool_protocol_example(tag_example_cn));
    }

    #[test]
    fn contains_tool_protocol_tag_call_flags_embedded_tool_call_fences() {
        let embedded = r#"Let me call it:
```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```
Done."#;

        assert!(contains_tool_protocol_tag_call(embedded));
    }

    #[test]
    fn classify_tool_protocol_envelope_flags_standalone_tool_fences() {
        let tool_call_fence = r#"```tool_call
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
        let invoke_fence = r#"```invoke
{"name":"shell","arguments":{"command":"pwd"}}
```"#;
        let tool_name_fence = r#"```tool shell
{"command":"pwd"}
```"#;

        assert_eq!(
            classify_tool_protocol_envelope(tool_call_fence),
            Some(ToolProtocolEnvelopeKind::TaggedToolCall)
        );
        assert!(looks_like_tool_protocol_envelope(tool_call_fence));
        assert_eq!(
            classify_tool_protocol_envelope(invoke_fence),
            Some(ToolProtocolEnvelopeKind::TaggedToolCall)
        );
        assert!(looks_like_tool_protocol_envelope(invoke_fence));
        assert_eq!(
            classify_tool_protocol_envelope(tool_name_fence),
            Some(ToolProtocolEnvelopeKind::TaggedToolCall)
        );
        assert!(looks_like_tool_protocol_envelope(tool_name_fence));
    }

    #[test]
    fn classify_tool_protocol_envelope_preserves_top_level_arrays_without_protocol_marker() {
        assert!(!looks_like_tool_protocol_envelope(
            r#"[{"service":"billing","count":2}]"#
        ));

        assert!(!looks_like_tool_protocol_envelope(
            r#"[{"name":"shell","arguments":{}}]"#
        ));
    }

    #[test]
    fn classify_tool_protocol_envelope_preserves_top_level_schema_array() {
        let schema = r#"[{"name":"planner","parameters":{"goal":"string"}}]"#;

        assert_eq!(classify_tool_protocol_envelope(schema), None);
        assert!(!looks_like_tool_protocol_envelope(schema));
    }

    #[test]
    fn classify_tool_protocol_envelope_preserves_plain_user_json() {
        let profile = r#"{"name":"profile","parameters":{"timezone":"UTC"}}"#;
        assert_eq!(classify_tool_protocol_envelope(profile), None);
        assert!(!looks_like_tool_protocol_envelope(profile));
    }

    #[test]
    fn looks_like_tool_protocol_envelope_preserves_plain_json_with_similar_keys() {
        let config = r#"{"function_call":false,"description":"disable the feature"}"#;
        assert!(!looks_like_tool_protocol_envelope(config));

        let audit_log = r#"{"tool_calls":[{"service":"billing","count":2}]}"#;
        assert!(!looks_like_tool_protocol_envelope(audit_log));

        let queued_case =
            r#"{"tool_calls":[{"id":"case-1","status":"queued","service":"billing"}]}"#;
        assert!(!looks_like_tool_protocol_envelope(queued_case));

        let named_record =
            r#"{"tool_calls":[{"name":"planner","status":"queued","service":"workflow"}]}"#;
        assert!(!looks_like_tool_protocol_envelope(named_record));
    }

    #[test]
    fn parse_tool_calls_handles_whitespace_only_name() {
        // Recovery: Whitespace-only tool name should return None
        let value = serde_json::json!({"function": {"name": "   ", "arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_calls_handles_empty_string_arguments() {
        // Recovery: Empty string arguments should be handled
        let value = serde_json::json!({"name": "test", "arguments": ""});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test");
    }

    #[test]
    fn parse_arguments_value_handles_invalid_json_string() {
        // Recovery: Invalid JSON string should return empty object
        let value = serde_json::Value::String("not valid json".to_string());
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_none() {
        // Recovery: None arguments should return empty object
        let result = parse_arguments_value(None);
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_empty_array() {
        // Recovery: Empty tool_calls array should return empty vec
        let value = serde_json::json!({"tool_calls": []});
        let result = parse_tool_calls_from_json_value(&value);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_missing_tool_calls() {
        // Recovery: Missing tool_calls field should fall through
        let value = serde_json::json!({"name": "test", "arguments": {}});
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_top_level_array() {
        // Recovery: Top-level array of tool calls
        let value = serde_json::json!([
            {"name": "tool_a", "arguments": {}},
            {"name": "tool_b", "arguments": {}}
        ]);
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_glm_style_browser_open_url() {
        let response = "browser_open/url>https://example.com";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "curl -s 'https://example.com'");
    }

    #[test]
    fn parse_glm_style_quotes_url_apostrophes_and_metacharacters() {
        let calls =
            parse_glm_style_tool_calls("browser_open/url>https://example.com/it's;still=one");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(
            calls[0].1["command"],
            r#"curl -s 'https://example.com/it'"'"'s;still=one'"#
        );
    }

    #[test]
    fn parse_glm_style_shell_command() {
        let response = "shell/command>ls -la";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "ls -la");
    }

    #[test]
    fn parse_glm_style_http_request() {
        let response = "http_request/url>https://api.example.com/data";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http_request");
        assert_eq!(calls[0].1["url"], "https://api.example.com/data");
        assert_eq!(calls[0].1["method"], "GET");
    }

    #[test]
    fn parse_glm_style_ignores_plain_url() {
        // A bare URL should NOT be interpreted as a tool call — this was
        // causing false positives when LLMs included URLs in normal text.
        let response = "https://example.com/api";
        let calls = parse_glm_style_tool_calls(response);
        assert!(
            calls.is_empty(),
            "plain URL must not be parsed as tool call"
        );
    }

    #[test]
    fn parse_glm_style_json_args() {
        let response = r#"shell/{"command": "echo hello"}"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "echo hello");
    }

    #[test]
    fn parse_glm_style_multiple_calls() {
        let response = r#"shell/command>ls
browser_open/url>https://example.com"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn parse_glm_style_tool_call_integration() {
        // Integration test: GLM format should be parsed in parse_tool_calls
        let response = "Checking...\nbrowser_open/url>https://example.com\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(text.contains("Checking"));
        assert!(text.contains("Done"));
    }

    #[test]
    fn parse_glm_style_rejects_non_http_url_param() {
        let response = "browser_open/url>javascript:alert(1)";
        let calls = parse_glm_style_tool_calls(response);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_unclosed_tool_call_tag() {
        let response = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert_eq!(text, "Done");
    }

    #[test]
    fn parse_tool_calls_empty_input_returns_empty() {
        let (text, calls) = parse_tool_calls("");
        assert!(calls.is_empty(), "empty input should produce no tool calls");
        assert!(text.is_empty(), "empty input should produce no text");
    }

    #[test]
    fn parse_tool_calls_whitespace_only_returns_empty_calls() {
        let (text, calls) = parse_tool_calls("   \n\t  ");
        assert!(calls.is_empty());
        assert!(text.is_empty() || text.trim().is_empty());
    }

    #[test]
    fn parse_tool_calls_nested_xml_tags_handled() {
        // Double-wrapped tool call should still parse the inner call
        let response = r#"<tool_call><tool_call>{"name":"echo","arguments":{"msg":"hi"}}</tool_call></tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        // Should find at least one tool call
        assert!(
            !calls.is_empty(),
            "nested XML tags should still yield at least one tool call"
        );
    }

    #[test]
    fn parse_tool_calls_truncated_json_no_panic() {
        // Incomplete JSON inside tool_call tags
        let response = r#"<tool_call>{"name":"shell","arguments":{"command":"ls"</tool_call>"#;
        let (_text, _calls) = parse_tool_calls(response);
        // Should not panic — graceful handling of truncated JSON
    }

    #[test]
    fn parse_tool_calls_empty_json_object_in_tag() {
        let response = "<tool_call>{}</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        // Empty JSON object has no name field — should not produce valid tool call
        assert!(
            calls.is_empty(),
            "empty JSON object should not produce a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_closing_tag_only_returns_text() {
        let response = "Some text </tool_call> more text";
        let (text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "closing tag only should not produce calls"
        );
        assert!(
            !text.is_empty(),
            "text around orphaned closing tag should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_very_large_arguments_no_panic() {
        let large_arg = "x".repeat(100_000);
        let response = format!(
            r#"<tool_call>{{"name":"echo","arguments":{{"message":"{}"}}}}</tool_call>"#,
            large_arg
        );
        let (_text, calls) = parse_tool_calls(&response);
        assert_eq!(calls.len(), 1, "large arguments should still parse");
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_special_characters_in_arguments() {
        let response = r#"<tool_call>{"name":"echo","arguments":{"message":"hello \"world\" <>&'\n\t"}}</tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_text_with_embedded_json_not_extracted() {
        // Raw JSON without any tags should NOT be extracted as a tool call
        let response = r#"Here is some data: {"name":"echo","arguments":{"message":"hi"}} end."#;
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "raw JSON in text without tags should not be extracted"
        );
    }

    #[test]
    fn parse_tool_calls_multiple_formats_mixed() {
        // Mix of text and properly tagged tool call
        let response = r#"I'll help you with that.

<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>

Let me check the result."#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract one tool call from mixed content"
        );
        assert_eq!(calls[0].name, "shell");
        assert!(
            text.contains("help you"),
            "text before tool call should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_json() {
        // <tool_call> opened but closed with </invoke> — JSON body
        let input = r#"<tool_call>{"name": "shell", "arguments": {"command": "ls"}}</invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_glm_shortened() {
        // <tool_call>shell>uname -a</invoke> — GLM shortened inside cross-alias tags
        let input = "<tool_call>shell>uname -a</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_shortened_body_in_matched_tags() {
        // <tool_call>shell>pwd</tool_call> — GLM shortened in matched tags
        let input = "<tool_call>shell>pwd</tool_call>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_yaml_style_in_tags() {
        // <tool_call>shell>\ncommand: date\napproved: true</invoke>
        let input = "<tool_call>shell>\ncommand: date\napproved: true</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert_eq!(calls[0].arguments["approved"], true);
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_attribute_style_in_tags() {
        // <tool_call>shell command="date" /></tool_call>
        let input = r#"<tool_call>shell command="date" /></tool_call>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_file_read_shortened_in_cross_alias() {
        // <tool_call>file_read path=".env" /></invoke>
        let input = r#"<tool_call>file_read path=".env" /></invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], ".env");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_unclosed_glm_shortened_no_close_tag() {
        // <tool_call>shell>ls -la (no close tag at all)
        let input = "<tool_call>shell>ls -la";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_text_before_cross_alias() {
        // Text before and after cross-alias tool call
        let input = "Let me check that.\n<tool_call>shell>uname -a</invoke>\nDone.";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.contains("Let me check that."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_glm_shortened_body_url_to_curl() {
        // URL values for shell should be wrapped in curl
        let call = parse_glm_shortened_body("shell>https://example.com/api").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_browser_open_maps_to_shell_command() {
        // browser_open aliases to shell, and shortened calls must still emit
        // shell's canonical "command" argument.
        let call = parse_glm_shortened_body("browser_open>https://example.com").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_memory_recall() {
        // memory_recall>some query — default param is "query"
        let call = parse_glm_shortened_body("memory_recall>recent meetings").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "recent meetings");
    }

    #[test]
    fn parse_glm_shortened_body_function_style_alias_maps_to_message_send() {
        let call =
            parse_glm_shortened_body(r#"sendmessage(channel="alerts", message="hi")"#).unwrap();
        assert_eq!(call.name, "message_send");
        assert_eq!(call.arguments["channel"], "alerts");
        assert_eq!(call.arguments["message"], "hi");
    }

    #[test]
    fn parse_glm_shortened_body_rejects_empty() {
        assert!(parse_glm_shortened_body("").is_none());
        assert!(parse_glm_shortened_body("   ").is_none());
    }

    #[test]
    fn parse_glm_shortened_body_rejects_invalid_tool_name() {
        // Tool names with special characters should be rejected
        assert!(parse_glm_shortened_body("not-a-tool>value").is_none());
        assert!(parse_glm_shortened_body("tool name>value").is_none());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_includes_reasoning_content() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls(
            "answer",
            &calls,
            Some("deep thought"),
        );
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("deep thought"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_omits_reasoning_content_when_none() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls("answer", &calls, None);
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════

    // ═══════════════════════════════════════════════════════════════════════
    // Additional parser internals tests (moved from zeroclaw-runtime to keep
    // functions crate-private per Beta-tier API stability policy)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_call_value_handles_missing_name_field() {
        let value = serde_json::json!({"function": {"arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_call_value_handles_top_level_name() {
        let value = serde_json::json!({"name": "test_tool", "arguments": {}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test_tool");
    }

    #[test]
    fn parse_tool_call_value_accepts_top_level_parameters_alias() {
        let value = serde_json::json!({
            "name": "schedule",
            "parameters": {"action": "create", "message": "test"}
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "schedule");
        assert_eq!(
            result.arguments.get("action").and_then(|v| v.as_str()),
            Some("create")
        );
    }

    #[test]
    fn parse_tool_call_value_accepts_function_parameters_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "parameters": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("date")
        );
    }

    #[test]
    fn parse_tool_call_value_preserves_tool_call_id_aliases() {
        let value = serde_json::json!({
            "call_id": "legacy_1",
            "function": {
                "name": "shell",
                "arguments": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.tool_call_id.as_deref(), Some("legacy_1"));
    }

    #[test]
    fn extract_json_values_handles_empty_string() {
        let result = extract_json_values("");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_whitespace_only() {
        let result = extract_json_values(
            "   
	  ",
        );
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_multiple_objects() {
        let input = r#"{"a": 1}{"b": 2}{"c": 3}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn extract_json_values_handles_arrays() {
        let input = r#"[1, 2, 3]{"key": "value"}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn map_tool_name_alias_direct_coverage() {
        assert_eq!(map_tool_name_alias("bash"), "shell");
        assert_eq!(map_tool_name_alias("filelist"), "file_list");
        assert_eq!(map_tool_name_alias("memorystore"), "memory_store");
        assert_eq!(map_tool_name_alias("memoryforget"), "memory_forget");
        assert_eq!(map_tool_name_alias("http"), "http_request");
        assert_eq!(
            map_tool_name_alias("totally_unknown_tool"),
            "totally_unknown_tool"
        );
    }

    #[test]
    fn map_tool_name_alias_strips_dotted_namespaces() {
        // Gemini-style static prefixes still work.
        assert_eq!(map_tool_name_alias("default_api.file_read"), "file_read");
        assert_eq!(map_tool_name_alias("tools.shell"), "shell");

        // MCP-server-name prefixes (Gemini-via-OpenRouter also emits these
        // when the tool originates from an MCP server; the registry is
        // indexed by bare tool name, so we must strip them too).
        assert_eq!(
            map_tool_name_alias("google_workspace.search_gmail_messages"),
            "search_gmail_messages"
        );

        // Only the final segment is kept even with multiple dots.
        assert_eq!(map_tool_name_alias("a.b.c.final"), "final");

        // Stripped segment still runs through the alias table.
        assert_eq!(map_tool_name_alias("default_api.bash"), "shell");

        // Names without any dot are unaffected.
        assert_eq!(map_tool_name_alias("file_read"), "file_read");
    }

    #[test]
    fn default_param_for_tool_coverage() {
        assert_eq!(default_param_for_tool("shell"), "command");
        assert_eq!(default_param_for_tool("bash"), "command");
        assert_eq!(default_param_for_tool("file_read"), "path");
        assert_eq!(default_param_for_tool("memory_recall"), "query");
        assert_eq!(default_param_for_tool("memory_store"), "content");
        assert_eq!(default_param_for_tool("web_search_tool"), "query");
        assert_eq!(default_param_for_tool("web_search"), "query");
        assert_eq!(default_param_for_tool("search"), "query");
        assert_eq!(default_param_for_tool("http_request"), "url");
        assert_eq!(default_param_for_tool("browser_open"), "url");
        assert_eq!(default_param_for_tool("unknown_tool"), "input");
    }

    #[test]
    fn strip_trailing_terminal_markers_basic() {
        assert_eq!(strip_trailing_terminal_markers("Summary<eom>"), "Summary");
        assert_eq!(strip_trailing_terminal_markers("Summary<|eom|>"), "Summary");
    }

    #[test]
    fn strip_trailing_terminal_markers_preserves_unmarked_whitespace() {
        // No marker present: trailing whitespace is ordinary text and must be
        // preserved, matching the streaming path which never trims whitespace
        // that is not part of a marker suffix.
        assert_eq!(strip_trailing_terminal_markers("Answer\n"), "Answer\n");
        assert_eq!(strip_trailing_terminal_markers("Answer  "), "Answer  ");
        assert_eq!(
            strip_trailing_terminal_markers("Plain response\n\n"),
            "Plain response\n\n"
        );
    }

    #[test]
    fn strip_trailing_terminal_markers_whitespace_before_marker() {
        // Whitespace BEFORE a recognized marker belongs to the response text
        // and is preserved, matching the streaming stripper ("Answer\n<eom>"
        // streams as "Answer\n"). Only the marker itself (plus any whitespace
        // that followed it) is removed.
        assert_eq!(
            strip_trailing_terminal_markers("Summary  <eom>"),
            "Summary  "
        );
        assert_eq!(
            strip_trailing_terminal_markers("Summary \n\t<|eom|>"),
            "Summary \n\t"
        );
    }

    #[test]
    fn strip_trailing_terminal_markers_stacked() {
        assert_eq!(
            strip_trailing_terminal_markers("Summary<eom><|eom|>"),
            "Summary"
        );
        assert_eq!(
            strip_trailing_terminal_markers("Summary<|eom|><eom>"),
            "Summary"
        );
    }

    #[test]
    fn strip_trailing_terminal_markers_with_whitespace() {
        assert_eq!(
            strip_trailing_terminal_markers("Summary<eom>  \n"),
            "Summary"
        );
        assert_eq!(
            strip_trailing_terminal_markers("Summary<eom>\t\n  "),
            "Summary"
        );
        assert_eq!(
            strip_trailing_terminal_markers("Summary<eom>           <|eom|>"),
            "Summary"
        );
    }

    #[test]
    fn strip_trailing_terminal_markers_preserves_inline() {
        assert_eq!(
            strip_trailing_terminal_markers("Text with <eom> inline"),
            "Text with <eom> inline"
        );
        assert_eq!(
            strip_trailing_terminal_markers("Code: <|eom|> here"),
            "Code: <|eom|> here"
        );
    }

    #[test]
    fn strip_trailing_terminal_markers_empty() {
        assert_eq!(strip_trailing_terminal_markers(""), "");
        // Pure whitespace with no marker is ordinary text and is preserved,
        // matching the streaming path (which never trims unmarked whitespace).
        assert_eq!(strip_trailing_terminal_markers("   "), "   ");
        assert_eq!(strip_trailing_terminal_markers("<eom>"), "");
    }

    #[test]
    fn strip_trailing_terminal_markers_marker_only_with_whitespace() {
        assert_eq!(strip_trailing_terminal_markers("<eom>\n"), "");
        assert_eq!(strip_trailing_terminal_markers("<|eom|>  "), "");
        assert_eq!(strip_trailing_terminal_markers("<eom>\n<|eom|>"), "");
    }
}
