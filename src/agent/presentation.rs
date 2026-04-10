//! LLM presentation layer for tool output.
//!
//! Sits between tool execution and LLM-facing result formatting.
//! Applies four transformations:
//! 1. Strip ANSI escape codes (prevents garbage tokens)
//! 2. Flatten JSON responses to pipe-delimited plain text (Gemma 4 compat)
//! 3. Overflow handling (truncate + save to file + exploration hints)
//! 4. Metadata footer (exit code or ok/err + duration)

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub use crate::config::schema::PresentationConfig;
use crate::tools::ToolSpec;

static OVERFLOW_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Tools that use shell-style exit codes (0/1) in the metadata footer.
/// All other tools use "ok"/"err" labels instead.
const SHELL_LIKE_TOOLS: &[&str] = &["shell", "bg_run", "bg_status", "process"];

/// Strip ANSI escape codes from tool output.
pub fn strip_ansi(s: &str) -> String {
    strip_ansi_escapes::strip_str(s).to_string()
}

fn handle_overflow(output: &str, config: &PresentationConfig) -> String {
    let line_count = output.lines().count();
    let byte_count = output.len();

    if line_count <= config.max_output_lines && byte_count <= config.max_output_bytes {
        return output.to_string();
    }

    let overflow_path = save_overflow(output, &config.overflow_dir);

    // Truncate to max_output_lines, then enforce byte limit
    let mut truncated: String = output
        .lines()
        .take(config.max_output_lines)
        .collect::<Vec<_>>()
        .join("\n");

    // Safety: if a few long lines still exceed the byte limit, byte-truncate
    if truncated.len() > config.max_output_bytes {
        let mut cutoff = config.max_output_bytes;
        while cutoff > 0 && !truncated.is_char_boundary(cutoff) {
            cutoff -= 1;
        }
        truncated.truncate(cutoff);
    }

    let human_size = format_bytes(byte_count);
    let path_str = overflow_path.display();

    format!(
        "{truncated}\n\n--- output truncated ({line_count} lines, {human_size}) ---\n\
         Full output: {path_str}\n\
         Explore: cat {path_str} | grep <pattern>\n\
         \x20        cat {path_str} | tail 100"
    )
}

fn save_overflow(output: &str, overflow_dir: &str) -> PathBuf {
    let n = OVERFLOW_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = PathBuf::from(overflow_dir);
    fs::create_dir_all(&dir).ok();
    let path = dir.join(format!("cmd-{n}.txt"));
    fs::write(&path, output).ok();
    path
}

fn format_bytes(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1_048_576 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    }
}

fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

/// Maximum nesting depth for JSON flattening. Deeper values are stringified.
const FLATTEN_MAX_DEPTH: usize = 2;

/// Flatten a JSON object/array to pipe-delimited `key=value` plain text.
///
/// Returns the input unchanged if it doesn't parse as JSON.
/// Objects become `key=value | key=value`. Nested objects use dot notation
/// up to `FLATTEN_MAX_DEPTH`. Arrays of objects become one line per element.
///
/// CRITICAL: output must never contain `{`, `}`, or `"` — these collide with
/// Gemma 4's native `response:name{key:value}` and `<|"|>` token syntax.
pub fn flatten_json_output(output: &str) -> String {
    let trimmed = output.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return output.to_string();
    };

    match &value {
        serde_json::Value::Object(map) if map.is_empty() => "(empty)".to_string(),
        serde_json::Value::Object(map) => flatten_object(map, "", 0),
        serde_json::Value::Array(arr) if arr.is_empty() => "(empty list)".to_string(),
        serde_json::Value::Array(arr) => arr
            .iter()
            .enumerate()
            .map(|(i, v)| match v {
                serde_json::Value::Object(map) => {
                    format!("[{i}] {}", flatten_object(map, "", 0))
                }
                other => format!("[{i}] {}", format_value(other)),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => format_value(other),
    }
}

fn flatten_object(
    map: &serde_json::Map<String, serde_json::Value>,
    prefix: &str,
    depth: usize,
) -> String {
    let mut parts = Vec::new();
    for (key, value) in map {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match value {
            serde_json::Value::Object(inner) if depth < FLATTEN_MAX_DEPTH => {
                parts.push(flatten_object(inner, &full_key, depth + 1));
            }
            other => {
                parts.push(format!("{full_key}={}", format_value(other)));
            }
        }
    }
    parts.join(" | ")
}

/// Format a JSON value as Gemma 4-safe plain text.
///
/// CRITICAL: output must never contain `{`, `}`, `:` as structural
/// separators, or `"` around values — these collide with Gemma 4's
/// native `response:name{key:value}` and `<|"|>` token syntax.
fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr.iter().map(format_value).collect();
            format!("[{}]", items.join(", "))
        }
        serde_json::Value::Object(map) => {
            // Beyond max depth — flatten to key=value pairs inline
            // Do NOT use v.to_string() which produces JSON with {, }, :, "
            let pairs: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}={}", format_value(v)))
                .collect();
            format!("({})", pairs.join(", "))
        }
    }
}

/// Patterns that indicate behavioral instructions in tool descriptions.
const BEHAVIORAL_PREFIXES: &[&str] = &[
    "Use when",
    "Use this",
    "Do NOT",
    "Do not",
    "Don't",
    "Never ",
    "Only use",
    "Only call",
    "Designed for",
    "Check results with",
];

/// Simplify a tool spec for models that need compact schemas.
///
/// Applies four transformations:
/// 1. Truncate description to first sentence
/// 2. Strip sentences starting with behavioral instruction patterns
/// 3. Remove properties not in the `required` array
/// 4. Truncate parameter descriptions to first sentence
pub fn simplify_tool_spec(spec: &ToolSpec) -> ToolSpec {
    ToolSpec {
        name: spec.name.clone(),
        description: simplify_description(&spec.description),
        parameters: simplify_parameters(&spec.parameters),
    }
}

/// Simplified schema for the browser tool.
///
/// `simplify_tool_spec` strips all non-required params, but the browser tool
/// uses a single dispatch pattern: only `action` is required, while params like
/// `url`, `selector`, `value`, etc. are action-dependent. Without these, Gemma 4
/// cannot call the browser tool correctly. This schema keeps the essential params
/// with short descriptions and drops computer-use-only and snapshot-tuning params.
fn browser_simplified_params() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["action"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["open", "snapshot", "click", "fill", "type", "get_text",
                         "get_title", "get_url", "screenshot", "wait", "press",
                         "hover", "scroll", "is_visible", "close", "find"],
                "description": "Action: open(url), snapshot(), click(selector), fill(selector,value), type(selector,text), get_text(selector), get_title(), get_url(), screenshot(), wait(ms), press(key), hover(selector), scroll(direction), is_visible(selector), close(), find(by,value,find_action)."
            },
            "url": {
                "type": "string",
                "description": "URL to open."
            },
            "selector": {
                "type": "string",
                "description": "Element selector: @e1 ref, CSS (#id/.class), or text=..."
            },
            "value": {
                "type": "string",
                "description": "Value to fill or search for."
            },
            "text": {
                "type": "string",
                "description": "Text to type or wait for."
            },
            "key": {
                "type": "string",
                "description": "Key to press (Enter, Tab, Escape, etc.)"
            },
            "ms": {
                "type": "integer",
                "description": "Milliseconds to wait."
            },
            "direction": {
                "type": "string",
                "enum": ["up", "down", "left", "right"],
                "description": "Scroll direction."
            },
            "pixels": {
                "type": "integer",
                "description": "Pixels to scroll."
            },
            "by": {
                "type": "string",
                "enum": ["role", "text", "label", "placeholder", "testid"],
                "description": "Semantic locator type for find."
            },
            "find_action": {
                "type": "string",
                "enum": ["click", "fill", "text", "hover", "check"],
                "description": "Action to perform on found element."
            }
        }
    })
}

/// Simplify a tool spec, with special handling for tools that use dispatch patterns
/// where action-dependent parameters are not in the `required` array.
pub fn simplify_tool_spec_by_name(spec: &ToolSpec) -> ToolSpec {
    if spec.name == "browser" {
        return ToolSpec {
            name: spec.name.clone(),
            description: simplify_description(&spec.description),
            parameters: browser_simplified_params(),
        };
    }
    simplify_tool_spec(spec)
}

fn simplify_description(desc: &str) -> String {
    let sentence = first_sentence(desc);
    if BEHAVIORAL_PREFIXES.iter().any(|p| sentence.starts_with(p)) {
        return sentence;
    }
    sentence
}

fn first_sentence(text: &str) -> String {
    if let Some(pos) = text.find(". ") {
        let candidate = &text[..pos + 1];
        if candidate.ends_with("e.g.") || candidate.ends_with("i.e.") || candidate.ends_with("etc.")
        {
            if let Some(next_pos) = text[pos + 2..].find(". ") {
                return text[..pos + 2 + next_pos + 1].to_string();
            }
        }
        return candidate.to_string();
    }
    text.to_string()
}

fn simplify_parameters(params: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = params.as_object() else {
        return params.clone();
    };

    let mut result = obj.clone();

    let required: Vec<String> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
        let mut simplified_props = serde_json::Map::new();
        for (key, value) in props {
            if !required.contains(key) {
                continue;
            }
            let mut prop = value.clone();
            if let Some(desc) = prop.get("description").and_then(|d| d.as_str()) {
                let short = first_sentence(desc);
                prop["description"] = serde_json::Value::String(short);
            }
            simplified_props.insert(key.clone(), prop);
        }
        result.insert(
            "properties".to_string(),
            serde_json::Value::Object(simplified_props),
        );
    }

    serde_json::Value::Object(result)
}

/// Process tool output for LLM consumption.
///
/// Applies four transformations in order:
/// 1. Strip ANSI escape codes (prevents garbage tokens)
/// 2. Flatten JSON responses to pipe-delimited plain text (Gemma 4 compat)
/// 3. Overflow handling (truncate + save to file + exploration hints)
/// 4. Metadata footer (exit code or ok/err + duration)
pub fn present_for_llm(
    output: &str,
    tool_name: &str,
    success: bool,
    duration: Duration,
    config: &PresentationConfig,
) -> String {
    let mut result = output.to_string();

    // Step 1: Strip ANSI escape codes
    if config.strip_ansi {
        result = strip_ansi(&result);
    }

    // Step 2: Flatten JSON responses to pipe-delimited plain text
    if config.flatten_json_responses {
        result = flatten_json_output(&result);
    }

    // Step 3: Overflow handling
    result = handle_overflow(&result, config);

    // Step 4: Metadata footer
    if config.show_metadata {
        let dur = format_duration(duration);
        let status = if SHELL_LIKE_TOOLS.contains(&tool_name) {
            // Shell-like tools use numeric exit codes (familiar from training data)
            if success {
                "exit:0".to_string()
            } else {
                "exit:1".to_string()
            }
        } else {
            // Non-shell tools use ok/err labels (semantically accurate)
            if success {
                "ok".to_string()
            } else {
                "err".to_string()
            }
        };
        result.push_str(&format!("\n[{status} | {dur}]"));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── ANSI stripping tests ──

    #[test]
    fn strip_ansi_removes_color_codes() {
        let input = "\x1b[31mERROR\x1b[0m: something failed";
        assert_eq!(strip_ansi(input), "ERROR: something failed");
    }

    #[test]
    fn strip_ansi_removes_cursor_movement() {
        let input = "\x1b[2K\x1b[1G50% complete";
        assert_eq!(strip_ansi(input), "50% complete");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        let input = "no escape codes here\nline two";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn strip_ansi_handles_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn strip_ansi_handles_nested_codes() {
        let input = "\x1b[1m\x1b[32mBOLD GREEN\x1b[0m normal";
        assert_eq!(strip_ansi(input), "BOLD GREEN normal");
    }

    // ── Overflow tests ──

    #[test]
    fn overflow_short_output_unchanged() {
        let config = PresentationConfig::default();
        let input = "line1\nline2\nline3";
        let result = handle_overflow(input, &config);
        assert_eq!(result, input);
    }

    #[test]
    fn overflow_truncates_at_line_limit() {
        let config = PresentationConfig {
            max_output_lines: 3,
            max_output_bytes: 1_000_000,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let result = handle_overflow(&input, &config);
        assert!(result.contains("line 1\nline 2\nline 3"));
        assert!(result.contains("--- output truncated"));
        assert!(result.contains("10 lines"));
        assert!(result.contains("Full output:"));
        assert!(result.contains("grep <pattern>"));
        assert!(!result.contains("line 4"));
    }

    #[test]
    fn overflow_truncates_at_byte_limit() {
        let config = PresentationConfig {
            max_output_lines: 1_000_000,
            max_output_bytes: 50,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "a".repeat(200);
        let result = handle_overflow(&input, &config);
        assert!(result.contains("--- output truncated"));
        assert!(result.contains("Full output:"));
    }

    #[test]
    fn overflow_saves_full_output_to_file() {
        let overflow_dir = std::env::temp_dir().join("zeroclaw-test-overflow");
        let config = PresentationConfig {
            max_output_lines: 2,
            max_output_bytes: 1_000_000,
            overflow_dir: overflow_dir.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "line1\nline2\nline3\nline4\nline5";
        let result = handle_overflow(&input, &config);

        // Extract file path from output
        let path_line = result
            .lines()
            .find(|l| l.starts_with("Full output:"))
            .unwrap();
        let path_str = path_line.trim_start_matches("Full output: ").trim();
        let saved = fs::read_to_string(path_str).unwrap();
        assert_eq!(saved, input);

        // Cleanup
        let _ = fs::remove_dir_all(&overflow_dir);
    }

    // ── Duration formatting tests ──

    #[test]
    fn format_duration_milliseconds() {
        assert_eq!(format_duration(Duration::from_millis(42)), "42ms");
        assert_eq!(format_duration(Duration::from_millis(999)), "999ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(Duration::from_millis(1200)), "1.2s");
        assert_eq!(format_duration(Duration::from_secs(45)), "45.0s");
    }

    // ── present_for_llm integration tests ──

    #[test]
    fn present_for_llm_shell_uses_exit_codes() {
        let config = PresentationConfig::default();
        let result = present_for_llm("hello", "shell", true, Duration::from_millis(42), &config);
        assert!(result.ends_with("[exit:0 | 42ms]"));
    }

    #[test]
    fn present_for_llm_shell_failure_exit_code() {
        let config = PresentationConfig::default();
        let result = present_for_llm(
            "Error: not found",
            "shell",
            false,
            Duration::from_millis(5),
            &config,
        );
        assert!(result.ends_with("[exit:1 | 5ms]"));
    }

    #[test]
    fn present_for_llm_non_shell_uses_ok_err() {
        let config = PresentationConfig::default();
        let result = present_for_llm(
            "file contents",
            "file_read",
            true,
            Duration::from_millis(2),
            &config,
        );
        assert!(result.ends_with("[ok | 2ms]"));
        let result = present_for_llm(
            "Error: not found",
            "file_read",
            false,
            Duration::from_millis(3),
            &config,
        );
        assert!(result.ends_with("[err | 3ms]"));
    }

    #[test]
    fn present_for_llm_no_metadata_when_disabled() {
        let config = PresentationConfig {
            show_metadata: false,
            ..Default::default()
        };
        let result = present_for_llm("hello", "shell", true, Duration::from_millis(42), &config);
        assert_eq!(result, "hello");
    }

    #[test]
    fn present_for_llm_full_pipeline() {
        let config = PresentationConfig {
            max_output_lines: 2,
            overflow_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let input = "\x1b[31mline1\x1b[0m\nline2\nline3";
        let result = present_for_llm(input, "shell", true, Duration::from_millis(10), &config);
        assert!(result.contains("line1")); // ANSI stripped
        assert!(!result.contains("\x1b")); // no raw escapes
        assert!(result.contains("truncated")); // overflow triggered
        assert!(result.contains("[exit:0 | 10ms]")); // metadata
    }

    // ── JSON flattening tests ──

    #[test]
    fn flatten_json_simple_object() {
        let input =
            r#"{"name": "speakr-daily-summary", "schedule": "0 12 * * *", "enabled": true}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("name=speakr-daily-summary"));
        assert!(result.contains("schedule=0 12 * * *"));
        assert!(result.contains("enabled=true"));
        assert!(result.contains(" | "));
    }

    #[test]
    fn flatten_json_nested_object() {
        let input = r#"{"job": {"id": "abc-123", "status": "ok"}, "count": 3}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("job.id=abc-123"));
        assert!(result.contains("job.status=ok"));
        assert!(result.contains("count=3"));
    }

    #[test]
    fn flatten_json_array_of_objects() {
        let input = r#"[{"name": "job1", "status": "ok"}, {"name": "job2", "status": "err"}]"#;
        let result = flatten_json_output(input);
        assert!(result.contains("[0] name=job1 | status=ok"));
        assert!(result.contains("[1] name=job2 | status=err"));
    }

    #[test]
    fn flatten_json_not_json_passthrough() {
        let input = "This is plain text output, not JSON.";
        let result = flatten_json_output(input);
        assert_eq!(result, input);
    }

    #[test]
    fn flatten_json_empty_object() {
        let input = "{}";
        let result = flatten_json_output(input);
        assert_eq!(result, "(empty)");
    }

    #[test]
    fn flatten_json_null_values() {
        let input = r#"{"name": "test", "value": null}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("name=test"));
        assert!(result.contains("value=null"));
    }

    #[test]
    fn flatten_json_deeply_nested_uses_parens() {
        // 3 levels of nesting: depth 0 -> a, depth 1 -> b, depth 2 -> c is object
        // At depth 2, c exceeds FLATTEN_MAX_DEPTH so format_value produces parens
        let input = r#"{"a": {"b": {"c": {"d": "deep"}}}}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("a.b.c=(d=deep)"));
        assert!(!result.contains('{'));
        assert!(!result.contains('}'));
        assert!(!result.contains('"'));
    }

    #[test]
    fn flatten_json_output_never_contains_json_syntax() {
        let inputs = vec![
            r#"{"simple": "value"}"#,
            r#"{"nested": {"deep": {"deeper": "val"}}}"#,
            r#"[{"a": 1}, {"b": {"c": 2}}]"#,
            r#"{"arr": [1, 2, 3], "obj": {"k": "v"}}"#,
        ];
        for input in inputs {
            let result = flatten_json_output(input);
            assert!(
                !result.contains('{'),
                "contains {{ for: {input}\ngot: {result}"
            );
            assert!(
                !result.contains('}'),
                "contains }} for: {input}\ngot: {result}"
            );
            assert!(
                !result.contains('"'),
                "contains \" for: {input}\ngot: {result}"
            );
        }
    }

    #[test]
    fn present_for_llm_flattens_json_when_enabled() {
        let config = PresentationConfig {
            flatten_json_responses: true,
            ..Default::default()
        };
        let json_input = r#"{"status": "ok", "count": 5}"#;
        let result = present_for_llm(
            json_input,
            "cron_list",
            true,
            Duration::from_millis(10),
            &config,
        );
        assert!(result.contains("status=ok"));
        assert!(result.contains("count=5"));
        assert!(!result.contains(r#""status""#));
    }

    #[test]
    fn present_for_llm_skips_flatten_when_disabled() {
        let config = PresentationConfig::default();
        let json_input = r#"{"status": "ok", "count": 5}"#;
        let result = present_for_llm(
            json_input,
            "cron_list",
            true,
            Duration::from_millis(10),
            &config,
        );
        assert!(result.contains(r#""status""#));
    }

    #[test]
    fn flatten_realistic_cron_list_output() {
        let input = r#"[
  {
    "id": "d4e5f6",
    "name": "speakr-daily-summary",
    "schedule": {"type": "cron", "expr": "0 12,17 * * 1-5", "tz": "America/Vancouver"},
    "job_type": "agent",
    "enabled": true,
    "next_run": "2026-04-08T19:00:00Z"
  },
  {
    "id": "a1b2c3",
    "name": "morning-project-status",
    "schedule": {"type": "cron", "expr": "0 8 * * 1-5", "tz": "America/Vancouver"},
    "job_type": "agent",
    "enabled": true,
    "next_run": "2026-04-09T15:00:00Z"
  }
]"#;
        let result = flatten_json_output(input);
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[0]"));
        assert!(lines[0].contains("name=speakr-daily-summary"));
        assert!(lines[0].contains("schedule.expr=0 12,17 * * 1-5"));
        assert!(lines[0].contains("enabled=true"));
        assert!(lines[1].contains("[1]"));
        assert!(lines[1].contains("name=morning-project-status"));
        assert!(!result.contains('"'), "output contains quotes: {result}");
        assert!(
            !result.contains('{'),
            "output contains open brace: {result}"
        );
        assert!(
            !result.contains('}'),
            "output contains close brace: {result}"
        );
    }

    // ── Tool schema simplification tests ──

    #[test]
    fn simplify_truncates_description_to_first_sentence() {
        let spec = ToolSpec {
            name: "file_read".into(),
            description: "Read file contents with line numbers. Supports partial reading via offset and limit. Sensitive files are blocked by default.".into(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.description, "Read file contents with line numbers.");
    }

    #[test]
    fn simplify_strips_behavioral_instructions() {
        let spec = ToolSpec {
            name: "delegate".into(),
            description: "Delegate a task to another agent. Use when: a task benefits from a different model. Do NOT call this for simple lookups.".into(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.description, "Delegate a task to another agent.");
    }

    #[test]
    fn simplify_removes_optional_parameters() {
        let spec = ToolSpec {
            name: "file_read".into(),
            description: "Read a file.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file."},
                    "offset": {"type": "integer", "description": "Starting line number (1-based, default: 1)."},
                    "limit": {"type": "integer", "description": "Max lines to return (default: all)."}
                },
                "required": ["path"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("path"));
        assert!(!props.contains_key("offset"));
        assert!(!props.contains_key("limit"));
    }

    #[test]
    fn simplify_keeps_all_params_when_all_required() {
        let spec = ToolSpec {
            name: "memory_store".into(),
            description: "Store a memory.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Memory key."},
                    "value": {"type": "string", "description": "Memory value."}
                },
                "required": ["key", "value"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        assert_eq!(props.len(), 2);
    }

    #[test]
    fn simplify_truncates_parameter_descriptions() {
        let spec = ToolSpec {
            name: "browser".into(),
            description: "Control a browser.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "Browser action. Common actions and required params: open(url), get_text(selector), click(selector)."
                    }
                },
                "required": ["action"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        let desc = result.parameters["properties"]["action"]["description"]
            .as_str()
            .unwrap();
        assert_eq!(desc, "Browser action.");
    }

    #[test]
    fn simplify_preserves_name() {
        let spec = ToolSpec {
            name: "shell".into(),
            description: "Execute a shell command.".into(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.name, "shell");
    }

    #[test]
    fn simplify_handles_description_without_period() {
        let spec = ToolSpec {
            name: "cron_list".into(),
            description: "List all scheduled cron jobs".into(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.description, "List all scheduled cron jobs");
    }

    #[test]
    fn simplify_handles_empty_required_array() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "Test.".into(),
            parameters: json!({
                "type": "object",
                "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
                "required": []
            }),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        assert!(props.is_empty());
    }

    #[test]
    fn simplify_handles_no_required_field() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "Test.".into(),
            parameters: json!({"type": "object", "properties": {"a": {"type": "string"}}}),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        assert!(props.is_empty());
    }

    #[test]
    fn simplify_preserves_enum_and_type_fields() {
        let spec = ToolSpec {
            name: "browser".into(),
            description: "Browser.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "click", "snapshot"],
                        "description": "Browser action. Many details here."
                    }
                },
                "required": ["action"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        let action = &result.parameters["properties"]["action"];
        assert!(action["enum"].is_array());
        assert_eq!(action["type"], "string");
        assert_eq!(action["description"].as_str().unwrap(), "Browser action.");
    }

    // ── Integration tests with realistic schemas ──

    #[test]
    fn simplify_realistic_file_read_schema() {
        let spec = ToolSpec {
            name: "file_read".into(),
            description: "Read file contents with line numbers. Supports partial reading via offset and limit. Extracts text from PDF; other binary files are read with lossy UTF-8 conversion. Sensitive files are blocked by default.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file. Relative paths resolve from workspace; outside paths require policy allowlist."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Starting line number (1-based, default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: all)"
                    }
                },
                "required": ["path"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.description, "Read file contents with line numbers.");
        let props = result.parameters["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("path"));
        assert_eq!(
            props["path"]["description"].as_str().unwrap(),
            "Path to the file."
        );
    }

    #[test]
    fn simplify_realistic_browser_schema() {
        let spec = ToolSpec {
            name: "browser".into(),
            description: "Control a headless Chromium browser. Supports navigation, clicking, form filling, screenshots, and accessibility snapshots. Use snapshot for reading page content.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "snapshot", "click", "fill", "screenshot", "wait", "scroll", "find"],
                        "description": "Browser action. Common actions and required params: open(url), get_text(selector), click(selector)."
                    },
                    "url": {"type": "string", "description": "URL to navigate to (for open action)"},
                    "selector": {"type": "string", "description": "Element selector."},
                    "value": {"type": "string", "description": "Value to fill or search for."},
                    "direction": {"type": "string", "description": "Scroll direction."},
                    "ms": {"type": "integer", "description": "Milliseconds to wait."}
                },
                "required": ["action"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(result.description, "Control a headless Chromium browser.");
        let props = result.parameters["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("action"));
        assert!(props["action"]["enum"].is_array());
        assert_eq!(
            props["action"]["description"].as_str().unwrap(),
            "Browser action."
        );
    }

    #[test]
    fn simplify_browser_by_name_preserves_action_dependent_params() {
        // Browser uses action-dispatch: only `action` is required, but url/selector/value etc.
        // are essential for Gemma 4 to call it correctly. simplify_tool_spec_by_name must
        // preserve these rather than stripping them.
        let spec = ToolSpec {
            name: "browser".into(),
            description: "Web/browser automation with pluggable backends.".into(),
            parameters: json!({
                "type": "object",
                "required": ["action"],
                "properties": {
                    "action": {"type": "string", "enum": ["open", "snapshot"]},
                    "url": {"type": "string"},
                    "selector": {"type": "string"},
                    "value": {"type": "string"},
                    "x": {"type": "integer", "description": "Screen X coordinate (computer_use)"}
                }
            }),
        };
        let result = simplify_tool_spec_by_name(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        // Key params must be present
        assert!(props.contains_key("action"), "action must be kept");
        assert!(
            props.contains_key("url"),
            "url must be kept for open action"
        );
        assert!(
            props.contains_key("selector"),
            "selector must be kept for click/fill"
        );
        assert!(props.contains_key("value"), "value must be kept for fill");
        // computer_use-only params must be dropped
        assert!(
            !props.contains_key("x"),
            "computer_use coord x must be dropped"
        );
    }

    #[test]
    fn simplify_non_browser_by_name_uses_standard_simplification() {
        // Non-browser tools should use the standard simplify_tool_spec behavior
        let spec = ToolSpec {
            name: "shell".into(),
            description: "Execute a shell command. Returns stdout and stderr.".into(),
            parameters: json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {"type": "string", "description": "Command to run."},
                    "timeout": {"type": "integer", "description": "Timeout in seconds."}
                }
            }),
        };
        let result = simplify_tool_spec_by_name(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("command"), "required param must be kept");
        assert!(
            !props.contains_key("timeout"),
            "optional param must be stripped"
        );
    }

    #[test]
    fn simplify_realistic_bg_run_behavioral() {
        let spec = ToolSpec {
            name: "bg_run".into(),
            description: "Execute a tool in the background and return a job ID immediately. Use this for long-running operations where you don't want to block. Check results with bg_status.".into(),
            parameters: json!({"type": "object", "properties": {"tool": {"type": "string", "description": "Tool name."}}, "required": ["tool"]}),
        };
        let result = simplify_tool_spec(&spec);
        assert_eq!(
            result.description,
            "Execute a tool in the background and return a job ID immediately."
        );
    }
}
