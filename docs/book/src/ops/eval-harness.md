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
| `replay` | Replays scripted LLM responses from the fixture through the agent loop. Fully deterministic, no network. Cases that declare memory setup or expectations are rejected; use `--mode live` for those cases. | Free | Gated (default) |
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
declare `tools` it needs. Its `setup` can seed workspace files with
`workspace_files` and long-term memory with a `memory` map of key to content. The
harness stores each seed as an unscoped Core memory entry before the first turn.
Memory keys must be non-empty relative paths without `..`, and every character
must match the provider-safe `[A-Za-z0-9._/-]` grammar. Unlike values, raw keys
can appear in provider-visible context, so whitespace, control characters, and
other punctuation are rejected before provider construction.

Seeds pass through the production memory content scanner before persistence.
Flagged secret-like content or unsafe instructions fail the case before the live
provider is constructed. Eval fixtures are durable repository content and may be
sent to an external provider through normal memory context, so use synthetic
placeholders and never put real secrets or credentials in a seed.

The normal turn-memory policy can automatically recall relevant seeded entries
into the prompt context. Automatic recall uses the raw user input and the normal
relevance and context-budget filters, so it is not an exact-key guarantee.
`memory_recall` remains available as an explicit retrieval surface when a case
specifically needs or asserts tool-driven recall.

The requested tools are intersected with `[eval].live_allowed_tools`; the default
(empty) allows no real tools, so a case that needs tools requires the operator to
opt in explicitly. This rule also applies to `memory_store`, `memory_recall`,
`memory_forget`, `memory_export`, and `memory_purge`: a memory tool is available
only when both the case's `tools` list and the operator's live allowlist name it.
Declaring `setup.memory` or `expects.memory` does not grant a memory tool. For
example, a case that asserts tool-driven retrieval needs `memory_recall` in both
places.

Each live case runs inside a constrained execution envelope:

| Control | Behavior |
|---|---|
| Workspace | Fresh per-case temp directory; `workspace_only` policy blocks reads and writes outside it. |
| Tool registry | Runtime default and memory tools filtered to `case.tools` intersected with `[eval].live_allowed_tools`, then `shell` is dropped unconditionally (see "Shell is excluded" below); empty allowlist yields only the harmless echo tool. |
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

### Memory is isolated

Cases that declare memory setup or expectations, or that receive an allowlisted
memory tool, use a fresh SQLite memory backend at `memory/brain.db` under the case
workspace. The database never uses the operator's configured long-term-memory
store and disappears with the per-case temporary directory. The `memory/`
subtree is harness-owned, so workspace expectations should avoid asserting on
paths inside it; use `expects.memory` for memory state instead.

Because live output is non-deterministic and can embed workspace content, live runs
belong in the planned `evals/live/` suite, not the gating regression suite.

## Exit-code contract

`zeroclaw eval run` exits `0` iff every case passed, and `1` otherwise (any
failed check or run error). This is the CI gate: the process exit code is the
signal. The same decision is exposed as the pure function
`SuiteReport::exit_code()` so it can be tested at its real boundary.

## Case format

Each fixture is an `LlmTrace` with a `model_name`, a list of conversation `turns`,
and declarative `expects`. A replay turn includes `user_input` and scripted
response `steps`; a live turn includes `user_input` but omits `steps`. Live cases
can also request `tools` and declare setup maps under `setup.workspace_files` and
`setup.memory`. Replay rejects a non-empty `setup.memory` map or any
`expects.memory` block and reports that the case requires `--mode live`.

A case is either **positive** (a behavior that must happen) or **negative** (a
behavior that must NOT happen, e.g. `tools_not_used`, `response_not_contains`,
`max_tool_calls: 0`). See `evals/README.md` for the authoring rules, including the
two-experts test and the privacy requirement that fixtures use placeholder
identities only.

Example live case with isolated memory setup and checks:

```json
{
  "model_name": "recalls-project-status",
  "id": "gh1234_memory_recall",
  "tools": ["memory_recall"],
  "setup": {
    "memory": { "project/status": "The synthetic project status is green." }
  },
  "turns": [{
    "user_input": "Use memory_recall to retrieve the project status, even if it appears in context."
  }],
  "expects": {
    "response_contains": ["green"],
    "tools_used": ["memory_recall"],
    "memory": {
      "present": ["project/status"],
      "contains": { "project/status": ["synthetic", "green"] }
    }
  }
}
```

The operator must also include `memory_recall` in
`[eval].live_allowed_tools` for this case.

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

Memory checks (category `side_effect`), under `memory`:

- `present` / `absent`: exact keys that must / must not exist after the run.
- `contains`: a map of exact key to substrings that must appear in that entry's
  content. Each map value must contain at least one substring; an empty list is
  a failed malformed expectation rather than a vacuous pass.

Memory checks query the case's isolated memory backend by exact key rather than
using ranked recall. Each key must satisfy the same `[A-Za-z0-9._/-]` grammar as
seed keys and is validated before the backend is queried; an invalid key is a
failed check, never a memory access.
Memory expectations are live-mode only; replay rejects their declaration and
names `--mode live` in the error.

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
