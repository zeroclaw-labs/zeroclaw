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
| `live` | Executes cases against a real provider inside a per-case sandbox (see "Live mode"). | Real tokens | Never by default |

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

## Live mode

Live mode (`--mode live`) runs each case against a real configured provider, so it
costs real tokens and produces non-deterministic output. It is opt-in and never
runs in CI by default. Enable it by setting `[eval].live_provider` to a dotted
`providers.models` reference (e.g. `"anthropic.sonnet"`); an empty value keeps live
mode disabled.

A live case omits scripted `steps` (the provider produces the responses) and may
declare `tools` it needs and a `setup.workspace_files` map to seed the workspace.
The requested tools are intersected with `[eval].live_allowed_tools`; the default
(empty) allows no real tools, so a case that needs tools requires the operator to
opt in explicitly.

Each live case runs inside a sandbox:

| Control | Behavior |
|---|---|
| Workspace | Fresh per-case temp directory; `workspace_only` policy blocks reads and writes outside it. |
| Tool registry | Runtime default tools filtered to `case.tools` intersected with `[eval].live_allowed_tools`; empty allowlist yields only the harmless echo tool. |
| Autonomy | `Supervised`, never `Full`. |
| Approvals | Non-interactive backchannel manager: allowlisted tools auto-approve; anything else that reaches the approval gate is auto-denied (deterministic case failure). |
| Timeout | Each turn is bounded by `[eval].case_timeout_secs` (default 120); a slow turn fails the case rather than hanging. |

Because live output is non-deterministic and can embed workspace content, live runs
belong in the planned `evals/live/` suite, not the gating regression suite.

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

## Expectations reference

`expects` collects declarative checks. Every field is optional; each declared
check becomes one graded result, tagged with a category (`response`, `tool`,
`side_effect`, `budget`, `judge`) surfaced in the JSON report along with a
per-case `score` and `category_totals`.

Response checks (category `response`):

- `response_contains` / `response_not_contains`: substrings that must / must not
  appear in the final response.
- `response_matches`: regex patterns the final response must match (an invalid
  regex is a failed check, not a crash).
- `response_json`: a map of JSON pointer to expected value. The final response is
  parsed as JSON (falling back to the first ` ```json ` fenced block); each
  pointer must resolve to the expected value. If neither parse succeeds, every
  pointer check fails with "response is not JSON".

Tool checks (category `tool`):

- `tools_used` / `tools_not_used`: tool names that must / must not have been
  called.
- `max_tool_calls`: inclusive upper bound on the number of tool calls.
- `all_tools_succeeded`: whether every tool call must have succeeded.

Workspace checks (category `side_effect`), under `workspace`:

- `file_exists` / `file_absent`: workspace-relative paths that must / must not
  exist after the run.
- `file_contains`: a map of path to substrings that must appear in that file.

Every workspace path is validated as workspace-relative first; a path that
escapes the workspace (absolute or containing `..`) is a failed check, never a
filesystem access.

Budget checks (category `budget`), under `budget`, each an inclusive bound:
`max_input_tokens`, `max_output_tokens`, `max_total_tokens`, `max_duration_ms`,
`max_llm_calls`.

Example combining a workspace and a budget check:

```json
{
  "model_name": "writes-a-report",
  "id": "gh1234_report",
  "tools": ["file_write"],
  "turns": [{ "user_input": "Write status.json with status ok." }],
  "expects": {
    "workspace": {
      "file_exists": ["status.json"],
      "file_contains": { "status.json": ["ok"] }
    },
    "budget": { "max_llm_calls": 4, "max_total_tokens": 2000 }
  }
}
```
