# History management

The runtime keeps conversation history for each agent session and sends a
provider-facing working history to the model. Two complementary limits operate
on different representations:

1. **Token-budget trimming** acts on the provider-facing `ChatMessage` working
   history and drops oldest whole turns until the estimated context fits the
   token budget.
2. **Structured message-count trimming** mutates `Agent::history`
   (`ConversationMessage`) used by RPC, gateway, and ACP `Agent` turns when it
   exceeds the structured agent's effective message cap. Daemon channel loops
   that call the legacy `agent::run` path use the separate raw-message cap
   described below.

Token-budget trimming and the structured message-count limit retain turns
atomically. A turn starts at a real user message and includes the assistant
response and any tool calls and tool results before the next user message.
Trimming therefore does not split a tool call from its result.

## Whole-turn retention

`history_trim::trim_to_recent_turns` enforces the token budget, while
`history_trim::trim_conversation_to_recent_turns` enforces the structured
message-count limit. Each keeps the newest complete turn even when that turn by
itself exceeds the relevant limit. This is intentional: preserving a complete
current turn is safer than satisfying a numeric cap by dropping its newest
messages or breaking a tool exchange.

Leading system messages are retained. When no trim is needed, message order and
shape are left unchanged.

## Token budget

The token budget comes from `ResolvedRuntime::effective_context_budget()`:

- Existing profiles retain the historical 32,000-token budget when neither
  `max_context_tokens` nor `context_compact_ratio` is set, capped by the
  selected model's configured capacity when that capacity is smaller.
- `max_context_tokens` remains an absolute budget. An explicit value of `0`
  disables proactive token-budget trimming.
- Setting `context_compact_ratio` opts into a model-relative budget: the
  selected provider alias/model's `context_window` (or a 32,000 fallback when
  its capacity is unknown) multiplied by the ratio. Values outside `(0.0, 1.0]`
  are treated as unset. When both settings are present, `max_context_tokens`
  is a downward cap on the ratio-derived budget.
- When `history_pruning.enabled` is set with a positive
  `history_pruning.max_tokens`, that value pulls the budget down (never up),
  letting operators trim earlier.
- Every positive effective budget is capped by the selected model's capacity.
  The explicit zero sentinel remains zero and continues to disable proactive
  trimming.

Capacity and budget are resolved together for the active provider/model route.
Classifier hints and explicit session switches use the same route selection as
provider dispatch, so the next model call, proactive trim, overflow diagnostic,
cost attribution, and client context meter use the selected provider/model and
its resolved pair. If the selected model does not match the model configured on
that provider profile, capacity is treated as unknown instead of borrowing the
profile's metadata for a different model. The internal 32,000 compatibility
fallback remains available for safety calculations, but wire clients receive no
`model_context_window` value for an unknown capacity.

Token counts are estimated by `history::estimate_history_tokens`: roughly four
characters per token plus four framing tokens per message. This is a heuristic,
not a provider tokenizer.

Proactive token-budget trimming runs before the first provider call of a turn
when history already exceeds the effective budget and at provider-call
boundaries between tool-loop iterations. If a provider reports a context-window
overflow, the existing reactive recovery path retries after trimming to two
thirds of the current estimated history size. It does not derive a new recovery
target from model capacity. Both paths retain whole turns, so neither splits a
tool exchange.

## Structured message-count limit

`max_history_messages` is the configured value in the agent's runtime profile.
An explicitly configured value is authoritative for both the legacy raw path
and structured agent history, including `0`. Because structured trimming always
retains the newest whole turn, a value of `0` removes older turns but does not
erase the current turn.

When `max_history_messages` is omitted, the legacy raw cap remains `50`. The
structured agent's effective cap is derived from the tool-loop allowance:

```text
max(50, 2 * max_tool_iterations + 2)
```

Each tool iteration can add a tool call and a tool result; the extra two slots
cover the user message and final assistant response. With the default
`max_tool_iterations = 10`, the derived limit remains `50`.

## Visible trimming

Whenever token-budget trimming or the structured message-count limit drops
older turns, the runtime:

1. Inserts a breadcrumb before the first retained turn so the model knows that
   earlier context was omitted.
2. Emits `HistoryTrimmed` with the number of dropped messages, retained turns,
   and a reason identifying the token budget or message limit.

The event is surfaced through the active client transport and through the
observer path used by dashboards and event subscribers. Trimming is therefore
not log-only and is not silent to either the model or connected clients.

The legacy `agent::run` path in `loop_.rs` is an unchanged exception. Its raw
`ChatMessage` cap in `history::trim_history` remains message-level and reports
trimming through logs only, without the breadcrumb or `HistoryTrimmed` event.
This path serves interactive use as well as one-shot and non-interactive daemon,
cron, subagent, and SOP callers.

## Pairing safety

Whole-turn retention is the primary tool-pairing guarantee: a tool call and its
result belong to the same turn and are retained or dropped together. The orphan
sweep remains a final safety net for histories that were already inconsistent,
such as restored or externally modified sessions.

Tool-result length limits are separate. `max_tool_result_chars` bounds an
individual result when it is recorded; it does not trim conversation history.
Provider-side context enforcement is also separate, though a provider overflow
can trigger the runtime's reactive token-budget trim.
