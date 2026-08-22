# zeroclaw-eval

Agent evaluation harness for ZeroClaw.

**Phase 0 — deterministic replay.** Runs the *real* agent loop against scripted
LLM responses (an `LlmTrace` fixture) and grades the outcome against declarative
expectations. Because the model output is fixed, a replay eval is free, fast, and
fully deterministic: it proves the agent *machinery* (tool parsing, dispatch,
multi-turn looping) behaves correctly given a known model output. It does **not**
measure model quality — that is the live mode added in a later phase.

## CLI

```bash
# Replay every *.json fixture in the suite directory
# (defaults to [eval].suite_dir = evals/regression)
zeroclaw eval run

# Point at an explicit suite, emit machine-readable JSON
zeroclaw eval run --suite evals/regression --format json
```

Exits non-zero if any case fails, so it can gate CI. `--mode live` is reserved for
a later phase and currently returns a clear error.

## Case format

A case is a JSON trace fixture: scripted LLM response steps per turn, plus
declarative `expects` the run is graded against.

```json
{
  "model_name": "single-tool-echo",
  "turns": [
    {
      "user_input": "Echo hello for me",
      "steps": [
        { "response": { "type": "tool_calls",
          "tool_calls": [{ "id": "call_1", "name": "echo", "arguments": {"message": "hello"} }] } },
        { "response": { "type": "text", "content": "The echo tool said: hello" } }
      ]
    }
  ],
  "expects": {
    "response_contains": ["hello"],
    "tools_used": ["echo"],
    "max_tool_calls": 1,
    "all_tools_succeeded": true
  }
}
```

Supported expectations: `response_contains`, `response_not_contains`,
`response_matches` (regex), `tools_used`, `tools_not_used`, `max_tool_calls`,
`min_tool_calls`, `exact_tool_calls`, `all_tools_succeeded`,
`tool_arguments_contain`, `tool_results_contain`.

### Grading the dispatch boundary

`response_contains` only ever grades text the replay provider scripted for
itself, so on its own it cannot show that a value survived the round trip
through the agent. `tool_arguments_contain` and `tool_results_contain` grade
what actually crossed the dispatch boundary — the arguments the agent passed to
the tool, and the output the tool returned:

```json
"expects": {
  "exact_tool_calls": 2,
  "tool_arguments_contain": [
    { "tool": "echo", "needle": "alpha", "call_index": 0 },
    { "tool": "echo", "needle": "beta",  "call_index": 1 }
  ],
  "tool_results_contain": [
    { "tool": "echo", "needle": "alpha", "call_index": 0 }
  ]
}
```

`call_index` is optional and 0-based across calls to the named tool in dispatch
order; omit it to accept a match on any call to that tool. Prefer
`exact_tool_calls` over `tools_used` + `max_tool_calls` when the fixture claims a
specific number of dispatches: `tools_used` is existential and `max_tool_calls`
is only an upper bound, so together they still pass when a dispatch is missing.

Replay fixtures may only call tools the harness registers; Phase 0 ships a
side-effect-free `echo` tool (see `tools::default_tools`). Wiring the real
sandboxed tool registry for live evals is a later phase.

## Library shape

- `case` — the `LlmTrace` fixture format + suite loading.
- `replay::TraceLlmProvider` — a `ModelProvider` that replays trace steps in FIFO order.
- `tools` — deterministic built-in tools the replay agent can dispatch.
- `observer::RecordingObserver` — captures each dispatched tool call
  (`RecordedCall`: name, arguments, result, success) and token usage.
- `grader` — non-panicking `GradeResult` checks (the `Grader` trait is the
  extension point for side-effect/budget/LLM-judge graders in later phases).
- `runner` — builds an isolated agent per case, drives it, grades it.
- `report` — pass/fail aggregation, table + JSON rendering.
