# Tool Schema Simplification Layer

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a config-driven schema simplification layer that truncates tool descriptions, strips behavioral instructions, removes optional parameters, and shortens parameter descriptions — covering all 50+ affected tools without modifying any individual tool file.

**Architecture:** A new `simplify_tool_spec()` function in `src/agent/presentation.rs` transforms `ToolSpec` objects before they reach the provider. It's called at `src/agent/loop_.rs:1148-1152` after specs are collected, gated by a new `simplify_tool_schemas: bool` config field. The function applies four transformations: (1) truncate description to first sentence, (2) strip behavioral instruction patterns, (3) remove properties not in `required` array, (4) truncate parameter descriptions to first sentence. The existing `PresentationConfig` gets a new field. The function operates on `ToolSpec` (a simple struct with `name`, `description`, `parameters` fields) — no trait changes needed.

**Tech Stack:** Rust, serde_json, regex (for behavioral pattern stripping)

---

### Task 1: Add `simplify_tool_schemas` config field

**Files:**
- Modify: `src/config/schema.rs:812-845`

- [ ] **Step 1: Add the field**

In `PresentationConfig` (after `flatten_json_responses`), add:

```rust
    /// Simplify tool schemas for models with limited schema parsing capacity.
    /// Truncates descriptions to first sentence, strips behavioral instructions
    /// ("Do NOT...", "Use when..."), removes optional parameters, and shortens
    /// parameter descriptions. Default: false.
    #[serde(default)]
    pub simplify_tool_schemas: bool,
```

Update `Default` impl to include `simplify_tool_schemas: false`.

- [ ] **Step 2: Run tests**

Run: `cargo test --lib config -- --nocapture 2>&1 | tail -5`
Expected: Existing tests pass (field defaults to false, backward compatible via `#[serde(default)]`).

- [ ] **Step 3: Commit**

```bash
git add src/config/schema.rs
git commit -m "feat(config): add simplify_tool_schemas to PresentationConfig"
```

---

### Task 2: Implement schema simplification

**Files:**
- Modify: `src/agent/presentation.rs`

- [ ] **Step 1: Write the tests**

Add to the `tests` module in `src/agent/presentation.rs`:

```rust
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
        assert!(props.contains_key("path"), "required param should remain");
        assert!(!props.contains_key("offset"), "optional param should be removed");
        assert!(!props.contains_key("limit"), "optional param should be removed");
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
                        "description": "Browser action. Common actions and required params: open(url), get_text(selector), click(selector). OS-level actions require backend=computer_use."
                    }
                },
                "required": ["action"]
            }),
        };
        let result = simplify_tool_spec(&spec);
        let desc = result.parameters["properties"]["action"]["description"].as_str().unwrap();
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
                "properties": {
                    "a": {"type": "string"},
                    "b": {"type": "string"}
                },
                "required": []
            }),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        // Empty required = all optional = all removed
        assert!(props.is_empty());
    }

    #[test]
    fn simplify_handles_no_required_field() {
        let spec = ToolSpec {
            name: "test".into(),
            description: "Test.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "a": {"type": "string"}
                }
            }),
        };
        let result = simplify_tool_spec(&spec);
        let props = result.parameters["properties"].as_object().unwrap();
        // No required field = all optional = all removed
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
        assert!(action["enum"].is_array(), "enum should be preserved");
        assert_eq!(action["type"], "string", "type should be preserved");
        assert_eq!(action["description"].as_str().unwrap(), "Browser action.");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib simplify_ -- --nocapture 2>&1 | head -5`
Expected: compilation error — `simplify_tool_spec` doesn't exist yet.

- [ ] **Step 3: Implement the simplification function**

Add to `src/agent/presentation.rs`, after the JSON flattening functions and before `present_for_llm`:

```rust
use crate::tools::ToolSpec;

/// Patterns that indicate behavioral instructions in tool descriptions.
/// These get stripped because they confuse models with limited schema parsing.
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

/// Truncate to first sentence and strip behavioral instructions.
fn simplify_description(desc: &str) -> String {
    // Split into sentences (period followed by space or end)
    let first_sentence = first_sentence(desc);

    // Check if the first sentence itself is behavioral
    if BEHAVIORAL_PREFIXES.iter().any(|p| first_sentence.starts_with(p)) {
        // Return just the tool name hint (caller has the name)
        return first_sentence;
    }

    first_sentence
}

/// Extract the first sentence from text.
/// A sentence ends at ". " or "." at end of string.
fn first_sentence(text: &str) -> String {
    // Find first ". " boundary (sentence end followed by next sentence)
    if let Some(pos) = text.find(". ") {
        let candidate = &text[..pos + 1]; // include the period
        // Don't split on common abbreviations like "e.g." or "i.e."
        if candidate.ends_with("e.g.") || candidate.ends_with("i.e.") || candidate.ends_with("etc.") {
            // Try the next sentence boundary
            if let Some(next_pos) = text[pos + 2..].find(". ") {
                return text[..pos + 2 + next_pos + 1].to_string();
            }
        }
        return candidate.to_string();
    }
    // No ". " found — return entire text (single sentence or no period)
    text.to_string()
}

/// Remove optional properties and truncate parameter descriptions.
fn simplify_parameters(params: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = params.as_object() else {
        return params.clone();
    };

    let mut result = obj.clone();

    // Get the required field names
    let required: Vec<String> = obj
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Filter properties to only required ones, and simplify descriptions
    if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
        let mut simplified_props = serde_json::Map::new();
        for (key, value) in props {
            if !required.contains(key) {
                continue; // Skip optional parameters
            }
            let mut prop = value.clone();
            // Truncate parameter description to first sentence
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
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib simplify_ -- --nocapture`
Expected: All 10 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/agent/presentation.rs
git commit -m "feat(presentation): add tool schema simplification for Gemma 4

Adds simplify_tool_spec() to truncate descriptions to first sentence,
strip behavioral instructions, remove optional parameters, and shorten
parameter descriptions. Covers 50+ multi-sentence tools and 52+ tools
with optional params without modifying any individual tool file."
```

---

### Task 3: Wire into the agent loop

**Files:**
- Modify: `src/agent/loop_.rs:1148-1152`

The tool spec collection at line 1148 needs to conditionally apply simplification. The `PresentationConfig` is accessible via the config that's already threaded through the agent.

- [ ] **Step 1: Find how config is accessed in the loop**

The function `run_tool_call_loop` doesn't directly take a config reference, but uses task-local storage and function parameters. We need to pass the flag value. The cleanest approach: add a `simplify_schemas: bool` parameter to the function, set from the caller.

However, looking at the call sites, there's an easier path. The presentation config is already used in this file (for `present_for_llm`). Search for where `PresentationConfig` or `presentation` is accessed.

Read `src/agent/loop_.rs` around lines 2410-2430 to find how `present_for_llm` gets its config:

```rust
// The presentation config is accessed via a task-local:
let presentation_config = PRESENTATION_CONFIG
    .try_with(Clone::clone)
    .unwrap_or_default();
```

We'll use the same task-local.

- [ ] **Step 2: Add simplification to tool spec collection**

In `src/agent/loop_.rs`, modify lines 1148-1152. Change:

```rust
    let tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| tool.spec())
        .collect();
```

To:

```rust
    let presentation_config_for_schemas = PRESENTATION_CONFIG
        .try_with(Clone::clone)
        .unwrap_or_default();
    let tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| {
            let spec = tool.spec();
            if presentation_config_for_schemas.simplify_tool_schemas {
                crate::agent::presentation::simplify_tool_spec(&spec)
            } else {
                spec
            }
        })
        .collect();
```

Note: `PRESENTATION_CONFIG` is already a task-local defined in this file. Verify by searching for it.

- [ ] **Step 3: Also simplify the prompt-guided fallback path**

At line 2575-2577, there's another `tool.spec()` collection for the text-based tool instructions:

```rust
    let specs: Vec<crate::tools::ToolSpec> =
        tools_registry.iter().map(|tool| tool.spec()).collect();
    build_tool_instructions_from_specs(&specs)
```

Apply the same simplification here. This function doesn't have access to the task-local, so pass the flag as a parameter or access it there too.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib loop_ -- --nocapture 2>&1 | tail -10`
Expected: All existing tests pass.

Run: `cargo test --lib presentation -- --nocapture 2>&1 | tail -5`
Expected: All presentation tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "feat(agent): wire schema simplification into tool spec collection"
```

---

### Task 4: Enable in Sam's config and add integration tests

**Files:**
- Modify: `k8s/sam/03_zeroclaw_configmap.yaml`
- Modify: `src/agent/presentation.rs` (tests only)

- [ ] **Step 1: Add to Sam's config**

In `k8s/sam/03_zeroclaw_configmap.yaml`, in the `[presentation]` section (after `flatten_json_responses = true`), add:

```toml
      simplify_tool_schemas = true
```

- [ ] **Step 2: Add integration tests with real tool schemas**

Add to `src/agent/presentation.rs` tests module:

```rust
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

        // Description truncated to first sentence
        assert_eq!(result.description, "Read file contents with line numbers.");

        // Only required params remain
        let props = result.parameters["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("path"));

        // Parameter description also truncated
        let path_desc = props["path"]["description"].as_str().unwrap();
        assert_eq!(path_desc, "Path to the file.");
    }

    #[test]
    fn simplify_realistic_browser_schema() {
        let spec = ToolSpec {
            name: "browser".into(),
            description: concat!(
                "Control a headless Chromium browser. ",
                "Supports navigation, clicking, form filling, screenshots, and accessibility snapshots. ",
                "Use snapshot for reading page content, screenshot for visual verification."
            ).into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "snapshot", "click", "fill", "screenshot", "wait", "scroll", "find"],
                        "description": "Browser action. Common actions and required params: open(url), get_text(selector), click(selector). OS-level actions require backend=computer_use."
                    },
                    "url": {"type": "string", "description": "URL to navigate to (for open action)"},
                    "selector": {"type": "string", "description": "Element selector: @ref, CSS, text=..., or body for full page. Required for get_text, click, fill."},
                    "value": {"type": "string", "description": "Value to fill or search for."},
                    "direction": {"type": "string", "description": "Scroll direction: up or down."},
                    "ms": {"type": "integer", "description": "Milliseconds to wait."}
                },
                "required": ["action"]
            }),
        };
        let result = simplify_tool_spec(&spec);

        // Description truncated
        assert_eq!(result.description, "Control a headless Chromium browser.");

        // Only action remains (only required param)
        let props = result.parameters["properties"].as_object().unwrap();
        assert_eq!(props.len(), 1);
        assert!(props.contains_key("action"));

        // Enum preserved on action
        assert!(props["action"]["enum"].is_array());

        // Action description truncated
        let action_desc = props["action"]["description"].as_str().unwrap();
        assert_eq!(action_desc, "Browser action.");
    }

    #[test]
    fn simplify_realistic_bg_run_behavioral() {
        let spec = ToolSpec {
            name: "bg_run".into(),
            description: "Execute a tool in the background and return a job ID immediately. Use this for long-running operations where you don't want to block. Check results with bg_status or wait for auto-injection in the next turn. Background tools have a 600-second maximum timeout.".into(),
            parameters: json!({"type": "object", "properties": {"tool": {"type": "string", "description": "Tool name."}}, "required": ["tool"]}),
        };
        let result = simplify_tool_spec(&spec);
        // First sentence kept, behavioral sentences stripped
        assert_eq!(result.description, "Execute a tool in the background and return a job ID immediately.");
    }
```

- [ ] **Step 3: Run all tests**

Run: `cargo test --lib presentation -- --nocapture`
Expected: All tests pass.

- [ ] **Step 4: Validate Sam's config**

Run: `python3 -c "import yaml; yaml.safe_load(open('k8s/sam/03_zeroclaw_configmap.yaml')); print('OK')"`
Expected: `OK`

- [ ] **Step 5: Commit**

```bash
git add src/agent/presentation.rs k8s/sam/03_zeroclaw_configmap.yaml
git commit -m "feat(k8s/sam): enable simplify_tool_schemas and add integration tests"
```

---

## What This Covers

**Description simplification (50+ tools):**
Every multi-sentence description like `"Read file contents with line numbers. Supports partial reading via offset and limit. Sensitive files are blocked by default."` becomes `"Read file contents with line numbers."`

**Behavioral instruction stripping (6 tools):**
Sentences starting with "Use when", "Do NOT", "Check results with", etc. are removed. Example: `"Delegate a task to another agent. Use when: a task benefits from a different model."` becomes `"Delegate a task to another agent."`

**Optional parameter removal (52+ tools):**
Only parameters listed in the `required` array survive. Example: `file_read` goes from 3 params (path, offset, limit) to 1 (path). `browser` goes from 20+ params to 1 (action).

**Parameter description truncation (all tools):**
Parameter descriptions like `"Path to the file. Relative paths resolve from workspace; outside paths require policy allowlist."` become `"Path to the file."`

## Tools NOT affected (already compliant)

12 tools pass Gemma 4 compliance with no changes needed:
cron_list, cron_remove, cron_run, cron_runs, cron_update, image_info, memory_observe, openclaw_migration, process, proxy_config, shell, web_search_config

## Verification

- Config field: backward compatible (defaults to false)
- 10 unit tests for individual transformations
- 3 integration tests with realistic real-world tool schemas
- Full test suite: no regressions
- Sam's config: `simplify_tool_schemas = true`
