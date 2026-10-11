# History management

The runtime keeps conversation history for each agent session and sends a
provider-facing working history to the model. Two complementary limits operate
on different representations:

1. **Token-budget trimming** acts on the provider-facing `ChatMessage` working
   history and drops oldest whole turns until the estimated context fits the
   token budget.
2. **Whole-turn retention** applies the runtime profile's configured history
   limit to structured `Agent::history` (`ConversationMessage`) and the
   provider-facing `ChatMessage` history used by the legacy agent loop.

Both limits retain turns atomically. A turn starts at a real user message and
includes the assistant response and any tool calls and tool results before the
next user message. Trimming therefore does not split a tool call from its
result. A user message whose text happens to begin with `[Tool results]` is
treated as part of the turn before it, not as a new turn.

## Whole-turn retention

`history_trim::trim_to_recent_turns` enforces the token budget.
`history_trim::trim_to_recent_turn_count` and
`history_trim::trim_conversation_to_recent_turns` enforce the configured turn
limit for the two history representations. Each keeps the newest complete turn
even when the configured value is `0`. This is intentional: preserving a
complete current turn is safer than dropping its newest messages or breaking a
tool exchange.

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
- Every positive effective budget is capped by the selected model's
  *configured* capacity. The 32,000 fallback is a compatibility stub, not
  model truth: when the provider profile declares no `context_window` and the
  runtime profile sets a positive `max_context_tokens`, that budget is honored
  and also becomes the window operand (so a ratio applies to it) instead of
  being clamped down to 32,000. The explicit zero sentinel remains zero and
  continues to disable proactive trimming.

The effective budget is a proactive trimming target, not a hard request limit.
After dropping all eligible older turns, the runtime retains the newest complete
turn even if it remains above that target. It sends the request when the prepared
messages, images, hooks, and tool schemas fit the resolved model context window.
A request that still exceeds that capacity fails before provider dispatch; raising
the proactive target alone cannot make it fit. This capacity check also applies
when proactive trimming is disabled (`max_context_tokens = 0`) and to the final
summary request after the tool iteration limit is reached.

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
not a provider tokenizer. Loadable `[IMAGE:...]` markers are charged a fixed
per-image cost only in messages whose images are dispatched: user turns and the
tool results of the current user turn. A tool result's images are the
attachments the tool declared, not marker text in its output. Older tool
results are priced as their message bytes, and system and
assistant text is priced as text.

Proactive token-budget trimming runs before the first provider call of a turn
when history already exceeds the effective budget and at provider-call
boundaries between tool-loop iterations. If a provider reports a context-window
overflow, the existing reactive recovery path retries after trimming to two
thirds of the current estimated history size. It does not derive a new recovery
target from model capacity. Both paths retain whole turns, so neither splits a
tool exchange.

## Whole-turn limit

`max_history_messages` is the legacy name of the history setting in an agent's
runtime profile. In structured agents and the legacy agent loop, its unit is a
complete user turn rather than an individual message row. Tool calls and tool
results remain part of their surrounding turn and do not consume independent
slots.

The default is `50` turns. An explicitly configured value is authoritative,
including `0`; the newest complete turn is still retained. The field name is
kept for schema compatibility and can be renamed in a future schema version.

Channel sender caches remain a separate exception: they count individual
message rows and preserve their existing `0`-means-default behavior.

## Visible trimming

Whenever token-budget trimming or the whole-turn limit drops older turns, the
runtime:

1. Inserts a breadcrumb before the first retained turn so the model knows that
   earlier context was omitted.
2. Emits `HistoryTrimmed` with the number of dropped messages, retained turns,
   and a reason identifying the token budget or turn limit.

The event is surfaced through the active client transport and through the
observer path used by dashboards and event subscribers. Trimming is therefore
not log-only and is not silent to either the model or connected clients.

The legacy `agent::run` path in `loop_.rs` reports count-based trimming through
logs only, without the breadcrumb or `HistoryTrimmed` event. This path serves
interactive use as well as one-shot and non-interactive daemon, cron, subagent,
and SOP callers.

## Tool context retention

`keep_tool_context_turns` (runtime profile, default `2`) is the number of
completed turns before the running one whose tool-call and tool-result rows are
still sent to the model. Every older turn is sent as its user prompt, one
`[Tool exchange: N tool call(s) — results collapsed]` summary row in place of the
assistant row that issued the calls, and the closing assistant reply, so the
model still sees what was asked and what was answered without the raw tool
output. The running turn is always sent whole because the model needs its own
results, so `0` keeps tool context for the running turn only; the local-model
preset's `1` keeps it for one prior turn as well, and case-style presets use `8`.

This is a request-time bound, not a transcript edit. The tool loop builds each
provider request from a collapsed copy; the working history and every store an
owner persists from it (channel sender cache, ACP transcript, CLI session file)
keep all their rows, so raising the value later shows older tool rows again. The
summary row is the marker the providers already recognise: those that reject
consecutive assistant rows skip it before dispatch, so on those routes the older
turn is sent as prompt and reply only.

A turn collapses as a unit, in both native (`role=tool`) and prompt-mode
(`[Tool results]` carrier) shapes, so no orphan call or result is created. A
turn that ended on a call or a result keeps only its prompt and the summary; an
assistant row that still carries native tool calls is never kept as a closing
reply. Leading system messages and the trim breadcrumb are copied through. The loop's
own mid-turn feedback row (`[Tool call parse error]`, user-role because no tool
call id exists to attach it to) belongs to the turn it interrupts and does not
open a new one for retention. Two
loops that run on the caller's own history as part of the caller's turn never
collapse: live SOP steps and the skill-review fork, since each step or review
prompt is a user row and the caller's turn would otherwise look older from
inside them. The first request of a turn that collapses anything logs a `Tool
context collapsed` record with the row and turn counts; no `HistoryTrimmed`
event is emitted because no turn is lost.

On channels the same key also decides whether the current turn's tool rows are
persisted into the sender cache at all (`0` skips them).

## Pairing safety

Whole-turn retention is the primary tool-pairing guarantee: a tool call and its
result belong to the same turn and are retained or dropped together. The orphan
sweep remains a final safety net for histories that were already inconsistent,
such as restored or externally modified sessions.

Tool-result length limits are separate. `max_tool_result_chars` bounds an
individual result when it is recorded; it does not trim conversation history.
Provider-side context enforcement is also separate, though a provider overflow
can trigger the runtime's reactive token-budget trim.
