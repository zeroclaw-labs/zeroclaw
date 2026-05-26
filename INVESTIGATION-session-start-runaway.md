# BUG: Agent goes haywire mid-session

## Symptom
Mid-session, the agent starts running autonomously — investigating code, reading files, running git operations — as if responding to directives that aren't there. The conversation history appears corrupted from the model's perspective.

## Root Cause: Orphan AssistantToolCalls in `trim_history`

**File:** `crates/zeroclaw-runtime/src/agent/agent.rs`, line 1079

`trim_history` drops messages from the front of history when it exceeds `max_history_messages` (default: 50). It correctly handles **orphan ToolResults** — if trimming would leave a ToolResults at the head without its paired AssistantToolCalls, it drops the ToolResults too (lines 1100-1114).

**BUT it does NOT handle orphan AssistantToolCalls.** If trimming drops the ToolResults from the tail of the trimmed region but keeps the AssistantToolCalls that requested them, the model sees:

```
assistant: [tool_calls: shell("ls"), file_read("foo.rs")]   ← ORPHAN — results were trimmed
user: next message
assistant: ...
```

The model sees tool calls it made but never received results for. This corrupts the conversation from the model's perspective and causes unpredictable behavior — the model may retry the tools, hallucinate results, or go off the rails entirely.

### Why this triggers mid-session

`trim_history` is called after EVERY tool result push during the tool loop (line 2084 in agent.rs). With `max_history_messages = 50` and ~3 messages per tool iteration (AssistantToolCalls + ToolResults + assistant text), the trim fires constantly during long tool loops. Each trim can create a new orphan AssistantToolCalls at the boundary.

### The trim code (agent.rs:1079-1121)

```rust
fn trim_history(&mut self) {
    let max = self.config.max_history_messages;
    if self.history.len() <= max {
        return;
    }

    // Separates system messages, trims non-system from front
    // ...

    // Handles orphan ToolResults at head ✅
    while drop_count < other_messages.len()
        && matches!(
            &other_messages[drop_count],
            ConversationMessage::ToolResults(_)
        )
    {
        drop_count += 1;
    }

    // ❌ NO CHECK for orphan AssistantToolCalls
    // If the LAST message kept before the trim boundary is an
    // AssistantToolCalls, its paired ToolResults just got dropped.
    // The model now sees tool calls with no results.

    other_messages.drain(0..drop_count);
}
```

### The fix

After the orphan-ToolResults check, add a symmetric check: if the last message in the kept range is an AssistantToolCalls (meaning its ToolResults were in the dropped region), drop it too.

## Supporting Evidence

- `trim_history` fires at line 2084 during tool loop — every single iteration
- `max_history_messages` defaults to 50 — easily exceeded in tool-heavy sessions
- `max_tool_iterations = 1000` — the agent can loop for a very long time
- Context compression does NOT run in the RPC/TUI path — ruled out
- `strip_old_tool_context` in orchestrator is a separate mechanism (channel path only)
- The orphan-ToolResults guard (lines 1100-1114) proves the developers knew about pairing issues — the symmetric case was just missed

## Config Context (clamps agent)

```toml
[agents.clamps]
compact_context = true
keep_tool_context_turns = 2
model_provider = "anthropic.clamps"  # claude-opus-4-6

[runtime_profiles.clamps]
max_tool_iterations = 1000
max_context_tokens = 128000
max_history_messages = 50  # (default)
```

## Status
**ROOT CAUSE IDENTIFIED.** Fix needed in `trim_history` to handle orphan AssistantToolCalls symmetrically with orphan ToolResults.

History grows past `max_history_messages` → `trim_history` fires → the drop boundary lands between an AC/TR pair → orphan AC sits at position 0 of the surviving history → the model sees tool calls it supposedly made but never got results for.

What happens next is model-dependent but all bad:

- **Anthropic** might reject outright (like it does with orphan TRs — "unexpected tool_use_id")
- **More likely** — the model gets confused and starts hallucinating tool results, re-invoking tools it already ran, or just goes off the rails mid-conversation. The "bonk on the head" behavior. One minute it's fine, next turn it's acting drunk.

The trigger is **session length**. Short sessions never hit `max_history_messages`, so the trim never fires, so the bug never manifests. It's only after enough back-and-forth that the history overflows and the trim boundary happens to land on an AC/TR pair boundary. That's why it'd look like a random mid-session bonk — there's no user action that causes it, just the history crossing a threshold at an unlucky alignment.

The TR guard was already working (that pattern was correct). But the AC guard was dead — `(_)` never matches a struct variant, so the `while` loop body never executed.

The trim drops the N oldest non-system messages. If message N+1 (the new head) happens to be an `AssistantToolCalls`, you get the bonk. If it happens to be a `Chat` or `ToolResults` (the TR guard already worked), you don't.

So it's a function of:

- **History length** — how many messages are in the buffer when trim fires
- **`max_history_messages`** — the configured ceiling
- **The pattern of turns** — a user message followed by a simple text reply is 2 messages. A user message followed by 3 tool calls is 8 messages (user, AC, TR, AC, TR, AC, TR, assistant). Tool-heavy turns pack more messages per logical exchange.

That means tool-heavy sessions hit the trim sooner *and* have more AC/TR pairs for the boundary to land on. So yeah — it'd *look* like it correlates with specific tool calls, but it's really just that sessions with lots of tool use have higher odds of the trim boundary aligning wrong.

A session that's mostly chat back-and-forth could run for ages and never see it. A session where the agent is hammering shell/file tools every turn would hit it much sooner and more often.
