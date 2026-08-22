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
and `[eval].live_allowed_tools` both request it: `effective_live_tools`
(`crates/zeroclaw-eval/src/live.rs`) applies a hard denylist to the allowlist
intersection, so deny always wins. Because `shell` is absent from the effective
allowlist, a scripted `shell` call in a live case is stopped earlier than dispatch:
the non-interactive approval gate denies it (it is not auto-approved and cannot be
prompted for), and the denial is recorded in conversation history rather than the
tool ever running.

This is the ship-safe interim posture. An earlier version of this harness
wrapped `shell`'s subprocesses in a real OS
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
part of the conversation sent to the configured provider, a confidentiality
boundary, not just an integrity one. Re-admitting `shell` needs an
eval-specific sandbox contract that also denies sensitive host reads on every
accepted backend; that is a deliberate, tracked follow-up, not implemented
here. `live_shell_sandbox`/`ensure_real_sandbox` (the OS-sandbox construction
`shell` used to run under) remain in `live.rs` as building blocks for it.

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

### Every case must assert something

The loader fails closed on fixtures that cannot produce a grade, because a case
that asserts nothing still reports green: the gate would signal success in
exactly the situation it exists to catch. Three rules enforce this:

1. **Unknown keys are load errors.** Every case struct is
   `deny_unknown_fields`, so `respose_contains` (one transposed character) is a
   parse failure naming the offending key, not an ignored key that silently
   empties the expectation block.
2. **A no-op expectation block is a load error.** If every list in `expects` is
   empty and every optional bound is unset, including when `expects` is omitted
   entirely, `LlmTrace::from_file` rejects the fixture. A case that genuinely
   asserts nothing must say so with `"allow_no_expectations": true`, so the
   smoke-case contract is declared rather than stumbled into. One bad fixture
   fails the whole suite load; suites never silently skip a case.
3. **Zero grades is never a pass.** `CaseReport::passed` requires a non-empty
   grade vector (`[].iter().all(..)` is `true`, which is the bug). Every case
   gets a `run_completed` grade from the default grader catalog, so a declared
   smoke case still reports one honest result and "zero grades" always means
   "nothing was checked".

### Grader catalog

Graders implement the async `Grader` trait and are run by the runner while the
case workspace is still on disk, that ordering is the whole reason the trait is
async. `run_case` delegates to `run_case_with_graders(trace, deps, graders)`
(and `live::run_live_case` to `live::run_live_case_with_graders`), passing
`grader::default_graders`. The seam exists so the grade-before-teardown contract
can be tested from inside a real run rather than by calling `Grader::grade`
directly, which would not observe the runner's ordering at all.
