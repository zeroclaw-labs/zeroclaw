# Sam Screenshot Context Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make screenshot images available for the immediate Gemma 4 vision turn, then remove raw image payloads from durable conversation history so retries and later turns do not keep replaying `data:image/...` blobs.

**Architecture:** Treat screenshot-bearing tool results as one-shot multimodal context. The agent loop will keep the raw `[IMAGE:...]` marker only long enough to build the next provider request, then replace the stored history entry with a text-only summary. Provider serialization stays OpenAI-compatible with `user` multimodal content for the immediate turn, but retries and subsequent turns will operate on summarized text history instead of replaying base64 screenshots.

**Tech Stack:** Rust, ZeroClaw agent loop, OpenAI-compatible provider adapter, Tokio tests, existing multimodal marker parsing utilities.

---

## File Structure

- Modify: `src/agent/loop_.rs`
  - Own the screenshot tool-result lifecycle inside the tool loop.
  - Add a focused helper that rewrites image-bearing tool history entries into text-only summaries for durable conversation history after the immediate request has been assembled.
- Modify: `src/providers/compatible.rs`
  - Preserve the current immediate-turn serialization contract for image-bearing tool results.
  - Add focused unit tests that prove one-shot multimodal conversion still works when the agent loop passes a fresh image-bearing entry.
- Modify: `src/providers/reliable.rs`
  - Make provider retries attempt-aware so only the first attempt sees the raw screenshot-bearing request, and later attempts get a downgraded text-only copy.
  - Add focused retry tests for image-bearing requests.
- Modify: `src/multimodal.rs`
  - Add small utilities for detecting/removing image markers while preserving nearby descriptive text.
  - Keep this generic and provider-agnostic.
- Modify: `src/channels/mod.rs`
  - Add end-to-end regression coverage for failed screenshot turns not poisoning follow-up retries or later text turns with raw image markers.
- Modify: `docs/project/README.md`
- Add: `docs/project/sam-screenshot-context-lifecycle-2026-04-11.md`
  - Record the rationale, behavior change, risk, and rollback notes.

## Implementation Notes

- Preferred behavior:
  - Screenshot result enters history with `[IMAGE:...]`.
  - The very next model request can see it as multimodal `user` content.
  - Same-turn max-tokens continuation requests may keep the image, because they are part of one successful visual reasoning pass rather than a transport retry.
  - Internal provider retries for that same turn must not reuse the original raw image-bearing request.
  - After the first request is assembled, the durable history entry is rewritten to a text-only summary such as `"[Tool result]\nScreenshot captured (optimized JPEG, 101836 bytes). Image omitted from history after first vision turn."`
- Do not add a new config flag unless implementation reveals a hard need. This should be a fixed behavior for screenshot tool results.
- Do not attempt to summarize image contents with a second model call. The summary here is transport-oriented metadata, not semantic captioning.
- Keep the change scoped to image-bearing tool results. Text-only tool results should remain unchanged.

### Task 1: Add multimodal history-sanitizing helpers

**Files:**
- Modify: `src/multimodal.rs`
- Test: `src/multimodal.rs`

- [ ] **Step 1: Write the failing tests**

Add unit tests near the existing multimodal marker tests that verify:

```rust
#[tokio::test]
async fn strip_image_markers_preserves_surrounding_text() {
    let input = "Screenshot captured\n[IMAGE:data:image/jpeg;base64,abc]";
    let output = strip_image_markers_preserve_text(input);
    assert_eq!(output, "Screenshot captured");
}

#[tokio::test]
async fn strip_image_markers_handles_multiple_images() {
    let input = "Compare\n[IMAGE:data:image/png;base64,a]\n[IMAGE:data:image/jpeg;base64,b]";
    let output = strip_image_markers_preserve_text(input);
    assert_eq!(output, "Compare");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test strip_image_markers_preserves_surrounding_text strip_image_markers_handles_multiple_images`

Expected: FAIL because the helper does not exist yet.

- [ ] **Step 3: Write the minimal implementation**

Add a helper in `src/multimodal.rs`:

```rust
pub fn strip_image_markers_preserve_text(content: &str) -> String {
    let (cleaned_text, _) = parse_image_markers(content);
    cleaned_text.trim().to_string()
}
```

Add a second helper for string-level boolean detection. Do not reuse the existing slice-level `contains_image_markers(messages: &[ChatMessage])` name.

```rust
pub fn content_contains_image_markers(content: &str) -> bool {
    !parse_image_markers(content).1.is_empty()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test strip_image_markers_preserves_surrounding_text strip_image_markers_handles_multiple_images`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/multimodal.rs
git commit -m "refactor(multimodal): add image marker stripping helpers"
```

### Task 2: Make screenshot-bearing tool history one-shot

**Files:**
- Modify: `src/agent/loop_.rs`
- Test: `src/agent/loop_.rs`

- [ ] **Step 1: Write the failing tests**

Add focused tool-loop tests that cover the history lifecycle:

Use a scripted provider that overrides `Provider::chat()` and inspects `ChatRequest.messages` directly. Do not rely on a `chat_with_history()`-only test double here, because the tool loop uses `provider.chat(...)`.

```rust
#[tokio::test]
async fn image_tool_result_is_downgraded_after_immediate_request() {
    // Arrange a scripted provider that inspects the first request messages.
    // The first call should still contain the image marker.
    // After request construction, stored history should no longer contain "[IMAGE:".
}

#[tokio::test]
async fn downgraded_history_preserves_tool_metadata_without_raw_image_payload() {
    // Arrange a tool-result history entry with tool_call_id + image marker.
    // After downgrade, the tool message should still be valid JSON history
    // but must not contain raw "[IMAGE:" content.
}
```

Minimum assertions:
- first immediate request includes the screenshot marker in history/request input
- durable `history` no longer contains `[IMAGE:` after the request is prepared
- downgraded tool history still preserves non-image text and tool-call linkage metadata

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test image_tool_result_is_downgraded_after_immediate_request
cargo test downgraded_history_preserves_tool_metadata_without_raw_image_payload
```

Expected: FAIL because history currently retains raw image-bearing tool results.

- [ ] **Step 3: Write the minimal implementation**

In `src/agent/loop_.rs`, add a helper near the tool-loop history utilities:

```rust
fn downgrade_image_tool_results_for_history(history: &mut [ChatMessage]) {
    for message in history.iter_mut() {
        if message.role != "tool" {
            continue;
        }

        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&message.content) else {
            continue;
        };

        let Some(content) = value.get("content").and_then(|v| v.as_str()) else {
            continue;
        };

        if !crate::multimodal::content_contains_image_markers(content) {
            continue;
        }

        let summary = crate::multimodal::strip_image_markers_preserve_text(content);
        value["content"] = serde_json::Value::String(if summary.is_empty() {
            "Image tool result omitted from history after first vision turn.".to_string()
        } else {
            format!("{summary}\n\n[Image omitted from history after first vision turn]")
        });

        message.content = value.to_string();
    }
}
```

Call it in the tool loop after `prepared_messages` is cloned into `request_messages`, so:
- the current first-attempt request still gets the raw image
- future conversation turns use summarized history

Recommended insertion point:
- immediately after:

```rust
let mut request_messages = prepared_messages.messages.clone();
```

Important constraint:
- this history mutation alone does **not** fix internal retries inside `ReliableProvider`, because `provider.chat()` receives the already-built `request_messages` slice and `ReliableProvider` reuses that same request on every retry attempt
- the actual retry fix is covered in the next task
- same-turn `MaxTokens` continuation requests should keep using `request_messages` with the image intact unless a failing test proves that continuation itself is rejected downstream

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test image_tool_result_is_downgraded_after_immediate_request
cargo test downgraded_history_preserves_tool_metadata_without_raw_image_payload
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "fix(agent): make screenshot tool history one-shot"
```

### Task 3: Make internal retries image-safe

**Files:**
- Modify: `src/providers/reliable.rs`
- Test: `src/providers/reliable.rs`

- [ ] **Step 1: Write the failing retry tests**

Add focused tests around `ReliableProvider::chat()` that prove:

```rust
#[tokio::test]
async fn reliable_provider_first_attempt_keeps_image_but_retry_downgrades_it() {
    // Provider records each request.
    // Attempt 1 should see the raw image-bearing tool result.
    // Attempt 2 should see a downgraded text-only copy.
}

#[tokio::test]
async fn reliable_provider_text_only_requests_are_unchanged_across_retries() {
    // Control case: retries for text-only requests should remain byte-for-byte equivalent.
}
```

Minimum assertions:
- attempt 1 request contains the screenshot marker
- attempt 2 request does not contain `[IMAGE:`
- attempt 2 still preserves the non-image tool-result text

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test reliable_provider_first_attempt_keeps_image_but_retry_downgrades_it
cargo test reliable_provider_text_only_requests_are_unchanged_across_retries
```

Expected: FAIL because `ReliableProvider::chat()` currently reuses `request.messages` for every attempt.

- [ ] **Step 3: Write the minimal implementation**

Add a helper in `src/providers/reliable.rs` that derives attempt-local messages:

```rust
fn request_messages_for_attempt(messages: &[ChatMessage], attempt: u32) -> Cow<'_, [ChatMessage]> {
    if attempt == 0 {
        return Cow::Borrowed(messages);
    }

    Cow::Owned(downgrade_image_tool_results_for_retry(messages))
}
```

Implement `downgrade_image_tool_results_for_retry(messages)` as a small local transform that:
- only rewrites `role == "tool"` JSON messages whose `"content"` contains image markers
- preserves `tool_call_id`
- strips `[IMAGE:...]` and keeps surrounding text plus a transport note
- leaves all other messages unchanged

In `ReliableProvider::chat()`, build `req` from the attempt-local message slice instead of reusing `request.messages` unchanged:

```rust
let attempt_messages = request_messages_for_attempt(request.messages, attempt);
let req = ChatRequest {
    messages: attempt_messages.as_ref(),
    tools: request.tools,
    tool_choice: request.tool_choice.clone(),
};
```

Keep the change scoped to `chat()`. Do not change `chat_with_history()` or `chat_with_tools()` unless a failing test proves those paths are used for Sam's screenshot flow.

Do not alter the max-tokens continuation flow in `src/agent/loop_.rs` as part of this task. Continuations are intentionally allowed to keep the image because they extend the same successful visual turn rather than repeating a failed provider attempt.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test reliable_provider_first_attempt_keeps_image_but_retry_downgrades_it
cargo test reliable_provider_text_only_requests_are_unchanged_across_retries
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/providers/reliable.rs
git commit -m "fix(provider): downgrade screenshot payloads on retry"
```

### Task 4: Preserve immediate provider serialization behavior

**Files:**
- Modify: `src/providers/compatible.rs`
- Test: `src/providers/compatible.rs`

- [ ] **Step 1: Write or adjust the failing tests**

Keep the existing screenshot serialization regressions and add one more that proves text-only downgraded history stays text-only:

```rust
#[test]
fn downgraded_tool_result_without_image_serializes_without_image_parts() {
    let messages = vec![
        assistant_tool_call_message(),
        ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"Screenshot captured\n\n[Image omitted from history after first vision turn]"}"#),
    ];

    let native = OpenAiCompatibleProvider::convert_messages_for_native(&messages, true);
    assert_eq!(native[1].role, "tool");
    assert!(matches!(native[1].content, Some(MessageContent::Text(_))));
}
```

- [ ] **Step 2: Run tests to verify behavior**

Run:

```bash
cargo test tool_result_with_image_serializes_as_content_parts_on_wire
cargo test downgraded_tool_result_without_image_serializes_without_image_parts
```

Expected: first test still passes, new test may fail until added.

- [ ] **Step 3: Apply any minimal code changes only if needed**

The desired outcome is:
- raw image-bearing immediate turn still serializes as multimodal `user`
- downgraded history entry remains plain text and does not get re-expanded into image parts

If tests already pass, do not change runtime code here.

- [ ] **Step 4: Run tests to verify they pass**

Run the same two tests again.

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/providers/compatible.rs
git commit -m "test(provider): cover downgraded screenshot history serialization"
```

### Task 5: Add end-to-end regression for follow-up turns

**Files:**
- Modify: `src/channels/mod.rs`
- Test: `src/channels/mod.rs`

- [ ] **Step 1: Write the failing end-to-end regression**

Add a channel-level test similar to the existing failed-vision-history test, but with a scripted vision-capable provider that overrides `Provider::chat()` and captures `ChatRequest.messages` directly. Do not rely on a `chat_with_history()`-only test double for this task, because Sam's tool loop uses `provider.chat(...)`.

The test provider should:
- sees an image on the first post-tool request
- fails that first request
- succeeds on the next text-only retry

Assert:
- the first attempt receives image-bearing content
- the second attempt does not
- the final stored conversation history has no `[IMAGE:` remnants

Suggested implementation shape:

```rust
struct ChatCaptureRetryProvider {
    calls: Mutex<Vec<Vec<ChatMessage>>>,
}

#[async_trait::async_trait]
impl Provider for ChatCaptureRetryProvider {
    async fn chat(
        &self,
        request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.calls.lock().unwrap().push(request.messages.to_vec());
        // fail first, succeed second
    }
}
```

- [ ] **Step 2: Run the targeted test to verify it fails**

Run: `cargo test e2e_failed_screenshot_turn_does_not_replay_raw_image_history`

Expected: FAIL because current code reuses the same raw image-bearing history.

- [ ] **Step 3: Implement any minimal glue needed for the test**

If channel tests need a small scripted provider helper, add it locally to the test module instead of broadening production interfaces.

- [ ] **Step 4: Run the targeted test to verify it passes**

Run: `cargo test e2e_failed_screenshot_turn_does_not_replay_raw_image_history`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/channels/mod.rs
git commit -m "test(channels): cover screenshot history downgrade on retry"
```

### Task 6: Document behavior and rollback

**Files:**
- Add: `docs/project/sam-screenshot-context-lifecycle-2026-04-11.md`
- Modify: `docs/project/README.md`
- Modify: `docs/SUMMARY.md`

- [ ] **Step 1: Write the doc**

Capture:
- problem statement
- Gemma 4 best-practice rationale
- new one-shot screenshot history behavior
- retry behavior before/after
- rollback strategy: revert the history-downgrade commit

- [ ] **Step 2: Update doc indexes**

Add the new project doc to:
- `docs/project/README.md`
- `docs/SUMMARY.md`

- [ ] **Step 3: Run lightweight doc verification**

Run:

```bash
rg -n "sam-screenshot-context-lifecycle-2026-04-11" docs/project/README.md docs/SUMMARY.md
```

Expected: both index files reference the new doc.

- [ ] **Step 4: Commit**

```bash
git add docs/project/sam-screenshot-context-lifecycle-2026-04-11.md docs/project/README.md docs/SUMMARY.md
git commit -m "docs(project): record screenshot context lifecycle change"
```

### Task 7: Full verification

**Files:**
- Modify: none
- Test: `src/multimodal.rs`, `src/agent/loop_.rs`, `src/providers/compatible.rs`, `src/channels/mod.rs`

- [ ] **Step 1: Run formatting**

Run: `cargo fmt --all`

Expected: no diff after formatting rerun.

- [ ] **Step 2: Run targeted tests**

Run:

```bash
cargo test strip_image_markers_preserves_surrounding_text
cargo test strip_image_markers_handles_multiple_images
cargo test image_tool_result_is_downgraded_after_immediate_request
cargo test downgraded_history_preserves_tool_metadata_without_raw_image_payload
cargo test reliable_provider_first_attempt_keeps_image_but_retry_downgrades_it
cargo test reliable_provider_text_only_requests_are_unchanged_across_retries
cargo test tool_result_with_image_serializes_as_content_parts_on_wire
cargo test downgraded_tool_result_without_image_serializes_without_image_parts
cargo test e2e_failed_screenshot_turn_does_not_replay_raw_image_history
```

Expected: PASS.

- [ ] **Step 3: Run repo-level checks that matter for touched code**

Run:

```bash
cargo clippy --all-targets -- -D warnings
cargo test
```

Expected: PASS, or document any pre-existing unrelated failure.

- [ ] **Step 4: Final commit if verification required code/doc touch-ups**

```bash
git add -A
git commit -m "chore: finalize screenshot context lifecycle verification"
```

Only create this commit if verification required additional scoped fixes.

## Rollout Notes

- Deploy only after the targeted retry-history tests pass.
- Post-deploy validation should reproduce the Sam screenshot self-check and confirm:
  - one screenshot-bearing request reaches LiteLLM
  - retries do not resend raw `data:image/...` history
  - later text-only follow-ups in the same conversation succeed without image context bleed

## Risks

- If history is downgraded too early, the immediate vision turn could lose access to the screenshot.
- If the reliable-provider retry transform is missed or incomplete, retries will continue replaying the raw image payload even if durable history is downgraded.
- If continuation handling is changed accidentally, long visual answers may lose access to the screenshot mid-response.
- If the backend actually requires resending the image for successful internal retry semantics, this change may trade one failure mode for another; the targeted retry tests should keep that behavior explicit.
