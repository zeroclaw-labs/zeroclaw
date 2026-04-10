# Malformed Tool Call Sanitization Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent malformed `<tool_call>` tags in conversation history from crashing llama-server's chat template parser, which causes "Invalid url value" and "Failed to parse input" errors.

**Architecture:** When ZeroClaw's model emits an empty or unparseable `<tool_call>` block, the parsing layer detects it (logs a warning) but the raw text is preserved in the assistant message via a fallback path. This corrupted history then causes llama-server to reject subsequent requests. The fix sanitizes the assistant message content before it enters conversation history, stripping or replacing malformed `<tool_call>` tags that weren't successfully parsed into tool calls.

**Tech Stack:** Rust, ZeroClaw agent loop (`src/agent/loop_.rs`), parsing module (`src/agent/loop_/parsing.rs`)

---

## Root Cause Detail

Two code paths build the assistant history message when tool calls are parsed:

**Path A — Native tool calling (Sam's path):**
```
build_native_assistant_history_from_parsed_calls(response_text, calls, reasoning)
```
This function (line 837) converts parsed `ParsedToolCall` structs into clean JSON.
BUT: if `tool_calls` is empty (because parsing found `<tool_call>\n</tool_call>` with
no body), the function returns `None`. The caller (line 1855) then falls back to
`response_text.clone()` — which contains the raw malformed `<tool_call>` tags.

**Path B — Non-native (XML-in-text):**
```
build_assistant_history_with_tool_calls(response_text, tool_calls)
```
This function (line 874) reconstructs clean `<tool_call>` tags from parsed calls
only. But the `response_text` passed to it still contains the original raw tags.

**The bug:** In both paths, when parsing fails (empty tag, malformed JSON), the raw
`response_text` with its broken `<tool_call>` tags enters conversation history.
llama-server's Jinja chat template then parses these tags structurally and crashes.

---

## File Map

```
src/agent/loop_.rs
  - build_native_assistant_history_from_parsed_calls() :837 — add fallback sanitization
  - build_assistant_history_with_tool_calls() :874 — strip raw tags from response_text
  - Tool call result handling :1848-1873 — sanitize before storing

src/agent/loop_/parsing.rs
  - Extract new public fn: strip_malformed_tool_call_tags()
  - Existing tag constants: TOOL_CALL_OPEN_TAGS, TOOL_CALL_CLOSE_TAGS :~1200

tests:
  - src/agent/loop_/parsing.rs — test strip function
  - src/agent/loop_.rs — test history sanitization end-to-end
```

---

## Task 1: Add `strip_malformed_tool_call_tags()` to parsing module

**Files:**
- Modify: `src/agent/loop_/parsing.rs`

This function takes the raw response text and a list of successfully parsed
tool call spans, and strips any `<tool_call>...</tool_call>` blocks that
weren't part of a successful parse. This preserves the model's text output
while removing the toxic tags.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn strip_malformed_tool_call_tags_removes_empty_tags() {
    let input = "Let me check.\n<tool_call>\n</tool_call>\nDone.";
    let result = strip_malformed_tool_call_tags(input, &[]);
    assert_eq!(result, "Let me check.\n\nDone.");
}

#[test]
fn strip_malformed_tool_call_tags_preserves_text_around_valid_calls() {
    let input = "Thinking...\n<tool_call>\n{\"name\":\"shell\"}\n</tool_call>\nResult received.";
    // When the call was successfully parsed, the tag span is in parsed_spans
    let parsed_spans = vec![(12, 63)]; // start..end of the valid <tool_call>...</tool_call>
    let result = strip_malformed_tool_call_tags(input, &parsed_spans);
    // Valid tags should be preserved
    assert!(result.contains("<tool_call>"));
}

#[test]
fn strip_malformed_tool_call_tags_handles_mixed() {
    let input = "A\n<tool_call>{\"name\":\"shell\"}</tool_call>\nB\n<tool_call>\n</tool_call>\nC";
    // First tag was parsed, second was not
    let parsed_spans = vec![(2, 42)];
    let result = strip_malformed_tool_call_tags(input, &parsed_spans);
    assert!(result.contains("A"));
    assert!(result.contains("B"));
    assert!(result.contains("C"));
    assert!(!result.contains("<tool_call>\n</tool_call>"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test --lib agent::loop_::parsing::tests::strip_malformed -- --nocapture
```
Expected: compile error (function doesn't exist yet).

- [ ] **Step 3: Implement `strip_malformed_tool_call_tags`**

```rust
/// Strip `<tool_call>...</tool_call>` blocks from text that weren't successfully
/// parsed into tool calls. This prevents malformed tags (empty bodies, broken JSON)
/// from corrupting conversation history and crashing the chat template parser.
///
/// `parsed_call_count` is the number of tool calls that were successfully extracted.
/// If it's > 0, the model produced at least some valid calls and the raw tags for
/// those should be stripped too (they'll be reconstructed as structured JSON in
/// the assistant history). If it's 0 and tags exist, all tags are malformed.
pub fn strip_unparsed_tool_call_tags(text: &str, parsed_call_count: usize) -> String {
    // If no tool call tags exist, return as-is (fast path)
    if !TOOL_CALL_OPEN_TAGS.iter().any(|tag| text.contains(tag)) {
        return text.to_string();
    }

    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while !remaining.is_empty() {
        // Find the next open tag
        let next_open = TOOL_CALL_OPEN_TAGS
            .iter()
            .filter_map(|tag| remaining.find(tag).map(|pos| (pos, *tag)))
            .min_by_key(|(pos, _)| *pos);

        let Some((open_pos, open_tag)) = next_open else {
            result.push_str(remaining);
            break;
        };

        // Add text before the tag
        result.push_str(&remaining[..open_pos]);

        let after_open = &remaining[open_pos + open_tag.len()..];

        // Find matching close tag
        let close_found = TOOL_CALL_CLOSE_TAGS
            .iter()
            .filter_map(|tag| after_open.find(tag).map(|pos| (pos, *tag)))
            .min_by_key(|(pos, _)| *pos);

        if let Some((close_pos, close_tag)) = close_found {
            // Skip the entire <tool_call>...</tool_call> block
            remaining = &after_open[close_pos + close_tag.len()..];
        } else {
            // No close tag — skip just the open tag
            remaining = after_open;
        }
    }

    // Clean up extra blank lines left by stripping
    let cleaned = result
        .lines()
        .collect::<Vec<_>>()
        .join("\n");

    cleaned.trim().to_string()
}
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cargo test --lib agent::loop_::parsing::tests::strip_malformed -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_/parsing.rs
git commit -m "feat: add strip_unparsed_tool_call_tags to sanitize malformed tool call blocks"
```

---

## Task 2: Sanitize assistant history before storing

**Files:**
- Modify: `src/agent/loop_.rs`

Apply `strip_unparsed_tool_call_tags` to the `response_text` before it enters
conversation history, specifically in the fallback path where
`build_native_assistant_history_from_parsed_calls` returns `None`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn malformed_tool_call_does_not_corrupt_history() {
    // Simulate: model returns empty <tool_call> tags
    let response_text = "Let me check.\n<tool_call>\n</tool_call>";
    let calls: Vec<ParsedToolCall> = vec![]; // parsing found nothing

    // The native history builder returns None when calls is empty
    let native = build_native_assistant_history_from_parsed_calls(
        response_text, &calls, None
    );
    assert!(native.is_none());

    // The fallback should sanitize response_text
    let fallback = strip_unparsed_tool_call_tags(response_text, calls.len());
    assert!(!fallback.contains("<tool_call>"));
    assert!(fallback.contains("Let me check."));
}
```

- [ ] **Step 2: Apply sanitization in the fallback path**

In `src/agent/loop_.rs` around line 1848-1858, change:

```rust
// BEFORE:
let assistant_history_content = if native_calls.is_empty() {
    if use_native_tools {
        build_native_assistant_history_from_parsed_calls(
            &response_text,
            &calls,
            reasoning_content.as_deref(),
        )
        .unwrap_or_else(|| response_text.clone())
    } else {
        response_text.clone()
    }
```

To:

```rust
// AFTER:
let assistant_history_content = if native_calls.is_empty() {
    if use_native_tools {
        build_native_assistant_history_from_parsed_calls(
            &response_text,
            &calls,
            reasoning_content.as_deref(),
        )
        .unwrap_or_else(|| {
            // Native history builder failed (no valid calls parsed).
            // Strip malformed <tool_call> tags to prevent them from
            // corrupting the chat template on subsequent LLM calls.
            parsing::strip_unparsed_tool_call_tags(&response_text, calls.len())
        })
    } else {
        response_text.clone()
    }
```

- [ ] **Step 3: Also sanitize the non-native fallback path**

At line 2039 (`history.push(ChatMessage::assistant(response_text.clone()))`),
apply the same sanitization for the case where no tool calls were found but
the response still contains malformed tags:

```rust
// BEFORE:
history.push(ChatMessage::assistant(response_text.clone()));

// AFTER:
let sanitized = parsing::strip_unparsed_tool_call_tags(&response_text, 0);
history.push(ChatMessage::assistant(sanitized));
```

- [ ] **Step 4: Run full test suite**

```bash
cargo test --lib agent::loop_ -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "fix: sanitize malformed tool_call tags from history to prevent template parser crashes

When the model emits empty or unparseable <tool_call> blocks, they were
preserved in conversation history via the response_text fallback. This
caused llama-server's Jinja chat template parser to fail with 'Invalid
url value' or 'Failed to parse input' errors on subsequent requests.

Now strips malformed tags before storing in history, preserving the
model's text output while removing the toxic structural elements."
```

---

## Task 3: Add provider-level defense (belt + suspenders)

**Files:**
- Modify: `src/providers/compatible.rs`

Even with the history sanitization, add a defense layer in the compatible
provider's message conversion. If an assistant message content string still
contains raw `<tool_call>` tags when native tool calling is active, strip
them before serialization — they should have been converted to the `tool_calls`
JSON array and don't belong in the content field.

- [ ] **Step 1: Add sanitization in `convert_messages_for_native`**

In the assistant message handling within `convert_messages_for_native` (~line 1640),
after extracting tool_calls, strip any remaining raw tags from the content:

```rust
// When building native messages for assistant role:
// If this message has tool_calls (from native_calls), the content should
// NOT contain raw <tool_call> tags — those are structural artifacts that
// the chat template parser will choke on.
if message.role == "assistant" {
    let content_str = /* existing content extraction */;
    let cleaned = parsing::strip_unparsed_tool_call_tags(&content_str, 0);
    // Use cleaned as the content...
}
```

- [ ] **Step 2: Test with malformed history**

```bash
cargo test --lib providers::compatible::tests -- --nocapture
```

- [ ] **Step 3: Commit**

```bash
git add src/providers/compatible.rs
git commit -m "fix: strip residual tool_call tags from assistant content in native mode

Defense-in-depth: even if the agent loop fails to sanitize, the provider
won't send raw <tool_call> tags in the content field when native tool
calling is active."
```

---

## Task 4: Build and deploy

**Files:**
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (image version bump)

- [ ] **Step 1: Run full validation**

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

- [ ] **Step 2: Build new image**

```bash
# Build and push zeroclaw-sam image with the fix
docker build -t citizendaniel/zeroclaw-sam:v1.4.13 .
docker push citizendaniel/zeroclaw-sam:v1.4.13
```

- [ ] **Step 3: Update sandbox image version**

In `k8s/sam/04_zeroclaw_sandbox.yaml`, bump both container image references:
```yaml
image: citizendaniel/zeroclaw-sam:v1.4.13
```

- [ ] **Step 4: Apply and restart**

```bash
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 5: Verify by triggering a browser test**

Send Sam a browser test and monitor llama-swap activity for any 400/500 errors.

- [ ] **Step 6: Commit**

```bash
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "chore(k8s/sam): bump image to v1.4.13 — malformed tool_call sanitization"
```

---

## Summary

| Task | What | Risk |
|------|------|------|
| 1 | `strip_unparsed_tool_call_tags()` in parsing module | Low — new function, no existing behavior change |
| 2 | Apply sanitization in agent loop fallback paths | Medium — changes history content, needs careful testing |
| 3 | Provider-level defense (belt + suspenders) | Low — additional safety layer |
| 4 | Build, deploy, verify | Low — standard deploy |

**Dependencies:** Task 1 → Task 2 → Task 3 → Task 4 (sequential).

**Rollback:** Revert image to v1.4.12 in sandbox YAML.

**What this fixes:**
- "Invalid url value" errors during screenshot/image workflows
- "Failed to parse input" errors after long tool call sequences
- Any future template parser crash caused by malformed `<tool_call>` artifacts

**What this doesn't fix:**
- Model quality degradation in long contexts (separate concern)
- Browser skill lacking a turn budget guidance (separate skill update)
