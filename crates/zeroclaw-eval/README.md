# zeroclaw-eval

Agent evaluation harness for ZeroClaw.

**Replay mode (default) — deterministic.** Runs the *real* agent loop against scripted
LLM responses (an `LlmTrace` fixture) and grades the outcome against declarative
expectations. Because the model output is fixed, a replay eval is free, fast, and
fully deterministic: it proves the agent *machinery* (tool parsing, dispatch,
multi-turn looping) behaves correctly given a known model output. It does **not**
measure model quality.

**Live mode — real provider.** Runs the same fixtures against the provider named by
`[eval] live_provider`: real tokens, real network egress, non-deterministic output.
The tool surface is the `[eval] live_allowed_tools` allowlist, `shell` is hard-denied
regardless of what a case or the config requests, approvals run through a
non-interactive backchannel manager that auto-denies anything not allowlisted, and
each turn is bounded by `[eval] case_timeout_secs`.

## CLI

```bash
# Replay every *.json fixture in the suite directory (defaults to ./evals/regression)
zeroclaw eval run

# Point at an explicit suite, emit machine-readable JSON
zeroclaw eval run --suite evals/regression --format json

# Run against the real provider from `[eval] live_provider` (real tokens, real egress)
zeroclaw eval run --mode live
```

Exits non-zero if any case fails, so it can gate CI.

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
`all_tools_succeeded`.

Replay fixtures may only call tools the harness registers; Phase 0 ships a
side-effect-free `echo` tool (see `tools::default_tools`). Wiring the real
sandboxed tool registry for live evals is a later phase.

## Library shape

- `case` — the `LlmTrace` fixture format + suite loading.
- `replay::TraceLlmProvider` — a `ModelProvider` that replays trace steps in FIFO order.
- `tools` — deterministic built-in tools the replay agent can dispatch.
- `observer::RecordingObserver` — captures tool-call outcomes and token usage.
- `grader` — non-panicking `GradeResult` checks (the `Grader` trait is the
  extension point for side-effect/budget/LLM-judge graders in later phases).
- `runner` — builds an isolated agent per case, drives it, grades it.
- `report` — pass/fail aggregation, table + JSON rendering.
