# Tool Response JSON-to-Plaintext Normalization Layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a config-driven normalization step to the presentation layer that converts JSON tool responses to pipe-delimited plain text before they reach the LLM, fixing Gemma 4's progressive degradation when it receives JSON in tool results.

**Architecture:** The presentation layer (`src/agent/presentation.rs`) already sits between tool execution and LLM consumption, applying ANSI stripping, overflow handling, and metadata footers. We add a fourth transformation — JSON flattening — controlled by a new `flatten_json_responses` boolean in `PresentationConfig`. When enabled, any tool output that parses as a JSON object or array gets converted to `key=value | key=value` pipe-delimited format. This is a single-point fix that covers all 22 JSON-emitting tools without modifying any of them.

**Why pipe-delimited:** Gemma 4's native tool response format is `response:fn_name{key:value,key2:<|"|>str<|"|>}`. Three characters in JSON collide with this syntax:
- `{` `}` — structural delimiters in `response:name{...}`
- `:` — key-value separator in `key:value`
- `"` — conflicts with `<|"|>` string delimiter tokens

The `key=value | key=value` format avoids all three: `=` instead of `:`, `|` instead of `{}`, no quotes. The output must NEVER contain `{`, `}`, `:` as structural separators, or `"` around values — even for deeply nested objects or arrays that fall back to stringification.

**Tech Stack:** Rust, serde_json

---

### Task 1: Add `flatten_json_responses` config field

**Files:**
- Modify: `src/config/schema.rs:812-832`

- [ ] **Step 1: Write the test**

Add to the existing config round-trip tests (around line 10547):

```rust
#[test]
fn presentation_flatten_json_defaults_to_false() {
    let config = PresentationConfig::default();
    assert!(!config.flatten_json_responses);
}

#[test]
fn presentation_flatten_json_deserializes_from_toml() {
    let toml_str = r#"
        [presentation]
        flatten_json_responses = true
    "#;
    // Deserialize just the presentation section
    #[derive(serde::Deserialize)]
    struct Wrapper {
        presentation: PresentationConfig,
    }
    let parsed: Wrapper = toml::from_str(toml_str).unwrap();
    assert!(parsed.presentation.flatten_json_responses);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib presentation_flatten_json -- --nocapture`
Expected: compilation error — `flatten_json_responses` field doesn't exist yet.

- [ ] **Step 3: Add the field to PresentationConfig**

In `src/config/schema.rs`, add to the `PresentationConfig` struct (after `overflow_dir`):

```rust
    /// Convert JSON tool responses to pipe-delimited plain text before sending
    /// to the LLM. Useful for models like Gemma 4 where JSON in tool results
    /// collides with the model's native tool-call syntax. Default: false.
    #[serde(default)]
    pub flatten_json_responses: bool,
```

And update the `Default` impl to include the new field:

```rust
impl Default for PresentationConfig {
    fn default() -> Self {
        Self {
            max_output_lines: default_max_output_lines(),
            max_output_bytes: default_max_output_bytes(),
            strip_ansi: default_true(),
            show_metadata: default_true(),
            overflow_dir: default_overflow_dir(),
            flatten_json_responses: false,
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib presentation_flatten_json -- --nocapture`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add flatten_json_responses to PresentationConfig"
```

---

### Task 2: Implement JSON flattening logic

**Files:**
- Modify: `src/agent/presentation.rs`

The flattening function converts JSON values to pipe-delimited key=value strings. It handles nested objects by dot-notation (e.g., `schedule.expr=0 12 * * *`) and arrays by index. Values are unquoted when possible.

- [ ] **Step 1: Write the tests**

Add to the `tests` module in `src/agent/presentation.rs`:

```rust
    // ── JSON flattening tests ──

    #[test]
    fn flatten_json_simple_object() {
        let input = r#"{"name": "speakr-daily-summary", "schedule": "0 12 * * *", "enabled": true}"#;
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
    fn flatten_json_string_with_pipes() {
        let input = r#"{"cmd": "echo a | grep b"}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("cmd=echo a | grep b"));
    }

    #[test]
    fn flatten_json_deeply_nested_caps_at_two_levels() {
        let input = r#"{"a": {"b": {"c": "deep"}}}"#;
        let result = flatten_json_output(input);
        // Should flatten to 2 levels, then use parenthesized key=value for deeper
        assert!(result.contains("a.b=(c=deep)"));
        // Must NOT contain JSON syntax
        assert!(!result.contains('{'));
        assert!(!result.contains('}'));
        assert!(!result.contains('"'));
    }

    #[test]
    fn flatten_json_output_never_contains_json_syntax() {
        // Comprehensive test: no output should contain Gemma 4 collision characters
        // as structural elements
        let inputs = vec![
            r#"{"simple": "value"}"#,
            r#"{"nested": {"deep": {"deeper": "val"}}}"#,
            r#"[{"a": 1}, {"b": {"c": 2}}]"#,
            r#"{"arr": [1, 2, 3], "obj": {"k": "v"}}"#,
        ];
        for input in inputs {
            let result = flatten_json_output(input);
            assert!(!result.contains('{'), "Output contains '{{' for input: {input}\nGot: {result}");
            assert!(!result.contains('}'), "Output contains '}}' for input: {input}\nGot: {result}");
            assert!(!result.contains('"'), "Output contains '\"' for input: {input}\nGot: {result}");
        }
    }

    #[test]
    fn present_for_llm_flattens_json_when_enabled() {
        let config = PresentationConfig {
            flatten_json_responses: true,
            ..Default::default()
        };
        let json_input = r#"{"status": "ok", "count": 5}"#;
        let result = present_for_llm(json_input, "cron_list", true, Duration::from_millis(10), &config);
        assert!(result.contains("status=ok"));
        assert!(result.contains("count=5"));
        assert!(!result.contains(r#""status""#)); // no JSON quotes
    }

    #[test]
    fn present_for_llm_skips_flatten_when_disabled() {
        let config = PresentationConfig::default(); // flatten_json_responses = false
        let json_input = r#"{"status": "ok", "count": 5}"#;
        let result = present_for_llm(json_input, "cron_list", true, Duration::from_millis(10), &config);
        assert!(result.contains(r#""status""#)); // JSON preserved
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib flatten_json -- --nocapture`
Expected: compilation error — `flatten_json_output` function doesn't exist yet.

- [ ] **Step 3: Implement the flattening function**

Add to `src/agent/presentation.rs`, before the `present_for_llm` function:

```rust
/// Maximum nesting depth for JSON flattening. Deeper values are stringified.
const FLATTEN_MAX_DEPTH: usize = 2;

/// Flatten a JSON object/array to pipe-delimited `key=value` plain text.
///
/// Returns the input unchanged if it doesn't parse as JSON.
/// Objects become `key=value | key=value`. Nested objects use dot notation
/// up to `FLATTEN_MAX_DEPTH`. Arrays of objects become one line per element.
pub fn flatten_json_output(output: &str) -> String {
    let trimmed = output.trim();
    let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return output.to_string();
    };

    match &value {
        serde_json::Value::Object(map) if map.is_empty() => "(empty)".to_string(),
        serde_json::Value::Object(map) => flatten_object(map, "", 0),
        serde_json::Value::Array(arr) if arr.is_empty() => "(empty list)".to_string(),
        serde_json::Value::Array(arr) => {
            arr.iter()
                .enumerate()
                .map(|(i, v)| match v {
                    serde_json::Value::Object(map) => {
                        format!("[{i}] {}", flatten_object(map, "", 0))
                    }
                    other => format!("[{i}] {}", format_value(other)),
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
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
```

- [ ] **Step 4: Wire it into `present_for_llm`**

In the `present_for_llm` function, add a new step between ANSI stripping (step 1) and overflow handling (step 2):

```rust
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
            if success {
                "exit:0".to_string()
            } else {
                "exit:1".to_string()
            }
        } else {
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
```

Update the module doc comment at the top of the file:

```rust
//! LLM presentation layer for tool output.
//!
//! Sits between tool execution and LLM-facing result formatting.
//! Applies four transformations:
//! 1. Strip ANSI escape codes (prevents garbage tokens)
//! 2. Flatten JSON responses (prevents Gemma 4 tool-call syntax collision)
//! 3. Overflow handling (truncate + save to file + exploration hints)
//! 4. Metadata footer (exit code or ok/err + duration)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib presentation -- --nocapture`
Expected: All tests pass (both new and existing).

- [ ] **Step 6: Run full test suite**

Run: `cargo test`
Expected: No regressions.

- [ ] **Step 7: Commit**

```bash
git add src/agent/presentation.rs
git commit -m "feat(presentation): add JSON-to-plaintext flattening for Gemma 4 compat

Adds flatten_json_output() to the presentation layer. When
flatten_json_responses=true in [presentation] config, JSON tool
responses are converted to pipe-delimited key=value format before
reaching the LLM. Handles nested objects (dot notation, 2 levels),
arrays (one line per element), and passes non-JSON through unchanged.
Covers all 22 JSON-emitting tools without modifying any of them."
```

---

### Task 3: Enable in Sam's config

**Files:**
- Modify: `k8s/sam/03_zeroclaw_configmap.yaml`

- [ ] **Step 1: Add the config setting**

Find the `[presentation]` section in Sam's config (or add one if it doesn't exist). Add:

```toml
[presentation]
flatten_json_responses = true
```

If a `[presentation]` section already exists, just add the key to it.

- [ ] **Step 2: Validate YAML**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/03_zeroclaw_configmap.yaml')); print('OK')"`

Expected: `OK`

- [ ] **Step 3: Commit**

```bash
git add k8s/sam/03_zeroclaw_configmap.yaml
git commit -m "feat(k8s/sam): enable flatten_json_responses for Gemma 4"
```

---

### Task 4: Add integration test

**Files:**
- Modify: `src/agent/presentation.rs`

Add an integration-style test that simulates realistic tool output from one of the 22 JSON-emitting tools.

- [ ] **Step 1: Add realistic integration test**

```rust
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
        // Should be two lines (one per array element)
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("[0]"));
        assert!(lines[0].contains("name=speakr-daily-summary"));
        assert!(lines[0].contains("schedule.expr=0 12,17 * * 1-5"));
        assert!(lines[0].contains("enabled=true"));
        assert!(lines[1].contains("[1]"));
        assert!(lines[1].contains("name=morning-project-status"));
        // No Gemma 4 collision characters in output
        assert!(!result.contains('"'), "output contains quotes: {result}");
        assert!(!result.contains('{'), "output contains open brace: {result}");
        assert!(!result.contains('}'), "output contains close brace: {result}");
    }

    #[test]
    fn flatten_realistic_cron_add_output() {
        let input = r#"{
  "id": "new-job-id",
  "name": "vikunja-task-review",
  "schedule": "0 9 * * 1-5",
  "job_type": "agent",
  "session_target": "isolated",
  "next_run": "2026-04-09T16:00:00Z",
  "created": true
}"#;
        let result = flatten_json_output(input);
        assert!(result.contains("id=new-job-id"));
        assert!(result.contains("name=vikunja-task-review"));
        assert!(result.contains("created=true"));
        // No Gemma 4 collision characters
        assert!(!result.contains('"'));
        assert!(!result.contains('{'));
        assert!(!result.contains('}'));
    }
```

- [ ] **Step 2: Run tests**

Run: `cargo test --lib flatten_realistic -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add src/agent/presentation.rs
git commit -m "test(presentation): add realistic integration tests for JSON flattening"
```

---

## Affected Tools (covered by this single change)

These 22 tools return JSON that will be auto-flattened when `flatten_json_responses = true`:

| Tool | File |
|------|------|
| agents_ipc | src/tools/agents_ipc.rs |
| bg_run | src/tools/bg_run.rs |
| browser | src/tools/browser.rs |
| channel_ack_config | src/tools/channel_ack_config.rs |
| cron_add | src/tools/cron_add.rs |
| cron_list | src/tools/cron_list.rs |
| cron_run | src/tools/cron_run.rs |
| cron_runs | src/tools/cron_runs.rs |
| cron_update | src/tools/cron_update.rs |
| delegate_coordination_status | src/tools/delegate_coordination_status.rs |
| git_operations | src/tools/git_operations.rs |
| model_routing_config | src/tools/model_routing_config.rs |
| openclaw_migration | src/tools/openclaw_migration.rs |
| process | src/tools/process.rs |
| proxy_config | src/tools/proxy_config.rs |
| schedule | src/tools/schedule.rs |
| subagent_list | src/tools/subagent_list.rs |
| subagent_manage | src/tools/subagent_manage.rs |
| subagent_spawn | src/tools/subagent_spawn.rs |
| wasm_module | src/tools/wasm_module.rs |
| web_access_config | src/tools/web_access_config.rs |
| web_search_config | src/tools/web_search_config.rs |

## Verification

- Config field: deserialization round-trip test
- Flattening: 10 unit tests covering objects, arrays, nesting, passthrough, edge cases
- Integration: realistic cron_list and cron_add output tests
- Wiring: `present_for_llm` respects the flag (enabled=flattens, disabled=passthrough)
- Full suite: `cargo test` no regressions
- Sam's config: `flatten_json_responses = true` in configmap
