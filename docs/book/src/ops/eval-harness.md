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

`--format` selects `table` (default), `json`, or `junit`. JUnit XML maps each
case to a `<testcase>`: a failing case becomes a `<failure>`, a run error an
`<error>`, and a case that is unverifiable against a baseline a `<skipped/>`.

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

## Baselines and regression gating

Suites have a kind, resolved from the directory name (or the `--suite-kind`
override): a `capability` suite is tracked but never gating; everything else has
**regression** semantics (must stay green).

A **baseline** file (`zeroclaw-eval/baseline/v1`, stored under `evals/baselines/`)
records each case's verdict and comparability key from a prior run:

- `--write-baseline <file>` writes the current run as a baseline and exits with the
  run's normal code.
- `--baseline <file>` compares the current run against it, per case id.

Comparison is keyed by the comparability tuple `(case_hash, mode, provider_ref,
tool_surface)`:

- A changed key reports `changed - refresh baseline` (Unverifiable) and is never
  compared or gated.
- Baseline pass and current fail on a comparable case is a **regression**,
  classified by which categories flipped (response / tool / side-effect / budget).
- Current pass and baseline fail is an **improvement** (reported, never gates); a
  case only in the current run is **new**; a case only in the baseline is
  **removed** (warned). Per-case token deltas are reported as a percentage and are
  never gated.

**Live flakiness rule:** in live mode, a comparable case that regressed is re-run
once; if the re-run passes it is reported as `flaky (unconfirmed regression)` and
does not gate. Replay flips the gate directly with no retry (it is deterministic).

Gating is strictly per-case Pass to Fail flips; aggregate score deltas are never a
gate. To refresh a baseline after an intentional behavior change, re-run with
`--write-baseline` and commit the updated file.

## LLM judge (diagnostic by default)

A case can declare `expects.judge`: a list of rubrics, one per dimension. Each
rubric has a `name`, a `rubric` string, a pass `threshold` (default 0.7 on the
judge's 0.0 to 1.0 score), and an optional `include_transcript` flag. Configure a
judge with `[eval].judge_provider` (a dotted `providers.models` reference); prefer
a different model family than the one under test, since self-judging is biased
(the harness warns when the judge and live provider share a family).

Each rubric is graded by one isolated judge call at temperature 0.0. The judge's
0.0 to 1.0 score decides pass/fail against the threshold; its own opinion is
advisory. A score at or above the threshold passes; below fails; an `unknown`
verdict, malformed output, or a transport error is reported as
`UNKNOWN (diagnostic)` and never fails a build.

**Judge grades are diagnostic by default:** they are stripped from the pass/fail
gate unless `[eval].judge_gate` is true AND a calibration file exists at
`evals/calibration/<judge_ref>.json`, where `judge_ref` is the model-inclusive
`<type>.<alias>:<model>` with `/`, `.`, and `:` replaced by `_` (so calibration
is model-specific, matching the comparability key). When
`judge_gate` is set but no calibration file exists, the harness warns and stays
diagnostic. Judge token usage is never added to the case's own token totals (the
judge runs outside the agent), and the judge reference joins the baseline
comparability key, so swapping judges makes cases unverifiable rather than
silently compared.

Authoring rules: one dimension per rubric entry, and every judge case must also
declare at least one deterministic check (workspace, tool, or budget) so it is not
judge-only. Calibration protocol: dump at least 50 records with `--dump-records`,
have a human label them, compute the judge's agreement with the human labels, and
commit a calibration file `{"schema":"zeroclaw-eval/calibration/v1","judge_ref":
"...","labeled_records":N,"agreement":0.0-1.0,"labeler":"...","date":"YYYY-MM-DD"}`
with N at least 50 before enabling `judge_gate`.

## Exit-code contract

The process exit code is the CI gate, and it is suite-kind aware:

- **Regression suite, no baseline:** `0` iff every case passed, else `1`.
- **Regression suite, with `--baseline`:** `0` iff every failing case is excused,
  i.e. `1` if any case fails that is not `Unverifiable` (comparability key changed)
  or `flaky (unconfirmed regression)`. Confirmed per-case Pass to Fail flips gate;
  aggregate score or token deltas never do.
- **Capability suite:** always `0` unless a case ERRORED (a run error, not a check
  failure), which still exits `1`.

The decision is the pure function
`SuiteReport::exit_code(kind, comparison)` so it can be tested at its real boundary.

## Run receipts and record dumps

Every case run produces a receipt: a schema tag, the mode, the case id, a
SHA-256 `case_hash` of the case's canonical JSON, the `provider_ref`
(`scripted` for replay, `<type>.<alias>:<model>` for live), the sorted effective
`tool_surface`, and a `sandbox` stamp. These fields appear per case in the JSON
report and make runs comparable across time (the baseline workflow builds on
them).

Records can be dumped as JSON:

- `--dump-records <dir>` writes `<dir>/<case_id>.json` (record plus grades) for
  every case.
- On every run, any failed or errored case is auto-dumped to
  `target/eval-last-run/<case_id>.json` (cleared at the start of each run). When
  any exist, the table footer prints `failed-case records: target/eval-last-run/`.

Dumps are debugging artifacts, not fixtures. A live transcript can embed
workspace file content and model output, so **never commit a dump**;
`target/` is gitignored. Promoting a dump into a suite fixture requires the same
privacy placeholder pass as any other fixture (see the privacy contract): no real
names, transcripts, hostnames, or credentials.

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
