# Conversation Recovery: Implementation Spec

## Problem Statement

When `run_tool_call_loop` encounters a recoverable error (e.g., the model narrates
tool use instead of emitting tool calls), the system hard-fails after a single
in-loop retry. The error propagates to the channel handler at `channels/mod.rs:4158`,
which appends `[Task failed — not continuing this request]` to history and sends
`"⚠️ Error: {e}"` to the user. The conversation is dead — the user must manually
re-engage.

This is the equivalent of an HTTP client that makes one request, gets a 500, and
gives up. The fix is bounded retry with escalating strategies at the call site,
not deeper in the loop.

---

## Architecture Overview

Three changes, each independent and incrementally shippable:

1. **Error classification** — categorize agent loop errors as recoverable, transient, or fatal
2. **Retry-around-loop** — wrap the `run_tool_call_loop` call site with bounded retry + escalation
3. **Stale turn probe** — extend the heartbeat engine to detect and nudge stalled conversations

```
                    User message arrives (Signal)
                              │
                              ▼
                   ┌──────────────────────┐
                   │  Channel Handler     │
                   │  channels/mod.rs     │
                   │  :3808-4210          │
                   └──────────┬───────────┘
                              │
              ┌───────────────▼───────────────┐
              │  NEW: Recovery Wrapper         │
              │  Attempt 1: normal invocation  │
              │  Attempt 2: recovery prompt    │
              │  Attempt 3: compressed context │
              └───────────────┬───────────────┘
                              │
                   ┌──────────▼──────────┐
                   │  run_tool_call_loop  │
                   │  loop_.rs:1046       │
                   │  (unchanged)         │
                   └─────────────────────┘
```

---

## Component 1: Error Classification

### File: `src/agent/recovery.rs` (new)

```rust
/// Whether an agent loop error can be retried with a different strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorRecoverability {
    /// Model protocol failure — retry with escalating prompt strategy.
    /// Examples: deferred action, tool-call parse failure.
    Recoverable,

    /// Infrastructure hiccup — retry same request after backoff.
    /// Examples: provider 503, rate limit, network timeout.
    Transient,

    /// Hard stop — do not retry.
    /// Examples: cost limit, estop, security violation, loop detection hard-stop.
    Fatal,
}

pub fn classify_error(err: &anyhow::Error) -> ErrorRecoverability {
    let text = err.to_string();

    // Fatal: never retry these
    if is_cost_limit_error(err)
        || is_estop_error(err)
        || is_tool_loop_cancelled(err)
        || text.contains("loop detection hard-stop")
        || text.contains("security policy")
    {
        return ErrorRecoverability::Fatal;
    }

    // Recoverable: model behavior issues
    if text.contains("deferred action without emitting a tool call")
        || text.contains("tool-call parse")
    {
        return ErrorRecoverability::Recoverable;
    }

    // Transient: infrastructure / provider issues
    if is_context_window_overflow_error(err)
        || text.contains("503")
        || text.contains("rate limit")
        || text.contains("timed out")
    {
        return ErrorRecoverability::Transient;
    }

    // Default: don't retry unknown errors
    ErrorRecoverability::Fatal
}
```

### Why this matters

The channel handler at `mod.rs:4050-4210` already branches on error type
(`is_context_window_overflow_error`, `is_tool_iteration_limit_error`, etc.).
This formalizes that pattern into a single classifier that the retry wrapper
can use.

---

## Component 2: Retry-Around-Loop

### Where: `channels/mod.rs`, wrapping the call at lines 3808-3837

The existing code calls `run_tool_call_loop` once and matches on the result.
The change wraps this in a retry loop with escalating strategies.

### Strategy Escalation

```
┌─────────────────────────────────────────────────────────┐
│ Attempt 1: Normal invocation                            │
│   - Standard system prompt, full history                │
│   - run_tool_call_loop runs (includes its own 1 retry)  │
│   - If Err + Recoverable → continue to attempt 2        │
├─────────────────────────────────────────────────────────┤
│ Attempt 2: Recovery prompt injection                    │
│   - Backoff: recovery_backoff_base_ms (default 3s)      │
│   - Strip the failed assistant messages from history    │
│   - Inject recovery system nudge (see below)            │
│   - Optionally lower temperature                        │
│   - If Err + Recoverable → continue to attempt 3        │
├─────────────────────────────────────────────────────────┤
│ Attempt 3: Context compression + re-statement           │
│   - Backoff: recovery_backoff_base_ms * multiplier      │
│   - Compact history (reuse existing compact_sender_     │
│     history logic from mod.rs:4079)                     │
│   - Re-inject original user message explicitly          │
│   - Stronger system nudge                               │
│   - If Err → fall through to existing error handler     │
└─────────────────────────────────────────────────────────┘
```

### Recovery Prompts

**Attempt 2 system nudge** (injected as final user message before re-invocation):

```
RECOVERY: Your previous attempt described performing actions in natural language
without actually calling any tools. This is not acceptable — the user received
no result.

Rules for this attempt:
1. If you need to use a tool, you MUST emit a <tool_call> block. Describing
   what you would do is not the same as doing it.
2. If you have enough information to answer without tools, provide the final
   answer directly. Do not reference actions you did not actually perform.
3. Do not apologize or explain the retry. Just do the work.
```

**Attempt 3 system nudge** (stronger, with context re-statement):

```
RECOVERY (final attempt): Previous attempts failed to produce valid tool calls.
The conversation history has been compacted.

The user's original request was:
---
{original_user_message}
---

Respond to this request now. Use tools if needed (emit <tool_call> blocks),
or provide a direct answer. This is your last attempt.
```

### History Management

Critical: `run_tool_call_loop` takes `history: &mut Vec<ChatMessage>`. Failed
attempts mutate history with garbage (hallucinated assistant messages, retry
prompts). The retry wrapper must **snapshot and restore** history between
attempts.

```rust
// Before each attempt:
let history_snapshot = history.clone();

// After a recoverable failure:
*history = history_snapshot;  // restore clean state
// Then inject recovery prompt and retry
```

This is the key architectural insight — without history rollback, each retry
attempt inherits the broken context from the previous failure, making success
less likely with each attempt.

### Pseudocode

```rust
// In channels/mod.rs, replacing the single invocation at :3808-3837

let max_recovery_attempts = ctx.config.recovery.max_recovery_attempts; // default: 2
let mut recovery_attempt = 0;

let llm_result = loop {
    let history_snapshot = history.clone();

    let attempt_result = tokio::select! {
        () = cancellation_token.cancelled() => LlmExecutionResult::Cancelled,
        result = tokio::time::timeout(
            Duration::from_secs(timeout_budget_secs),
            /* ... existing run_tool_call_loop invocation ... */
        ) => LlmExecutionResult::Completed(result),
    };

    match &attempt_result {
        LlmExecutionResult::Completed(Ok(Err(e)))
            if recovery_attempt < max_recovery_attempts
                && classify_error(e) == ErrorRecoverability::Recoverable =>
        {
            recovery_attempt += 1;

            // Restore history to pre-failure state
            history = history_snapshot;

            // Backoff
            let backoff_ms = ctx.config.recovery.backoff_base_ms
                * ctx.config.recovery.backoff_multiplier.powi(recovery_attempt as i32);
            tokio::time::sleep(Duration::from_millis(backoff_ms as u64)).await;

            // Inject recovery prompt
            let nudge = if recovery_attempt == 1 {
                RECOVERY_PROMPT_ATTEMPT_2.to_string()
            } else {
                format!(RECOVERY_PROMPT_ATTEMPT_3, original_user_message = &user_content)
            };
            history.push(ChatMessage::user(nudge));

            // Optionally compress on final attempt
            if recovery_attempt >= 2 {
                compact_sender_history(ctx.as_ref(), &history_key);
            }

            runtime_trace::record_event(
                "conversation_recovery_attempt",
                Some(msg.channel.as_str()),
                Some(route.provider.as_str()),
                Some(route.model.as_str()),
                None,
                None,
                Some(&format!("recovery attempt {recovery_attempt}/{max_recovery_attempts}")),
                serde_json::json!({
                    "attempt": recovery_attempt,
                    "backoff_ms": backoff_ms,
                    "error": scrub_credentials(&e.to_string()),
                }),
            );

            continue; // retry
        }
        _ => break attempt_result, // success, fatal error, or out of retries
    }
};

// ... existing match on llm_result continues unchanged ...
```

### What Changes in the Error Handler

The existing error branch at `mod.rs:4158-4209` (the generic `else` case) is
where deferred-action errors currently land. After this change:

- Recoverable errors are retried before reaching this branch
- If all retries are exhausted, the error still reaches this branch
- The error message sent to the user should be improved:

```rust
// Instead of: "⚠️ Error: Model deferred action without emitting..."
// Send:       "⚠️ I tried multiple approaches but couldn't complete
//              that action. Here's what I was working on: {summary}.
//              You can ask me to try again with a different approach."
```

- The `[Task failed — not continuing this request]` history marker at :4193
  should be softened when recovery was attempted:
  `[Task paused after recovery attempts — ask to retry or try a different approach]`

---

## Component 3: Stale Turn Probe

### Problem

Some conversations stall silently — the model produces a plausible-looking
response that implies follow-up ("I'll check that and get back to you") but
no follow-up ever happens. The deferred-action guard catches some of these,
but not all (e.g., when the model produces a complete-looking answer that
happens to be fabricated).

### Approach: Extend HeartbeatEngine

The heartbeat engine (`heartbeat/engine.rs`) already ticks periodically and
reads HEARTBEAT.md. Extend it to also check for stale conversation turns.

### New State: Pending Follow-up Tracker

```rust
// New file: src/heartbeat/pending_followups.rs

/// A record of a conversation turn that implied follow-up work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingFollowup {
    pub channel: String,
    pub reply_target: String,
    pub history_key: String,
    pub created_at: SystemTime,
    pub ttl_secs: u64,
    pub context_summary: String,  // what the agent was working on
    pub nudge_count: u32,         // how many times we've nudged
    pub max_nudges: u32,          // default: 2
}

/// Stored as JSON in workspace_dir/pending_followups.json
pub struct PendingFollowupStore { ... }
```

### When to Create a Pending Follow-up

After a successful `run_tool_call_loop` response, check if the response implies
unfinished work:

```rust
// In channels/mod.rs, after line 4006 (successful response handling)

if response_implies_followup(&delivered_response) {
    pending_followups.add(PendingFollowup {
        channel: msg.channel.clone(),
        reply_target: msg.reply_target.clone(),
        history_key: history_key.clone(),
        created_at: SystemTime::now(),
        ttl_secs: ctx.config.recovery.stale_followup_timeout_secs, // default: 300
        context_summary: truncate_with_ellipsis(&delivered_response, 200),
        nudge_count: 0,
        max_nudges: 2,
    });
}
```

The `response_implies_followup` function uses a similar regex approach to the
existing deferred-action detector — looks for phrases like "I'll get back to
you", "let me check on that", "working on it", etc. But this runs on
*successful* responses, not failures.

### When to Clear a Pending Follow-up

- When the same `history_key` (sender) sends a new message (conversation continued)
- When the same `history_key` gets a successful response (follow-up completed)
- When `nudge_count >= max_nudges` (give up gracefully)
- When `ttl_secs` has elapsed AND nudge was attempted

### HeartbeatEngine Extension

```rust
// In heartbeat/engine.rs, extend tick():

async fn tick(&self) -> Result<usize> {
    let heartbeat_tasks = self.collect_tasks().await?.len();

    // NEW: check for stale follow-ups
    let stale = self.followup_store.collect_expired().await?;
    for followup in stale {
        if followup.nudge_count >= followup.max_nudges {
            self.followup_store.remove(&followup.history_key).await?;
            continue;
        }

        // Send a nudge message to the channel
        let nudge_msg = format!(
            "Checking in — I was working on something for you but it may have stalled. \
             Context: {}. Want me to continue?",
            followup.context_summary
        );

        self.channel_sender.send(ChannelOutbound {
            channel: followup.channel.clone(),
            reply_target: followup.reply_target.clone(),
            content: nudge_msg,
        }).await?;

        self.followup_store.increment_nudge(&followup.history_key).await?;
    }

    Ok(heartbeat_tasks + stale.len())
}
```

### Why This is Separate from Component 2

Component 2 handles **immediate failures** — the model broke protocol and we
retry right away. Component 3 handles **delayed failures** — the model appeared
to succeed but the conversation is actually stalled. Different detection
mechanism, different recovery strategy, different timescale.

---

## Configuration

```toml
[recovery]
enabled = true

# Component 2: retry-around-loop
max_recovery_attempts = 2           # attempts around the loop (on top of the 1 inside)
backoff_base_ms = 3000              # initial backoff before retry
backoff_multiplier = 3.0            # exponential factor (3s, 9s)
recovery_temperature = 0.3          # lower temperature for recovery attempts (0 = use default)
compress_on_final_attempt = true    # compact history before last attempt

# Component 3: stale turn probe
stale_followup_enabled = true
stale_followup_timeout_secs = 300   # 5 min before heartbeat checks
stale_followup_max_nudges = 2       # max nudge messages before giving up
```

### Schema Addition: `src/config/schema.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RecoveryConfig {
    #[serde(default = "default_recovery_enabled")]
    pub enabled: bool,

    #[serde(default = "default_max_recovery_attempts")]
    pub max_recovery_attempts: u32,

    #[serde(default = "default_recovery_backoff_base_ms")]
    pub backoff_base_ms: u64,

    #[serde(default = "default_recovery_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Temperature override for recovery attempts. 0 = use default.
    #[serde(default)]
    pub recovery_temperature: f64,

    #[serde(default = "default_compress_on_final")]
    pub compress_on_final_attempt: bool,

    #[serde(default = "default_stale_followup_enabled")]
    pub stale_followup_enabled: bool,

    #[serde(default = "default_stale_followup_timeout")]
    pub stale_followup_timeout_secs: u64,

    #[serde(default = "default_stale_followup_max_nudges")]
    pub stale_followup_max_nudges: u32,
}
```

---

## Observability

Every recovery action emits a runtime trace event:

| Event | When | Payload |
|---|---|---|
| `conversation_recovery_attempt` | Before each retry | attempt number, backoff, error |
| `conversation_recovery_success` | Retry succeeded | attempt that worked, total time |
| `conversation_recovery_exhausted` | All retries failed | all attempts, final error |
| `stale_followup_detected` | Heartbeat finds expired | history_key, age, context |
| `stale_followup_nudge_sent` | Nudge delivered | channel, nudge_count |
| `stale_followup_cleared` | Follow-up resolved | reason (user_replied, max_nudges, completed) |

---

## What Does NOT Change

- **`run_tool_call_loop` internals** — the in-loop single retry stays as-is.
  It handles the fast/easy case (model self-corrects on one nudge). The
  recovery wrapper handles the case where it doesn't.

- **Deferred-action regex** — detection logic is correct and working.

- **Loop detection** (no-progress, ping-pong, failure streak) — independent
  failure mode, separate recovery path. These remain fatal.

- **Security/cost guards** — classified as Fatal, never retried.

- **Channel message routing** — unchanged. Recovery is transparent to the
  channel layer.

---

## Implementation Order

1. **Error classification** (`recovery.rs`) — pure function, no dependencies,
   easy to test. Refactors existing ad-hoc string matching in mod.rs.

2. **Retry-around-loop** (modify `channels/mod.rs`) — biggest impact, directly
   addresses the McBarge incident. Requires history snapshot/restore.

3. **Stale turn probe** (extend `heartbeat/engine.rs`) — independent from 1
   and 2. Addresses the long-tail failure mode.

---

## Testing Strategy

### Unit Tests

- `classify_error` returns correct category for each known error type
- History snapshot/restore produces identical history after rollback
- Recovery prompts are injected correctly at each attempt level
- `response_implies_followup` regex matches/rejects correctly
- Pending follow-up store CRUD operations
- Backoff timing calculations

### Integration Tests

- Simulate deferred-action error → verify retry fires → verify history is clean
- Simulate 3 consecutive deferred-action errors → verify all retries exhausted
  → verify user-facing error message is improved
- Simulate fatal error → verify no retry attempted
- Simulate transient error → verify backoff timing
- Heartbeat tick with expired follow-up → verify nudge sent
- Heartbeat tick with max-nudged follow-up → verify removal

### Manual Validation

Reproduce the McBarge scenario:
1. Send a research query that requires multiple tool calls
2. Artificially constrain `max_tokens` on the provider to force truncation
3. Verify recovery fires, history is restored, and the second attempt succeeds
4. Verify observability events are emitted at each stage

---

## Risk Assessment

| Risk | Mitigation |
|---|---|
| Recovery attempts burn extra tokens/cost | Bounded to 2 extra attempts. Cost tracking applies across retries (same `cost_enforcement_context`). |
| History snapshot doubles memory briefly | History is already in memory; clone is O(n) but bounded by context window. Freed immediately after attempt. |
| Recovery prompt confuses the model further | Prompts are directive and explicit. Lower temperature reduces creativity. Final attempt compresses context. |
| Stale follow-up nudge annoys user | Max 2 nudges, 5-min minimum delay, cleared on any user reply. Can be disabled. |
| Retry delays user response | Backoff is 3s + 9s = 12s max. Current failure mode (hard error) requires manual re-engagement, which is much slower. |

---

## Appendix: The McBarge Incident

Timeline reconstruction showing where each component would have intervened:

```
06:30:04  LLM returns 257 tokens, no tool calls, deferred action detected
          ├─ EXISTING: in-loop retry prompt injected
          │
06:30:30  LLM returns 200 tokens, no tool calls, deferred action again
          ├─ EXISTING: anyhow::bail!()
          │
          ├─ NEW (Component 1): classify_error → Recoverable
          │
          ├─ NEW (Component 2): history restored to pre-06:30:04 state
          │                      recovery prompt injected
          │                      3s backoff
          │                      run_tool_call_loop re-invoked
          │                      (model gets clean context + explicit instructions)
          │
          ├─ If attempt 2 also fails:
          │   ├─ history restored again
          │   ├─ context compressed
          │   ├─ original user request re-stated
          │   ├─ 9s backoff
          │   ├─ final attempt
          │   │
          │   └─ If still fails:
          │       └─ "I tried multiple approaches but couldn't complete that
          │           action. Here's what I was working on: researching the
          │           McBarge. You can ask me to try again."
          │
          └─ If attempt 2 succeeds:
              └─ Normal response delivered. User never knows recovery happened.
```

With the 200-token truncation as a likely contributing factor, the clean
history + recovery prompt on attempt 2 would likely have succeeded — the
model would start fresh without the confused context of the failed iterations.
