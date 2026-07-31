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
| Shell / subprocesses | If `shell` is in the effective tool set, its subprocesses are wrapped in a real OS sandbox backend (Landlock on Linux with the `sandbox-landlock` feature, Firejail on Linux otherwise, `sandbox-exec` on macOS). If no real backend is available on the platform, the case fails closed before the agent runs (a clear error naming the missing backend) rather than running `shell` unsandboxed. |
| Autonomy | `Supervised`, never `Full`. |
| Approvals | Non-interactive backchannel manager: allowlisted tools auto-approve; anything else that reaches the approval gate is auto-denied (deterministic case failure). |
| Timeout | Each turn is bounded by `[eval].case_timeout_secs` (default 120); a slow turn fails the case rather than hanging. |

### Residual host access

The shell sandbox confines subprocesses; it does not grant an airtight boundary,
and operators relying on it should know exactly what it still permits.

- Linux, Landlock (`sandbox-landlock` feature): the child process's filesystem
  access is confined to the case workspace plus a blanket `/tmp` allowance,
  with `/usr` and `/bin` readable (for executing commands). Within the
  workspace and `/tmp`, the child may read and write existing files, list
  directories, and create or remove regular files, directories, and symlinks
  (`MakeReg`, `MakeDir`, `RemoveFile`, `RemoveDir`, `MakeSym`); creating device
  nodes, sockets, or named pipes stays denied everywhere, workspace included.
  Outside the workspace and `/tmp` (and the read-only `/usr`, `/bin`), all of
  these operations, including plain file creation, are denied. Network is NOT
  restricted by Landlock as configured here (no `AccessNet` rule); a sandboxed
  shell command can still reach the network freely. Because each case's own
  workspace is itself created under `/tmp`, the effective write boundary is
  "workspace plus the system temp tree", not strict workspace-only
  confinement.
- macOS, `sandbox-exec` (Seatbelt): deny-by-default policy. Writes are allowed
  to the workspace, `/tmp`, `/private/tmp`, and `/private/var/folders/**`.
  Reads are allowed more broadly: the write locations above, plus system
  paths (`/usr`, `/bin`, `/sbin`, `/Library`, `/System`, `/etc`, `/opt`, and
  others) and the invoking user's dotfile directories under `$HOME`. Network
  is denied by default except DNS resolution (via `mDNSResponder`) and
  localhost connections.
- No backend available (Windows; Linux without `sandbox-landlock` or
  Firejail installed): a live case that requests `shell` errors before the
  agent runs at all (fail closed), rather than falling back to an
  unsandboxed shell.
- Linux network confinement (blocking outbound network entirely, not just
  filesystem access) needs Landlock ABI v4 (`AccessNet`, kernel 6.7+) and is
  a known follow-up, not implemented by the current sandbox construction.

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
