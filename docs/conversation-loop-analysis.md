# Conversation Loop Analysis — DaemonClaw vs. Research Systems

## Problem Statement

When Richard sends a message on Telegram:
- **Silence until completion** — no progressive feedback during tool calling
- **No response on errors** — if the LLM call or tool execution fails, the user gets nothing
- **No cancellation visibility** — `/stop` works but no intermediate state feedback

This document compares how each system handles the "inbound → processing → outbound" lifecycle.

---

## System Architecture Comparison

### 1. DaemonClaw (Rust) — Our System

**Flow:**
```
Telegram webhook → mpsc channel → run_message_dispatch_loop()
  → semaphore acquire → dispatch_worker()
    → process_channel_message()
      → memory recall → context compression → typing indicator (async)
      → run_tool_call_loop() [blocking, async — collects ALL tool results]
        → StreamDelta events → delta_tx channel
      → draft_updater task [only when stream_mode != Off]
      → final message send / finalize_draft()
```

**Key characteristics:**
- `run_tool_call_loop()` (loop_.rs, ~8000 lines) is a single `async` function that drives the entire tool call cycle
- Stream deltas (`StreamDelta::Status` / `StreamDelta::Text`) are emitted during inference
- Draft updates (`send_draft` → `update_draft` → `editMessageText`) are **only active when `stream_mode != Off`**
- Default stream mode: **`Off`** — this means NO progressive feedback at all
- Typing indicator (`send_chat_action: typing`) is spawned as a separate task but only when NOT in partial draft mode
- Ack reaction (👁) is sent immediately on receipt — this is the only real-time signal

**Where it breaks down:**
- `stream_mode = "off"` (current config) → draft_updater is None → user sees nothing until completion
- Error paths in `process_channel_message()` have `tracing::warn!()` logging but **no channel reply** to the user for many failure modes
- The typing indicator task gets cancelled when the LLM starts, and there's no "I'm running a tool" indicator
- Tool execution results are sent via `ChannelNotifyObserver` → thread messages, but only when `show_tool_calls = true` in config

**Error handling gaps:**
```rust
// After run_tool_call_loop, errors are logged but not always sent to channel:
match llm_result {
    Ok(Ok(Ok(text))) => { /* normal delivery */ }
    Ok(Ok(Err(e))) => {
        // Provider error — sometimes replied, sometimes just logged
        tracing::warn!("LLM error: {e}");
        // Only sends to channel if target_channel exists AND error isn't cancellation
    }
    Ok(Err(_)) => {
        // Timeout — sends timeout message
    }
    Err(_) => {
        // Cancellation — silently returns
    }
}
```

### 2. OpenClaw (TypeScript) — The Parent Project

**Flow:**
```
Channel plugin (e.g. Telegram) → inbound event queue → auto-reply/dispatch.ts
  → getReply() → agent-runner.ts
    → block-reply-pipeline.ts OR streaming pipeline
    → Telegram-specific: draft-stream.ts (progressive editMessageText)
```

**Key characteristics:**
- **`draft-stream.ts`** (Telegram-specific): Full-featured progressive streaming
  - `sendMessage()` → `editMessageText()` with throttled updates (default 1000ms)
  - Handles 4096-char Telegram limit with auto-chunking (sends completed chunk, starts new)
  - `minInitialChars` debounce — won't send until enough content to avoid push notification spam
  - Generation tracking — handles superseded drafts from concurrent updates
  - HTML parse mode support for markdown rendering
- **`block-streaming.ts`**: Coalesces streaming blocks into meaningful chunks before delivery
- **`typing.ts` / `typing-persistence.ts`**: Typing indicator management with persistence across restarts
- **`agent-runner.ts`**: Has timing tracker that logs milestones for debugging slow turns
- **`channel-notify-observer`**: Tool events sent as live thread messages (similar to our `ChannelNotifyObserver`)
- Error replies are structured: `buildKnownAgentRunFailureReplyPayload()` ensures users always get a message

**Key advantage:** OpenClaw's Telegram extension has a dedicated `draft-stream.ts` that handles all edge cases of progressive delivery — rate limiting, chunk overflow, generation conflicts, HTML rendering.

### 3. Hermes Agent (Python)

**Flow:**
```
Gateway inbound → cron/scheduler or direct call → run_conversation()
  → Blocking loop: API call → tool execution → API call → ... → completion
  → stream_callback (optional, for TTS pipeline only)
  → Gateway delivers final response
```

**Key characteristics:**
- `run_conversation()` (4350 lines) is a **synchronous-looking** function that blocks until completion
- `stream_callback` parameter exists but is ONLY used for TTS pipeline (start audio generation before full response)
- No progressive streaming to channel — the entire response is accumulated then delivered
- `KawaiiSpinner` (terminal only) provides visual feedback for CLI use
- `_emit_status()` callback sends status updates — but only to the gateway WebSocket, not to Telegram directly
- **Error handling is better**: `FailoverReason` classification and `_classify_api_error()` map errors to user-facing messages
- Has model fallback with user notification: "Switching to fallback model due to..."

**Where it differs:** Hermes has the most robust error classification and fallback system, but still suffers the same "no intermediate feedback" issue for channel users. The `stream_callback` is a missed opportunity — it could be wired to Telegram editMessageText but isn't.

### 4. Claude Code (Leaked Source — TypeScript)

**Flow:**
```
UI input → Task.ts (task management) → StreamingToolExecutor.ts → API stream
  → AssistantEvents (Thinking, TextDelta, ToolUse, ToolResult)
  → Built incrementally in UI via event stream
```

**Key characteristics:**
- Pure streaming architecture — `AssistantEvent` enum: `Thinking`, `TextDelta`, `ToolUse`
- Events are consumed incrementally by the UI — no "wait until done" anywhere
- Tool execution is async but the event stream stays open
- **No channel integration** — Claude Code is a CLI/desktop app, not a multi-channel agent
- Error handling is minimal in the event model — errors abort the stream and the UI shows the error state

**Relevance:** Claude Code's streaming model is ideal but it's a single-user desktop app. The patterns (event enum, incremental delivery) are applicable but the channel delivery problem doesn't exist here.

### 5. Claw-Code (Rust) — Claude Code Fork

**Flow:**
```
CLI input → ConversationRuntime::run_turn() → api_client.stream()
  → Vec<AssistantEvent> (Thinking, TextDelta, ToolUse)
  → Tool execution loop (blocking per iteration)
  → Hooks (PreToolUse, PostToolUse) with progress reporter
```

**Key characteristics:**
- `AssistantEvent` enum mirrors Claude Code: `Thinking`, `TextDelta`, `ToolUse`
- `run_turn()` collects ALL events into a `Vec<AssistantEvent>`, then processes — **NOT truly streaming**
- `HookProgressReporter` trait allows hooks to receive progress, but not wired to any channel
- Auto-compaction when context exceeds threshold
- Pure CLI focus — no channel/streaming-to-Telegram concerns

**Relevance:** Claw-Code is DaemonClaw's closest architectural sibling (both Rust), but it has no channel delivery layer. The conversation loop is simpler but has the same "collect all events, then deliver" pattern.

---

## Comparison Matrix

| Feature | DaemonClaw | OpenClaw | Hermes | Claude Code | Claw-Code |
|---------|-----------|----------|--------|-------------|-----------|
| **Language** | Rust | TypeScript | Python | TypeScript | Rust |
| **Streaming to channel** | ✅ (draft edits) | ✅ (draft-stream.ts) | ❌ | N/A (CLI) | ❌ |
| **Progressive text updates** | Off by default | On by default | ❌ | ✅ (UI events) | ❌ |
| **Typing indicator** | ✅ (scoped task) | ✅ (persistent) | ❌ | N/A | ❌ |
| **Tool execution visibility** | Thread messages (opt-in) | Thread messages | ❌ | ✅ (UI) | ❌ |
| **Error → channel reply** | Partial | ✅ (structured) | ✅ (classified) | ✅ (UI) | ❌ |
| **Model fallback + notify** | ✅ (scoped) | ✅ | ✅ (with message) | ❌ | ❌ |
| **Draft chunking (>4096)** | Truncate | ✅ (new message) | N/A | N/A | N/A |
| **Cancellation support** | ✅ (token) | ✅ | ✅ | ✅ (abort) | ✅ (hook) |
| **In-flight tracking** | ✅ (per sender) | ✅ (run registry) | ❌ | N/A | ❌ |
| **Debounce rapid messages** | ✅ (debouncer) | ✅ (inbound-debounce) | ❌ | N/A | ❌ |

---

## Root Cause Analysis for Richard's Issues

### Issue 1: "No response until done"

**Direct cause:** `stream_mode = "off"` in `/etc/daemonclaw/config.toml`

When `stream_mode` is `Off`:
1. `supports_draft_updates()` returns `false`
2. No `delta_tx/delta_rx` channel is created
3. No `draft_message_id` is sent
4. No `draft_updater` task is spawned
5. `StreamDelta` events from `run_tool_call_loop()` are dropped
6. The typing indicator IS spawned (since `is_partial_draft = false`)
7. But it gets cancelled when the full response is ready

**Fix:** Set `stream_mode = "partial"` in config.toml. This enables:
- Initial `sendMessage` with "..."
- Progressive `editMessageText` updates every 1000ms
- Status updates during tool execution

**However**, there's a gap: during tool execution (between API call returning tool_use and the next API call), the draft shows the tool call text but there's no "running tool X..." indicator. OpenClaw handles this with `native-tool-progress-draft.ts` which sends intermediate progress messages in the thread.

### Issue 2: "No response on error"

**Direct cause:** Several error paths in `process_channel_message()` only log but don't reply to channel.

Specific gaps:
1. **Provider initialization failure** → ✅ sends error to channel
2. **LLM call timeout** → ✅ sends timeout message
3. **LLM call provider error** → ⚠️ sometimes sends, sometimes silently logs
4. **Tool execution error** → ❌ tool errors are logged via observer but may not reach channel
5. **Cancellation** → ❌ silently returns, no "cancelled" message sent

OpenClaw solves this with `buildKnownAgentRunFailureReplyPayload()` which guarantees a user-facing error message for every failure mode.

### Issue 3: "No intermediate feedback"

Even with `stream_mode = "partial"`, there's a UX gap:
- Draft message shows accumulating text from the LLM stream
- But tool calls show as `{"name":"shell","arguments":...}` JSON in the draft
- No "I'm looking into that..." or "Running build..." interim messages

OpenClaw solves this by:
1. Stripping tool call JSON from draft display
2. Sending separate thread messages for tool progress
3. `native-tool-progress-draft.ts` for structured tool status in the draft itself

---

## Recommended Fixes (Priority Order)

### P0: Enable streaming (immediate)
```toml
[channels.telegram]
stream_mode = "partial"        # Change from "off"
draft_update_interval_ms = 1500  # Slightly slower to reduce rate limit risk
```

### P1: Error → channel reply for ALL failure modes
Add a catch-all at the end of `process_channel_message()`:
```rust
// After the match on llm_result, add:
Err(e) => {
    let msg = format!("⚠️ Request failed: {}", sanitize_api_error(&e.to_string()));
    if let Some(ch) = target_channel.as_ref() {
        let _ = ch.send(&SendMessage::new(&msg, &msg.reply_target)).await;
    }
}
```

### P2: Tool execution visibility
- Strip tool call JSON from draft display (already partially done via `strip_think_tags_inline`)
- Send structured tool progress in thread messages (wire `ChannelNotifyObserver` to format tool names nicely)

### P3: Cancellation acknowledgment
When `cancellation_token.is_cancelled()` fires, send a "⏹ Cancelled" message before returning.

### P4: Streaming polish (match OpenClaw quality)
- Draft chunk overflow → send completed chunk + start new message (OpenClaw does this)
- HTML parse mode for Telegram (OpenClaw renders markdown)
- `minInitialChars` debounce (avoid push notification spam for short responses)
