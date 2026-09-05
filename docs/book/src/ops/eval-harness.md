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
| Tool registry | Runtime default tools filtered to `case.tools` intersected with `[eval].live_allowed_tools`, then `shell` is dropped unconditionally (see "Shell is excluded" below); empty allowlist yields only the harmless echo tool. |
| Autonomy | `Supervised`, never `Full`. |
| Approvals | Non-interactive backchannel manager: allowlisted tools auto-approve; anything else that reaches the approval gate is auto-denied (deterministic case failure). |
| Timeout | Each turn is bounded by `[eval].case_timeout_secs` (default 120); a slow turn fails the case rather than hanging. |

### Shell is excluded

`shell` can never be part of the live tool surface, even when a case's `tools`
and `[eval].live_allowed_tools` both request it. `effective_live_tools`
(`crates/zeroclaw-eval/src/live.rs`) applies a hard denylist to the allowlist
intersection, so deny always wins.

A scripted `shell` tool call in a live case is stopped *before* tool dispatch,
by the approval gate. Because `shell` is excluded from the effective tool set,
it is also excluded from `risk.auto_approve` (both are built from the same set
in `run_live_case`), so its approval requirement resolves to `Prompt`. Live
mode wires a non-interactive backchannel, so there is no operator to ask and
the runtime denies the call by policy. What the model sees fed back is a
runtime-policy denial, not shell output and not a "tool not available"
dispatch error:

```text
Tool call not executed: 'shell' requires approval and no operator decision was
available, so the runtime denied it by policy. This was not a user's decision.
```

Operators debugging a live case that expected `shell` should therefore look for
the approval-gate denial (a WARN record with `denied_by_runtime`), not for a
tool-registry lookup failure.

This is the ship-safe interim posture. An
earlier version of this harness wrapped `shell`'s subprocesses in a real OS
sandbox backend (Landlock, Firejail, or `sandbox-exec`) instead of excluding
it outright, but every accepted backend still permitted host *reads* wide
enough to leak host data back into the conversation sent to a real provider:

- Linux, Landlock (`sandbox-landlock` feature): the child process's filesystem
  access was confined to the case workspace plus a blanket `/tmp` allowance,
  with `/usr` and `/bin` readable. Network was NOT restricted (no `AccessNet`
  rule); a sandboxed shell command could still reach the network freely.
- macOS, `sandbox-exec` (Seatbelt): deny-by-default for writes, but reads were
  allowed broadly: system paths (`/usr`, `/bin`, `/sbin`, `/Library`,
  `/System`, `/etc`, `/opt`, and others) and the invoking user's dotfile
  directories under `$HOME`.
- Firejail (Linux, no `sandbox-landlock` feature): `--private=home` with
  `--noprofile` added no workspace whitelist, read-only host-root rule, or
  network restriction beyond that.

Confining the *writes* (which those backends do well; see
`crates/zeroclaw-eval/tests/live_shell_sandbox.rs`'s history for the escape
tests this proved) was not sufficient, because live-mode tool output becomes
part of the conversation sent to the configured provider, making it a
confidentiality boundary and not just an integrity one. Re-admitting `shell` needs an
eval-specific sandbox contract that also denies sensitive host reads on every
accepted backend; that is a deliberate, tracked follow-up, not implemented
here. `live_shell_sandbox`/`ensure_real_sandbox` (the OS-sandbox construction
`shell` used to run under) remain in `live.rs` as building blocks for it.

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
tool_surface, sandbox)`. The tool surface records requested, effective, and
registered tools, and the sandbox posture is part of the key, so runs with
different actual capabilities are never called comparable:

- A changed key reports `changed - refresh baseline` (Unverifiable) and is never
  compared or gated.
- Baseline pass and current fail on a comparable case is a **regression**,
  classified by which categories flipped (response / tool / side-effect / budget).
- Current pass and baseline fail is an **improvement** (reported, never gates); a
  case only in the current run is **new**; a case only in the baseline is
  **removed** (warned). Per-case token deltas are reported as a percentage and are
  never gated.
- A current case that errored is reported as a **run error** (`CurrentError`),
  never `removed`, and always gates. Its record retains pre-run provenance but
  has no completion data.

Baseline inputs fail closed: a baseline file with an unrecognized `schema` tag,
unknown fields, an empty case id, or duplicate case ids is rejected at parse
time, and a current run with duplicate case ids is rejected at comparison time,
so malformed or ambiguous inputs can never produce a trusted gate result.

Baseline *writes* fail closed too. A baseline must describe every case in the
suite, so `--write-baseline` refuses to write at all when any case errored or did
not produce grades and completion data; the error names the offending case ids. This is deliberate:
an omitted case would be classified merely `new` on the next run, and a failing
`new` case never gates, so a silently shortened baseline would convert a hard
regression into a permanently excused case. The check runs before touching the
baseline target and the write itself is atomic, so a failed run neither creates a
new baseline nor replaces an existing one. A case that completed but *failed its
checks* is still recorded normally.

### Baseline schema compatibility

Baseline entries carry a three-stage `tool_surface` and a `sandbox` stamp, both
of which are part of the comparability key. These widen the
`zeroclaw-eval/baseline/v1` entry schema, and the parser is strict: **baseline
files written before this change are rejected and must be regenerated once**
with `--write-baseline`. This is a one-time migration:
regenerate on a known-green run so the new reference is trustworthy, and commit
the refreshed file.

**Live flakiness rule:** in live mode, a comparable case that regressed is re-run
under its own effective repeat policy; if the re-run clears `pass^k` it is
reported as `flaky (unconfirmed regression)` and does not gate. A `repeat: k`
case therefore cannot be excused by one lucky attempt. Replay flips the gate
directly with no retry (it is deterministic).

Gating is strictly per-case Pass to Fail flips; aggregate score deltas are never a
gate. To refresh a baseline after an intentional behavior change, re-run with
`--write-baseline` and commit the updated file.

## Repeated runs (pass@k, pass^k, variance)

A live case can set `repeat: k` (clamped to 1..=50) to run k fully isolated times
(fresh workspace, agent, and provider each run). Replay is deterministic, so
`repeat > 1` runs once with a warning.

Per case the report gives `passes/k`, `pass@k` (passes > 0), `pass^k` (all k
passed), per-check flip counts, and the mean and sample standard deviation of
total tokens and duration. A live case counts as PASSED for gating and baselines
iff `pass^k` (the consistency standard).

At the suite level, each case's success proportion `p_i = passes_i / k_i` is
collapsed first (one value per case), so correlated resamples do not fake
precision; the report prints the repeated-case pass rate with a bounded 95%
interval and the `n of N cases have complete repeat statistics` population. The
displayed bounds are clamped to `[0%, 100%]`. The implementation uses a
Student-t multiplier on n-1 degrees of freedom, including a finite-df
approximation above the embedded small-sample table; it never substitutes the
infinite-df normal value for a finite sample.

That statistic is deliberately restricted to cases that actually repeated
(effective `k > 1`). Effective `k = 1` cases and cases whose run errored have no
within-case success proportion, so including them would give a single-shot case
the same weight as a 20-run case. It is therefore **not** the suite pass rate --
the suite's own `passed/total` line is reported separately, and the two can
legitimately differ. The printed population (`n of N cases have complete repeat
statistics`) makes the exclusion explicit.

An optional per-case `cluster` label averages correlated case families together
before the error bar; omitting it asserts independence. With fewer than two
independent units, the report shows the observed rate and marks the 95% CI
unavailable.

A case with `0/k` passes at `k >= 5` is flagged low-signal, and at `k >= 20` is
flagged for inspection. The count alone does not assign the cause to either the
task or the agent.

**Partial repetition sets.** If a repetition errors partway through, the
repetitions that already completed are retained and reported (`repeat p/k (c
completed)`) together with the error, rather than discarded -- live runs are paid
for, and the partial evidence is what makes the aggregate disputable. Such a set
is fail-closed: the missing repetitions never count as passes, so it cannot
establish `pass^k`, and the case fails. Baseline retries apply the same completeness
check: a passing representative from a truncated retry never downgrades a regression.

The JSON report and record dump also contain a stable, one-based `attempts` list.
Each completed repetition records its pass/fail outcome, token and duration
metrics, LLM-call count, and per-check verdicts. The repetition that stops a
partial set is retained as an explicit error entry. These are deliberately
minimal receipts: case-level provenance stays on the representative run record,
and full transcripts are not duplicated for every repetition.

## Exit-code contract

The process exit code is the CI gate, and it is suite-kind aware:

- **Regression suite, no baseline:** `0` iff every case passed, else `1`.
- **Regression suite, with `--baseline`:** the per-case comparison is the single
  gating authority. `1` iff there is at least one confirmed Pass to Fail flip on
  a comparable case, or a case ERRORED (a run error has no trustworthy
  comparison). Failures classified `new`, `unchanged` (failed in both runs),
  `unverifiable` (comparability key changed), or `flaky (unconfirmed
  regression)` are reported but never gate; aggregate score or token deltas
  never do either. A new or still-failing case gates on the next run without
  `--baseline`, or once a refreshed baseline records it as passing.
- **Capability suite:** always `0` unless a case ERRORED (a run error, not a check
  failure), which still exits `1`.

The decision is the pure function
`SuiteReport::exit_code(kind, comparison)` so it can be tested at its real boundary.

## Machine-readable output (`--format json`)

`--format json` emits one complete JSON document on stdout. Every document
carries `suite_kind` and the `exit_code` the process exits with. When
`--baseline` was given, a top-level `baseline` section explains the gate:

```json
{
  "passed": 5, "failed": 1, "total": 6, "all_passed": false,
  "suite_kind": "regression", "exit_code": 1,
  "cases": [ ... ],
  "baseline": {
    "per_case": {
      "case-a": { "classification": "regression", "categories": ["tool"] },
      "case-b": { "classification": "unchanged", "token_delta_pct": 2.0 }
    },
    "confirmed_regressions": 1,
    "current_errors": 0,
    "flaky_unconfirmed": 0,
    "gates": true
  }
}
```

`per_case` classifications are `new`, `removed`, `current_error`,
`unverifiable`, `regression` (with flipped `categories`),
`flaky_unconfirmed`, `improvement`, and `unchanged` (with
`token_delta_pct` when comparable). A failing CI artifact therefore always
states why the gate failed; the exit code never encodes information missing
from the document.

When completed repeated cases are present, `repeat_ci` is a structured object
with the `[0, 1]` `pass_rate`, bounded `lower` and `upper` confidence limits,
the repeated-case and suite populations, and the effective independent-unit
count after cluster collapsing. `lower` and `upper` are `null` when fewer than
two independent units make the interval unavailable. This JSON surface remains
locale-independent; only the human-readable table is localized.

## Run receipts and record dumps

Every case run produces a receipt: a schema tag, the mode, the case id, a
SHA-256 `case_hash` of the case's canonical JSON, the `provider_ref`
(`scripted` for replay, `<type>.<alias>:<model>` for live), the sorted effective
`tool_surface`, and a `sandbox` stamp. These fields appear per case in the JSON
report and make runs comparable across time (the baseline workflow builds on
them).

Records can be dumped as JSON:

- `--dump-records <dir>` writes `<dir>/<case_id>.json` (record plus grades) for
  every case. The directory and files are restricted to the current user.
- On every completed run, failed or errored cases are auto-dumped under
  `<install>/eval-artifacts/runs/<run-id>/`. The table footer prints the exact
  directory when any failed-case records exist. The owner-only
  `<install>/eval-artifacts/last-run` pointer names that completed run.

Dumps are debugging artifacts, not fixtures. A live transcript can embed
workspace file content and model output, so **never commit a dump**;
the automatic directory is private runtime state outside the current working
directory. Completed-run publication is serialized across processes: a new run
replaces the pointer atomically, then retires the previous completed run while
leaving other processes' active staging directories alone. Promoting a dump into
a suite fixture requires the same privacy placeholder pass as any other fixture
(see the privacy contract): no real names, transcripts, hostnames, or credentials.

## Case format

Each fixture is an `LlmTrace`: a `model_name`, a list of conversation `turns`
(each with a `user_input` and scripted response `steps`), and declarative
`expects`. A case is either **positive** (a behavior that must happen) or
**negative** (a behavior that must NOT happen, e.g. `tools_not_used`,
`response_not_contains`, `max_tool_calls: 0`). See `evals/README.md` for the
authoring rules, including the two-experts test and the privacy requirement that
fixtures use placeholder identities only.

### Every case must assert something

Fixture loading fails closed when an unknown key, an omitted or empty
expectation block, or a zero-length string expectation would make a case unable
to certify meaningful behavior. One invalid fixture aborts the suite load; it
is never silently skipped. Report aggregation independently requires at least
one grade, so an in-memory caller cannot manufacture a green case from an empty
grade vector.

### Grader catalog

Graders implement the async, workspace-aware `Grader` trait. The runner awaits
them before it drops each case's temporary workspace, allowing later
side-effect graders to inspect real case state. Production builds the catalog
once through `grader::default_graders`; `run_case_with_graders` and
`live::run_live_case_with_graders` accept an injected catalog so tests exercise
that same grade-before-teardown path instead of calling a grader directly.

## Expectations reference

`expects` collects declarative checks. Every field is optional; each declared
check becomes one graded result, tagged with a category (`response`, `tool`,
`side_effect`, `budget`, `judge`, or `config`) surfaced in the JSON report along
with a per-case `score` and `category_totals`. The harness emits a failing
`config` grade only as a runtime backstop when an in-process case bypasses the
fixture loader and declares no effective checks.

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
