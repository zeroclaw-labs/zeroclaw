# Sam Screenshot Terminal Vision Turn Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the first post-screenshot Gemma request a single multimodal turn with no appended safety heartbeat, then validate whether that alone resolves the backend error before considering any tool-schema changes.

**Architecture:** Keep screenshot capture and multimodal conversion unchanged, but mark the immediate follow-up request as a special screenshot-analysis turn for heartbeat handling only. In `run_tool_call_loop`, once a screenshot-bearing tool result is present in the prepared request, suppress extra user-message injection for that request. Do not remove `tools` or `tool_choice` in the primary fix, because the agent loop cannot know in advance whether the model will need another tool call. If heartbeat-only remediation still reproduces `Invalid url value`, a follow-up experiment can test tool-schema suppression as a separate change.

**Tech Stack:** Rust, ZeroClaw agent loop, provider trait request assembly, OpenAI-compatible provider adapter, Tokio tests, existing multimodal marker utilities.

---

## File Structure

- Modify: `src/agent/loop_.rs`
  - Own detection of the immediate screenshot-bearing request and suppress safety-heartbeat injection on that call.
  - Add a narrow helper that decides when the request can omit native tools/tool_choice for a terminal vision answer.
- Modify: `src/providers/traits.rs`
  - Keep `ChatRequest` unchanged unless implementation proves a small explicit flag is cleaner than deriving behavior from `messages` + `tools`.
  - If a flag becomes necessary, keep it request-local and non-configurable.
- Modify: `src/providers/compatible.rs`
  - Add regression coverage that the OpenAI-compatible request keeps the screenshot as a `user` multimodal message while preserving whatever `tools` / `tool_choice` the agent loop passes.
- Modify: `src/providers/reliable.rs`
  - Add a retry-focused regression only if implementation introduces new request-local metadata for heartbeat suppression. Otherwise keep this as a verification-only checkpoint.
- Modify: `src/multimodal.rs`
  - Reuse the existing image-marker helpers from the prior fix; only extend if a tiny “last message is screenshot-bearing multimodal user” helper materially simplifies the agent-loop logic.
- Modify: `docs/project/README.md`
- Add: `docs/project/sam-screenshot-terminal-vision-turn-2026-04-12.md`
  - Record the new RCA and the narrower request-shape remediation.

## Implementation Notes

- The validated bad production shape was:
  1. tool call executes screenshot
  2. screenshot result becomes a multimodal `user` message
  3. a second `user` safety heartbeat is appended
  4. request also carries native tool schema
- The validated good direct shape was:
  1. one `user` multimodal message
  2. no trailing `user` heartbeat
  3. no tool schema
- What has been proven:
  - heartbeat is definitely an extra structural difference between failing production requests and working direct probes
  - tool schema is also a difference, but it has **not** been isolated as causal yet
- Therefore the primary remediation must target heartbeat suppression first. Tool omission is a follow-up experiment, not part of the initial fix.
- Scope of this fix:
  - change only the **first** post-screenshot LLM request
  - do not change screenshot capture, image optimization, or the one-shot history downgrade logic
  - do not change same-turn max-token continuations unless tests show they also need the stripped shape
- Preferred behavior:
  - if the immediate request already contains a screenshot-bearing multimodal `user` message, skip safety-heartbeat injection for that request
  - preserve `tools` and `tool_choice` in the initial remediation so screenshot-driven tool workflows still work
  - only test `tools` / `tool_choice` suppression in a follow-up experiment if heartbeat-only remediation still reproduces the error
- Do not add a config flag unless the implementation proves that existing behavior elsewhere depends on unconditional heartbeat injection.

### Task 1: Add failing tests for heartbeat suppression on the immediate screenshot request

**Files:**
- Modify: `src/agent/loop_.rs`
- Test: `src/agent/loop_.rs`

- [ ] **Step 1: Write the failing tests**

Add focused tests around `run_tool_call_loop` request assembly:

```rust
#[tokio::test]
async fn screenshot_followup_request_skips_safety_heartbeat() {
    // Arrange a scripted provider that captures the first request.
    // Drive the loop through a browser screenshot tool result.
    // Assert the request contains the multimodal screenshot turn
    // and does not append a trailing "[Safety Heartbeat".
}
```

Minimum assertions:
- the request contains the screenshot-bearing multimodal `user` message
- no later message in that same request contains `[Safety Heartbeat`

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test screenshot_followup_request_skips_safety_heartbeat
```

Expected: FAIL because `run_tool_call_loop` currently appends the heartbeat unconditionally.

- [ ] **Step 3: Write the minimal implementation**

In `src/agent/loop_.rs`, add a helper near the existing heartbeat helpers:

```rust
fn request_contains_screenshot_followup_turn(messages: &[ChatMessage]) -> bool {
    // Return true when the current request already includes a screenshot-bearing
    // multimodal user turn produced from a tool result.
}
```

Then gate heartbeat injection:

```rust
let screenshot_followup_turn = request_contains_screenshot_followup_turn(&request_messages);

if let Some(ref hb) = heartbeat_config {
    if should_inject_safety_heartbeat(iteration, hb.interval) && !screenshot_followup_turn {
        request_messages.push(ChatMessage::user(reminder));
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test screenshot_followup_request_skips_safety_heartbeat
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agent/loop_.rs
git commit -m "fix(agent): suppress heartbeat on screenshot follow-up"
```

### Task 2: Add provider-shape regression coverage

**Files:**
- Modify: `src/providers/compatible.rs`
- Test: `src/providers/compatible.rs`

- [ ] **Step 1: Write the failing tests**

Add request-serialization tests around the OpenAI-compatible payload:

```rust
#[tokio::test]
async fn screenshot_followup_request_keeps_user_multimodal_image_content() {
    // Assert the same request still serializes the screenshot into
    // a user content array with text + image_url blocks.
}

#[tokio::test]
async fn screenshot_followup_request_preserves_tool_schema_when_present() {
    // Assert provider serialization does not silently strip tools/tool_choice.
    // The heartbeat fix is in the agent loop, not here.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test screenshot_followup_request_keeps_user_multimodal_image_content
cargo test screenshot_followup_request_preserves_tool_schema_when_present
```

Expected: FAIL only if the current regression fixtures do not yet cover the screenshot follow-up shape.

- [ ] **Step 3: Write the minimal implementation**

Preferred approach:
- keep provider code unchanged
- add regression tests proving the screenshot stays in `user` multimodal form and provider serialization faithfully reflects the agent-loop request

Only patch provider code if Task 1 reveals a real serialization mismatch that contradicts the stored LiteLLM request body.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test screenshot_followup_request_keeps_user_multimodal_image_content
cargo test screenshot_followup_request_preserves_tool_schema_when_present
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/providers/compatible.rs
git commit -m "test(compatible): cover screenshot follow-up request shape"
```

### Task 3: Verify retry behavior only if new request-local metadata is introduced

**Files:**
- Modify: `src/providers/reliable.rs` (only if needed)
- Test: `src/providers/reliable.rs` (only if needed)

- [ ] **Step 1: Write the failing tests**

Only add this task if Task 1 introduces new request-local state that could be lost or recomputed during retries. If heartbeat suppression is fully realized in the initial `request_messages` before `provider.chat(...)`, retries should already preserve the stripped shape and no code change is needed in `ReliableProvider`.

If needed, add a retry-specific regression:

```rust
#[tokio::test]
async fn screenshot_terminal_request_retries_do_not_reintroduce_heartbeat_or_tools() {
    // First attempt captures a screenshot follow-up request with no heartbeat.
    // Retry attempts should preserve the same stripped request shape.
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test screenshot_followup_request_retries_do_not_reintroduce_heartbeat
```

Expected: FAIL only if retries rebuild or mutate the request in a way that reintroduces the heartbeat.

- [ ] **Step 3: Write the minimal implementation**

Only if the test fails:
- thread the terminal screenshot decision through the retry-local request rebuild
- keep the fix local to `ReliableProvider::chat()`
- do not broaden it into a general retry policy change

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test screenshot_followup_request_retries_do_not_reintroduce_heartbeat
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/providers/reliable.rs
git commit -m "fix(reliable): preserve screenshot follow-up heartbeat suppression on retry"
```

### Task 4: Document the RCA and rollback path

**Files:**
- Add: `docs/project/sam-screenshot-terminal-vision-turn-2026-04-12.md`
- Modify: `docs/project/README.md`

- [ ] **Step 1: Write the doc**

Document:
- the validated failing production request sequence
- the successful direct probe sequence
- why the initial fix targets heartbeat suppression on the immediate screenshot analysis turn
- why tool-schema suppression is intentionally deferred pending a heartbeat-only validation result
- rollback: revert the terminal-turn gating if it regresses legitimate screenshot-driven tool chains

- [ ] **Step 2: Link it from the project index**

Add the new entry to `docs/project/README.md`.

- [ ] **Step 3: Verify docs formatting**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS for Rust formatting; docs require visual sanity check only.

- [ ] **Step 4: Commit**

```bash
git add docs/project/README.md docs/project/sam-screenshot-terminal-vision-turn-2026-04-12.md
git commit -m "docs(project): record terminal screenshot request RCA"
```

### Task 5: End-to-end verification against Sam

**Files:**
- No code changes expected

- [ ] **Step 1: Run focused Rust verification**

Run:

```bash
cargo test screenshot_followup_request_skips_safety_heartbeat
cargo test screenshot_followup_request_keeps_user_multimodal_image_content
cargo test screenshot_followup_request_preserves_tool_schema_when_present
```

Expected: PASS.

If Task 3 was needed, also run:

```bash
cargo test screenshot_followup_request_retries_do_not_reintroduce_heartbeat
```

Expected: PASS.

- [ ] **Step 2: Build and deploy the Sam image**

Run the same deployment flow used for `v1.5.10`, with a fresh image tag and the updated `k8s/sam/04_zeroclaw_sandbox.yaml`.

- [ ] **Step 3: Re-run the Wikipedia image-grounded validation**

Prompt Sam:

```text
Open https://en.wikipedia.org/wiki/Main_Page, navigate to the picture of the day, take a screenshot of the picture itself, and describe the photo in 2-3 sentences. Do not rely on page text alone; use the screenshot to verify what is in the image.
```

Expected:
- screenshot capture succeeds
- no `Invalid url value`
- Sam returns a description grounded in the image

- [ ] **Step 4: Check LiteLLM spend logs**

Verify the stored request for the successful screenshot turn:
- contains the screenshot-bearing multimodal `user` message
- omits the trailing safety heartbeat from that same request
- note whether `tools`/`tool_choice` are still present; that is expected in the primary fix

- [ ] **Step 5: Commit deployment manifest changes**

```bash
git add k8s/sam/04_zeroclaw_sandbox.yaml
git commit -m "deploy(sam): roll out terminal screenshot request fix"
```

## Risks and Rollback

- Main risk: heartbeat suppression could weaken safety-policy reinjection for one screenshot-bearing round.
- Mitigation: keep the suppression narrowly scoped to the immediate screenshot follow-up request and leave later rounds unchanged.
- Follow-up experiment risk: suppressing tools on the screenshot follow-up request could block legitimate workflows where the model must inspect the screenshot and then call another tool before answering.
- Rollback: revert the heartbeat-suppression commit first if it regresses policy behavior; if a later tool-omission experiment is attempted, keep that in a separate commit for clean rollback.
