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
not a provider tokenizer. Loadable `[IMAGE:...]` markers are charged a fixed
per-image cost only in messages whose images are dispatched: user turns and the
latest tool results. Markers that preparation strips from older tool results
are not charged, and system and assistant text is priced as text.

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

## Manual context compaction

Trimming drops history without describing what was dropped. Manual context compaction is the explicit, user-invoked alternative for native ZeroCode Code sessions (`chat_mode = "acp"`, the `zerocode_code` interaction surface): it summarizes a contiguous prefix of older completed turns into one bounded continuity summary while retaining the original transcript.

The commands are local to the ZeroCode Code pane: `/compact-context` runs one manual compaction, `/restore-context` deactivates the active checkpoint. There is no automatic trigger, no ordinary-Chat or channel rollout, and no external ACP agent exposure. The daemon refuses the operation on a busy session with a typed busy result; it never queues behind a running turn and never cancels one.

### What may be covered

Coverage is certified by durable terminal ranges, not inferred. Every terminal append (turn finalization, failed-turn persistence, interrupted-turn recovery) records, in the same transaction, the span of message rows it settled and how the turn ended: `completed`, `failed`, or `interrupted`. Compaction may cover only:

- a contiguous run of **completed** ranges starting at the session's first message row. Legacy rows with no range, interrupted turns, failed turns, and ambiguous or unpaired tool exchanges are refused with a typed, user-explainable error, never silently certified or repaired into coverage;
- a prefix that **excludes the newest completed turn**, which is always retained together with everything after it.

Row-id adjacency is session-local: `acp_messages` ids are global, so another session's rows can sit numerically between this session's turns without affecting coverage.

### The operation

`/compact-context` runs while the session is idle (fail-fast, non-barging admission) and uses the admitted Agent's existing routed provider and model for exactly one bounded, no-tool summarization request under the agent's effective context budget. The request includes the original covered turns verbatim; the runtime refuses the result if the model returns tool calls, empty text, oversized output, a summary whose framed provider-message form would not usefully shrink the projection, or a projection that exceeds the live message-count limit. Provider retry policy may still make several HTTP attempts inside the one logical operation.

Before the durable commit, every failure (cancellation, timeout, provider refusal, stale source, no useful savings) leaves the prior projection untouched. The commit is one SQLite write transaction that rechecks the session incarnation, kill state, in-flight turns, exact source identity, and the prior active checkpoint, so a stale request can never overwrite a later operation.

### The projection

The summary is derived data, never canonical history. Original messages are retained unchanged; `AcpSessionStore` keeps the one active checkpoint (coverage boundary, summary, model provenance, operation id) beside them. The provider-facing projection retains the original opening user message, followed by one labeled assistant-context summary and the retained tail. The user message anchors the historical exchange for provider turn-order compatibility; it is not a generated instruction. The summary is an automated, explicitly lossy historical record of lower trust, never a user command, system instruction, approval, or tool result. Both the anchor and summary count toward the savings threshold and flow through the same budget accounting and trimming safeguards as other history entries, so a later trim can still drop them with the usual visible-omission breadcrumb.

Both explicit native resume and lazy rehydration seed from the same projected read, so live continuation and restart select the same committed projection. Intentional transcript reads (`session/messages`) always return originals through the raw transcript reader. The unadapted external ACP `session/load`/`session/resume` path fails closed for sessions with an active checkpoint instead of silently returning unprojected originals.

### Restore and recompaction

`/restore-context` deactivates the checkpoint and rebuilds the projection from retained originals plus every turn appended after the compaction, subject to the ordinary visible limits. It never rewinds the conversation, reruns tools, or reverses files and external effects. A later `/compact-context` recomputes its coverage from the originals, never from the previous summary.

Operation ids make retries idempotent: a retry of a committed compaction is recognized (`already_committed`) without a second model operation, and an old compact retry whose checkpoint was already restored or superseded reports `superseded` without touching the current projection. Restore is likewise fenced: a stale restore cannot deactivate a checkpoint a later compaction installed.

### Durability and teardown

Durable writes use the store's existing WAL configuration (`synchronous = NORMAL`), which survives process restarts but makes no power-loss guarantee. The durable work runs inside an owned settlement task holding session admission: if the RPC connection is torn down mid-operation, the response task is aborted but the settlement still joins its commit, installs or invalidates the exact live incarnation, and only then releases the session. A post-commit install failure invalidates that incarnation so the next prompt rehydrates from the committed checkpoint.
