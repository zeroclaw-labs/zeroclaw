# Eval harness

The eval harness (`zeroclaw eval run`, crate `crates/zeroclaw-eval`) runs agent
evaluation *cases* (JSON trace fixtures) through the real agent loop and grades
each run against declarative expectations. It is how ZeroClaw guards agent-loop
behavior (tool dispatch, multi-turn ordering, response formatting, refusals)
against regression.

It is distinct from `[agent.eval]`, the in-loop response-quality scorer. The
harness is configured under `[eval]` and invoked as a CLI subcommand.

## Modes

| Mode | What it does | Cost | CI |
|---|---|---|---|
| `replay` | Replays scripted LLM responses from the fixture through the agent loop. Fully deterministic, no network. | Free | Gated (default) |
| `live` | Executes cases against a real provider (planned; see the live-mode section once it lands). | Real tokens | Never by default |

## Suite taxonomy

Suites are directories of `*.json` fixtures (see `evals/README.md`):

- `evals/regression/`: must stay at 100% pass. Gated in CI via
  `crates/zeroclaw-eval/tests/regression_suite.rs`; a failure blocks merge. This
  is the default `[eval].suite_dir`.
- `evals/capability/` (planned): hard tasks with a low pass rate; tracked over
  time, never gated.
- `evals/live/` (planned): cases executed against a real provider; never run in
  CI by default.

## Running

```bash
# Replay the default regression suite:
zeroclaw eval run

# Point at a specific suite, emit machine-readable JSON:
zeroclaw eval run --suite evals/regression --format json
```

`--suite` overrides `[eval].suite_dir`; `--mode` overrides `[eval].mode`. Suite
loading is non-recursive: only direct `*.json` children of the suite directory
are cases.

## Exit-code contract

`zeroclaw eval run` exits `0` iff every case passed, and `1` otherwise (any
failed check or run error). This is the CI gate: the process exit code is the
signal. The same decision is exposed as the pure function
`SuiteReport::exit_code()` so it can be tested at its real boundary.

## Case format

Each fixture is an `LlmTrace`: a `model_name`, a list of conversation `turns`
(each with a `user_input` and scripted response `steps`), and declarative
`expects`. A case is either **positive** (a behavior that must happen) or
**negative** (a behavior that must NOT happen, e.g. `tools_not_used`,
`response_not_contains`, `max_tool_calls: 0`). See `evals/README.md` for the
authoring rules, including the two-experts test and the privacy requirement that
fixtures use placeholder identities only.
