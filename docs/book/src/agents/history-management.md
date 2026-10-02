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
tool results of the current user turn. Markers that preparation strips from
older tool results are not charged, and system and assistant text is priced as
text.

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

## Pairing safety

Whole-turn retention is the primary tool-pairing guarantee: a tool call and its
result belong to the same turn and are retained or dropped together. The orphan
sweep remains a final safety net for histories that were already inconsistent,
such as restored or externally modified sessions.

Tool-result length limits are separate. `max_tool_result_chars` bounds an
individual result when it is recorded; it does not trim conversation history.
Provider-side context enforcement is also separate, though a provider overflow
can trigger the runtime's reactive token-budget trim.

## Scheduled jobs bound to a conversation

An agent cron job with `session_target = "main"` belongs to the conversation
it was created from. When `cron_add` creates such a job during a channel turn,
the runtime records that conversation beside the job. The record comes from
the turn the tool call is running in. No tool argument can set or change it,
and it does not appear in what `cron_list` or the cron API return.

Each scheduled run of a bound job then does two things:

- It loads the conversation's history as context, between the system prompt
  and the job's own message, in the same cleaned form a channel turn sends.
  When the history ends with a message nobody has answered yet, the job's
  message is joined to it, as consecutive user messages are in a channel
  turn.
- When the run completes with something to say, it appends one exchange to
  the conversation: the job's message, prefixed `[cron:<id> <name>]`, and the
  reply. The append goes through the writer the channel's own turns use, so
  nothing is lost when a channel turn is in progress at the same moment, and
  the next message in that conversation sees what the job said.

The binding selects history only. Where the result is delivered is still the
job's `delivery` settings, and the job gains no access to any other
conversation or to another agent's conversations. Three rules keep it that
way:

- **Manual runs.** A manual run returns the job's output to whoever triggered
  it, so it uses the conversation only when it is triggered from that same
  conversation (`cron_run` during one of its turns). Triggered anywhere else,
  including the gateway and RPC, the job runs without the conversation.
- **Edits.** Changing what a bound job is told, what it may use, or where its
  output goes (`prompt`, `name`, `delivery`, `model`, `allowed_tools`,
  `uses_memory`, `session_target`) from anywhere other than its conversation
  removes the binding. Pausing, resuming, and rescheduling keep it.
- **Ownership.** The conversation belongs to the agent the job is stored
  under, on the channel it arrived on. If configuration later hands the job
  to a different agent, or hands that channel to a different agent, the job
  no longer reaches the conversation.

A `main` job with no recorded conversation still runs, without conversation
context. That covers jobs declared in configuration (declaring an existing
job there removes its binding), jobs created outside a channel conversation
(the gateway, the CLI, ACP), jobs created before this behaviour existed, jobs
switched to `main` by `cron_update`, and jobs that lost their binding to an
edit. The runtime does not pick a conversation on
the job's behalf. `isolated` jobs are never bound.

A run that finishes while a channel turn is still in progress in the same
conversation lands between that turn's message and its reply. Both are kept;
the conversation then reads as the two exchanges interleaved, as it does when
two channel turns overlap.

Each run records the result in the `persistence` field of its run record:

| Value | Meaning |
| --- | --- |
| `not_bound` | No conversation applies to this run. |
| `persisted` | The run's exchange was appended to the conversation it ran with. |
| `skipped` | The run had its conversation but left nothing to record: it failed, or it deliberately said nothing (`NO_REPLY`, or an empty reply). |
| `failed` | A conversation is recorded for the job, but it could not be reached before the run or written after it: the agent is not serving that channel in this process, the job now runs under a different agent, or the history could not be read or written. |

The scheduler and the channels start independently. For the first 60 seconds
after the scheduler starts, a scheduled run of a bound job waits for the
agent's channels to start serving, so a job that is overdue when the daemon
starts still runs with its conversation. After that window a run does not
wait: if the agent's channels are not serving, it runs without the
conversation and records `failed`.
