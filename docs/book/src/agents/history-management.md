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

- When `history_pruning.enabled` is set with a positive
  `history_pruning.max_tokens`, the budget is the lower of that value and
  `max_context_tokens`.
- Otherwise the budget is `max_context_tokens`.

Token counts are estimated by `history::estimate_history_tokens`: roughly four
characters per token plus four framing tokens per message. This is a heuristic,
not a provider tokenizer.

Token-budget trimming runs before the first provider call of a turn when
history already exceeds the effective budget and at provider-call boundaries
between tool-loop iterations, including reactively when a provider reports that
the context window was exceeded. It retains whole turns, so it never splits a
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
