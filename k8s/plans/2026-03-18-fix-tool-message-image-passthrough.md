# Fix Tool Message Image Passthrough Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix screenshot/image_info 500 errors by ensuring ZeroClaw's compatible provider sends `[IMAGE:]` markers in tool messages as structured `image_url` content parts, not as raw strings that litellm passes through as text tokens.

**Architecture:** ZeroClaw stores tool results as `ChatMessage::tool(json_string)` where the JSON wraps `tool_call_id` and `content`. The compatible provider parses this JSON at `convert_messages_for_native()` and calls `to_message_content()` which extracts `[IMAGE:]` markers into `image_url` content parts. This conversion works correctly. However, the converted `NativeMessage` is serialized and sent to litellm, which then does its OWN message transformation via `hosted_vllm`. The bug: ZeroClaw correctly converts the markers to content parts, litellm receives them as content parts, and litellm forwards them. BUT — when ZeroClaw sends the tool message content as a plain string (not content parts) because the JSON parse path reconstructs it differently, litellm treats the content as raw text and sends 1.8MB of base64 as tokens. The fix ensures the content parts with `image_url` are always sent in the wire format, never as embedded strings.

**Tech Stack:** Rust, serde_json, ZeroClaw compatible provider

---

## Root Cause Detail

The flow for a tool result containing an `[IMAGE:]` marker:

```
ZeroClaw agent loop
  → ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"...text...\n[IMAGE:data:image/png;base64,...]"}"#)

ZeroClaw compatible provider (convert_messages_for_native)
  → Parses JSON string, extracts content_text with [IMAGE:] marker
  → Calls to_message_content("tool", &content_text, true)
  → Returns MessageContent::Parts([Text{...}, ImageUrl{url: "data:image/..."}])
  → Serializes NativeMessage { role: "tool", content: Some(Parts([...])), tool_call_id: Some("call_1") }

Sent to litellm as JSON:
  → { "role": "tool", "tool_call_id": "call_1", "content": [{"type":"text","text":"..."}, {"type":"image_url","image_url":{"url":"data:image/..."}}] }

litellm receives and forwards to llama-server → WORKS
```

This path works in tests. But in practice, the tool message may take a different
code path where content ends up as a string instead of parts. This happens when:
- The JSON parse at line 1679 fails (content too large, malformed)
- The tool_call_id doesn't match (orphan detection at line 1711-1718)
- The message falls through to the generic handler at line 1735

In all fallback cases, the content goes as `MessageContent::Text(raw_string)`
which includes the full `[IMAGE:]` marker with 1.8MB of base64. litellm treats
this as text → 1.3M tokens → context exceeded or URL parse error.

**The fix:** Add a safety net that strips `[IMAGE:]` markers from any
`MessageContent::Text` content in tool messages, converting them to
`MessageContent::Parts` with proper `image_url` entries. This ensures
images always go as structured parts regardless of which code path is taken.

---

## File Map

```
src/providers/compatible.rs
  - Modify convert_messages_for_native() — add image marker extraction for
    tool messages that fall through to the generic text path
  - Modify to_message_content() — already handles [IMAGE:] extraction,
    but verify it's called for ALL tool message paths

src/multimodal.rs
  - Read-only reference — parse_image_markers() is the canonical extractor
```

---

### Task 1: Add test proving the bug exists

**Files:**
- Modify: `src/providers/compatible.rs` (test module)

- [ ] **Step 1: Write the failing test**

This test sends a tool message through `convert_messages_for_native` where the
tool_call_id is missing from the assistant's tool_calls set (orphan path). The
content contains an `[IMAGE:]` marker. The test asserts the image ends up as
an `image_url` content part, not embedded in a text string.

```rust
#[test]
fn tool_message_with_image_marker_in_fallback_path_extracts_image() {
    use crate::providers::ChatMessage;

    // Simulate: tool result with [IMAGE:] marker falls through to generic handler
    // because tool_call_id doesn't match any assistant tool_call
    let tool_content = serde_json::json!({
        "tool_call_id": "orphan_call_id",
        "content": "Screenshot captured\n[IMAGE:data:image/jpeg;base64,/9j/4AAQ]"
    });

    let messages = vec![
        ChatMessage::user("take a screenshot".to_string()),
        // No assistant message with tool_calls — so the tool_call_id is orphan
        ChatMessage::tool(tool_content.to_string()),
    ];

    let native = OpenAiCompatibleProvider::convert_messages_for_native(&messages, true);

    // Find the message that contains our tool result
    // (it may be converted to role: "user" as fallback)
    let has_image_part = native.iter().any(|msg| {
        if let Some(MessageContent::Parts(parts)) = &msg.content {
            parts.iter().any(|p| matches!(p, MessagePart::ImageUrl { .. }))
        } else {
            false
        }
    });

    // The [IMAGE:] marker should NOT be in any text content as raw string
    let has_raw_marker = native.iter().any(|msg| {
        match &msg.content {
            Some(MessageContent::Text(t)) => t.contains("[IMAGE:"),
            Some(MessageContent::Parts(parts)) => parts.iter().any(|p| {
                if let MessagePart::Text { text } = p {
                    text.contains("[IMAGE:")
                } else {
                    false
                }
            }),
            None => false,
        }
    });

    assert!(
        has_image_part || !has_raw_marker,
        "image should be extracted as image_url part, not left as raw [IMAGE:] text"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test --lib providers::compatible::tests::tool_message_with_image_marker_in_fallback -- --nocapture
```

Expected: FAIL — the orphan tool message falls through with raw `[IMAGE:]` text.

- [ ] **Step 3: Commit failing test**

```bash
git add src/providers/compatible.rs
git commit -m "test: prove tool message image marker not extracted in fallback path"
```

---

### Task 2: Fix the fallback path to extract images

**Files:**
- Modify: `src/providers/compatible.rs:1720-1732` (orphan/missing tool_call_id fallback)

The orphan tool message path at line 1721-1731 creates a `MessageContent::Text`
with the raw content. It should use `to_message_content` instead to extract
`[IMAGE:]` markers.

- [ ] **Step 1: Apply the fix**

Replace the orphan/missing tool_call_id fallback (lines ~1721-1731):

```rust
// BEFORE:
native_messages.push(NativeMessage {
    role: "user".to_string(),
    content: Some(MessageContent::Text(format!(
        "[Tool result]\n{}",
        content_text
    ))),
    tool_call_id: None,
    tool_calls: None,
    reasoning_content: None,
});

// AFTER:
native_messages.push(NativeMessage {
    role: "user".to_string(),
    content: Some(Self::to_message_content(
        "user",
        &format!("[Tool result]\n{}", content_text),
        allow_user_image_parts,
    )),
    tool_call_id: None,
    tool_calls: None,
    reasoning_content: None,
});
```

- [ ] **Step 2: Run test to verify it passes**

```bash
cargo test --lib providers::compatible::tests::tool_message_with_image_marker_in_fallback -- --nocapture
```

Expected: PASS

- [ ] **Step 3: Run all provider tests**

```bash
cargo test --lib providers::compatible::tests
```

Expected: All pass (102+ tests)

- [ ] **Step 4: Commit**

```bash
git add src/providers/compatible.rs
git commit -m "fix: extract [IMAGE:] markers from tool messages in fallback path

Tool messages that fall through to the orphan/missing-id path were
wrapped in MessageContent::Text with raw [IMAGE:] markers. litellm
then sent the 1.8MB base64 as text tokens, causing context exceeded
or 'Invalid url value' errors from llama-server.

Now uses to_message_content() for the fallback path, which extracts
[IMAGE:] markers into proper image_url content parts."
```

---

### Task 3: Add defense for the generic handler

**Files:**
- Modify: `src/providers/compatible.rs:1735-1753` (generic message handler)

The generic handler at line 1735 already calls `to_message_content()` which
handles `[IMAGE:]` extraction. BUT — it only extracts for `role == "user" ||
role == "tool"`. The fallback path from Task 2 converts the role to `"user"`,
so this is already covered. However, if a tool message falls all the way through
(JSON parse fails at line 1679), it hits the generic handler with `role == "tool"`.

Verify this path also extracts images correctly.

- [ ] **Step 1: Write test for JSON-parse-failure path**

```rust
#[test]
fn tool_message_with_unparseable_json_extracts_image() {
    use crate::providers::ChatMessage;

    // Tool message content is NOT valid JSON — goes straight to generic handler
    let raw_content = "Screenshot result\n[IMAGE:data:image/jpeg;base64,/9j/4AAQ]";
    let messages = vec![
        ChatMessage::user("test".to_string()),
        ChatMessage {
            role: "tool".to_string(),
            content: raw_content.to_string(),  // Not JSON-wrapped
        },
    ];

    let native = OpenAiCompatibleProvider::convert_messages_for_native(&messages, true);

    let has_raw_marker = native.iter().any(|msg| {
        match &msg.content {
            Some(MessageContent::Text(t)) => t.contains("[IMAGE:"),
            _ => false,
        }
    });

    assert!(
        !has_raw_marker,
        "image should be extracted even when tool content is not valid JSON"
    );
}
```

- [ ] **Step 2: Run test — should already pass**

The generic handler at line 1737 calls `to_message_content(&message.role, ...)`,
and `to_message_content` checks `role == "tool"` at line 1601. So this should
already work. If it passes, no code change needed — just the test.

```bash
cargo test --lib providers::compatible::tests::tool_message_with_unparseable_json -- --nocapture
```

- [ ] **Step 3: Commit test**

```bash
git add src/providers/compatible.rs
git commit -m "test: verify tool messages with non-JSON content also extract images"
```

---

### Task 4: Build, deploy, verify

**Files:**
- Modify: `k8s/sam/04_zeroclaw_sandbox.yaml` (image version bump)
- Modify: `Dockerfile.sam` (if needed)

- [ ] **Step 1: Run full test suite**

```bash
cargo test --lib agent::loop_::tests providers::compatible::tests
```

- [ ] **Step 2: Build new image**

```bash
docker build -f Dockerfile.sam -t citizendaniel/zeroclaw-sam:v1.4.15 .
docker push citizendaniel/zeroclaw-sam:v1.4.15
```

- [ ] **Step 3: Update sandbox image version**

```bash
sed -i 's/zeroclaw-sam:v1.4.14/zeroclaw-sam:v1.4.15/g' k8s/sam/04_zeroclaw_sandbox.yaml
```

- [ ] **Step 4: Apply and restart**

```bash
kubectl apply -f k8s/sam/04_zeroclaw_sandbox.yaml
kubectl delete pod -n ai-agents zeroclaw
kubectl wait --for=condition=Ready pod/zeroclaw -n ai-agents --timeout=120s
```

- [ ] **Step 5: Verify — have Sam take a screenshot**

Send Sam: "Take a screenshot of wikipedia.org and describe what you see"

Monitor llama-swap activity for any 500 errors.

- [ ] **Step 6: Commit and push**

```bash
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "chore(k8s/sam): bump image to v1.4.15 — fix tool message image passthrough"
git push
```

---

## Summary

| Task | What | Risk |
|------|------|------|
| 1 | Failing test proving the bug | Low |
| 2 | Fix orphan/missing-id fallback to use to_message_content | Low — single line change |
| 3 | Verify generic handler already works | Low — test only |
| 4 | Build, deploy, verify | Low — standard deploy |

**Dependencies:** 1 → 2 → 3 → 4 (sequential)

**Rollback:** Revert image to v1.4.14

**What this fixes:** Screenshot and image_info 500 "Invalid url value" errors
when tool messages take the orphan/fallback path in the compatible provider.

**What this doesn't fix:**
- Image optimization fallback for very tall screenshots (separate improvement)
- The root issue of why some tool_call_ids are orphaned (investigate separately)
