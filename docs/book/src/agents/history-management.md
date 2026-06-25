# History management

The runtime keeps one conversation history per agent session and sends it to
the model on every turn. Left unbounded that history outgrows the context
window, so the runtime has exactly one mechanism to bound it: **whole-turn
trimming**. This page disambiguates that mechanism from the adjacent operations
people conflate it with, and documents the user-visible signal it emits.

Everything here is sourced from the runtime code in
`crates/zeroclaw-runtime/src/agent/`. Where a behavior is named, the function
that implements it is named with it.

## The one trimmer

`history_trim::trim_to_recent_turns(history, budget_tokens)` is the whole of the
trimming logic. Its rule is deliberately small enough to hold in your head:

> Keep the most recent whole turns that fit the token budget, drop the rest,
> never cut a turn in half.

A **turn** starts at a real user message and runs until the next real user
message, covering the assistant reply, any assistant tool-call rows, and any
tool-result rows in between. Tool exchanges live entirely inside a turn, so
dropping whole turns can never split a `tool_use` from its `tool_result`.
Pairing safety for providers that enforce it (Anthropic among them) is
**structural**, not patched up afterward.

The function returns a `TrimResult` carrying `dropped_turns`, `dropped_messages`,
`kept_turns`, `tokens_before`, `tokens_after`, and `trimmed`. `trimmed` is true
only when at least one whole turn was dropped. Leading system messages are
always preserved, and at least the most recent whole turn is always kept even
if that single turn exceeds the budget: the model never gets nuked to nothing.

## When it runs

Trimming runs at two moments, never mid-tool-loop:

1. **Preemptive**, once at the start of a turn, when the history already exceeds
   the effective budget (`run_tool_call_loop`, iteration 0).
2. **Reactive**, when a provider returns a context-window-exceeded error; the
   recovery path drops oldest whole turns and retries
   (`turn::context_recovery::try_recover_context_overflow` and the interactive
   loop's overflow arm).

Both paths call the same `trim_to_recent_turns`. There is no per-iteration
pruning and no summarization step.

## The budget

The budget comes from `ResolvedRuntime::effective_context_budget()`:

- When `history_pruning.enabled` is set with a positive `history_pruning.max_tokens`,
  the budget is the lower of that floor and `max_context_tokens`, so an explicit
  budget trims earlier than the hard ceiling.
- Otherwise the budget is `max_context_tokens` and the hard ceiling is the only
  trigger.

Token counts are estimated by `history::estimate_history_tokens`: roughly four
characters per token plus four framing tokens per message. It is a heuristic,
not a tokenizer.

> The `history_pruning.*` config keys are reused as-is; `collapse_tool_results`
> and `keep_recent` no longer drive any code path (the most recent whole turn is
> kept structurally). The key idents are scheduled to be renamed at config
> schema V4 and are intentionally left in place until then.

## It is never silent

When `trimmed` is true the caller does two things so the loss is always visible:

1. Injects a breadcrumb into the history, after the leading system messages and
   before the first kept turn:
   `[earlier turns omitted to fit the context window]` (`history_trim::breadcrumb`).
2. Emits a visible "context was trimmed" signal on every client surface, the
   same multi-surface visibility contract that turn cancellation uses:
   - **ACP** (`session/update` of type `history_trimmed`, mapped in
     `acp_server.rs`) carrying `sessionId`, `droppedMessages`, `keptTurns`,
     and `reason` (`SessionUpdateEvent::HistoryTrimmed`).
   - **Gateway WebSocket** (`{"type":"history_trimmed", ...}`, mapped in
     `ws.rs` from `TurnEvent::HistoryTrimmed`).
   - **SSE `/api/events`** (`{"type":"history_trimmed", ...}`, mapped in
     `sse.rs` from `ObserverEvent::HistoryTrimmed`) carrying
     `dropped_messages`, `kept_turns`, `reason`, plus `agent_alias` /
     `channel` / `turn_id` when the attribution span carries them.

   When the context changes underneath the model, the end user is told why.

The breadcrumb matters beyond the UI. With it in context, a model asked to
recall dropped work answers honestly ("the earlier turns were omitted from my
context window") instead of fabricating a result it can no longer see.

## What trimming is not

These are distinct operations. Only the first one drops conversation history.

| Operation | What it does | Where |
|---|---|---|
| **Trimming** | Drops oldest **whole turns** to fit the token budget. The only thing that removes history. | `history_trim::trim_to_recent_turns` |
| **Orphan sweep** | Removes a `tool_result` whose `tool_use` is gone (or vice versa) so providers do not 400 on a dangling pair. A pairing-safety net, not a size control. | `history_pruner::remove_orphaned_tool_messages` |
| **System normalization** | Merges and reorders system messages to the front. Changes shape, never drops turns. | `history::normalize_system_messages` |
| **Tool-result capping** | At collection time, caps a single tool result's length (`max_tool_result_chars`). Bounds one message as it is recorded; does not touch history. | `history::truncate_tool_result` |
| **Provider truncation** | The provider's own context-window enforcement, server-side. Out of the runtime's hands; the reactive path reacts to it. | provider API |

There is no context **compression** or **summarization** step. The runtime does
not replace old turns with a synthetic summary and does not inject placeholder
markers into provider-visible history. If you are looking for that, it was
removed: collapsing turns into summaries is exactly the silent-mutation pattern
that made models report work they could no longer see.

## Pairing safety

The hard invariant is that a request never carries a `tool_use` without its
`tool_result` or vice versa. Two things guarantee it:

1. Whole-turn trimming cannot split a pair, because both halves live inside the
   same turn and turns are dropped atomically.
2. The orphan sweep runs as a final net for histories that arrive already
   broken (reloaded sessions, upstream edits), removing any dangling tool row
   before the request goes out.

A trimmed history therefore passes the orphan sweep with nothing to remove,
which is asserted directly in the `history_trim` unit tests.
