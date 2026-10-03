# Testing

ZeroClaw uses a five-level testing taxonomy backed by filesystem layout. Each level has a different boundary and a different cost; pick the lowest level that proves what you need to prove.

When a PR claims behavior that a user directly runs, clicks, sends, installs, or observes, use [User-boundary proof](./user-boundary-proof.md) to identify the smallest test or manual check that reaches that boundary.

## The five levels

| Level | What it tests | Boundary | Where it lives |
|---|---|---|---|
| **Unit** | A single function or struct | Everything mocked | `#[cfg(test)]` blocks in `src/**` or co-located `tests.rs` |
| **Component** | One subsystem inside its own boundary | Subsystem real, everything else mocked | `tests/component/` |
| **Integration** | Multiple internal components wired together | Real internals, external APIs mocked | `tests/integration/` |
| **System** | Full request → response across all internal boundaries | Only external APIs mocked | `tests/system/` |
| **Live** | Full stack with real external services | Nothing mocked, `#[ignore]`'d | `tests/live/` |

Plus two non-test directories:

| Directory | Purpose |
|---|---|
| `tests/manual/` | Human-driven test scripts (shell, Python), run directly, not via cargo |
| `tests/support/` | Shared mock infrastructure, not a test binary, included as `mod support;` from each level |

## Running tests

<div class="os-tabs-src">

#### sh

```sh
cargo test                                  # unit + component + integration + system
cargo test --lib                            # unit only
cargo test --test component                 # component only
cargo test --test integration               # integration only
cargo test --test system                    # system only
cargo test --test live -- --ignored         # live (requires API credentials)
cargo test --test integration agent         # filter within a level
cargo nextest run --locked --workspace --exclude zeroclaw-desktop  # what CI runs
./scripts/ci/parallel_runtime_test_gate.sh  # repeated same-process runtime/channel tests
./dev/ci.sh all                             # full CI battery (Docker)
./dev/ci.sh firmware-protocol               # standalone firmware protocol host gate (Docker)
./dev/ci.sh test-component                  # level-specific CI commands (Docker)
```

</div>

The `firmware-protocol` command checks the standalone
`firmware/zeroclaw-fw-protocol` crate, which is outside the root Cargo
workspace. `scripts/ci/firmware_protocol_gate.sh` is the canonical definition
of its formatting, strict Clippy, and locked-test checks; required CI and the
pre-push hook invoke the same helper.

The parallel runtime gate repeats the complete `zeroclaw-runtime` and
`zeroclaw-channels` library test binaries with 16 harness threads. Running the
whole binaries is intentional: it detects interference between state-mutating
tests and otherwise unrelated agent turns that filtered test runs cannot expose.
Required CI runs this gate in a separate job for changes to either crate, the
workspace dependency manifests, or the gate's own CI files. Other PRs skip it;
pushes to `master` and merge queue runs retain the full regression backstop.
Override repetitions with `ZEROCLAW_PARALLEL_TEST_RUNS` and harness threads
with `ZEROCLAW_PARALLEL_TEST_THREADS`.

## Application acceptance on Linux

`scripts/ci/runtime_acceptance.sh` builds and runs the actual `zeroclaw` and
`zerocode` binaries with locked dependencies, default features, and the `ci`
profile. Install Python 3.9+, tmux, and OpenSSL alongside the normal Rust build
prerequisites, then run:

```sh
scripts/ci/runtime_acceptance.sh --suite core --artifacts-dir /tmp/acceptance-core
scripts/ci/runtime_acceptance.sh --suite full --bin-dir target/x86_64-unknown-linux-gnu/ci --artifacts-dir /tmp/acceptance-full
python3 tests/system/runtime_acceptance/test_contracts.py
python3 tests/system/runtime_acceptance/test_faults.py --bin-dir target/x86_64-unknown-linux-gnu/ci --artifacts-dir /tmp/acceptance-faults
```

`--bin-dir` reuses existing binaries. Each scenario creates its own config,
storage, workspace, socket, and loopback services. Child environments exclude
inherited application settings and credentials. The only fake services are the
external OpenAI-compatible API and OIDC issuer; no local model, API keys, or
production test mode is needed. ZeroCode runs in tmux with actual keyboard
input, rendered-cell assertions, and terminal lifecycle assertions.

The scenario registry in `tests/system/runtime_acceptance/scenarios.py` owns
suite membership:

| Suite | Required application outcomes |
| --- | --- |
| Core | Warm local connection, automatic daemon startup, saved config compatibility, streamed conversation, approved and denied file writes, SOP checkpoint/completion, persisted SOP record and memory after restart, native HTTP pairing and credential rejection. |
| Full | All core scenarios plus verified WSS with client certificates and native/OIDC credentials, invalid OIDC token rejection, live permission changes across RPC and HTTP, and trusted local startup with OIDC configured. |

The saved V3 config fixture represents an installation with no OIDC or user
roster. It is synthetic: the exact historical startup failure revision is not
known, so passing this test does not establish that it reproduces that incident.

When acceptance is selected, the Linux build leg of Quality Gate builds both
applications from the same checked-out revision and immediately runs acceptance
with those binaries. A skipped acceptance suite keeps the existing Cargo build
and does not add ZeroCode compilation.
`scope.py` is the sole selection policy: documentation/metadata-only PRs skip,
ordinary code runs core, and sensitive paths or `risk:high`, `domain:security`,
`priority:p0`, and `priority:p1` labels select full. Unknown or unusable inputs
select full. Merge queue, master pushes, and ordinary manual dispatches run full.
PR updates and changes to the four selection labels reevaluate Quality Gate.
Other label events allocate no runners, use a separate concurrency group, and
cannot cancel active CI or publish a skipped check named `CI Required Gate`.
Missing label-event data fails closed and runs Quality Gate. Acceptance failures
fail the existing build dependency, and `CI Required Gate` rejects skipped builds.

GitHub must filter events before checkout. The job and concurrency expressions
are generated from the canonical labels in `scope.py`; after changing that
policy, run `python3 tests/system/runtime_acceptance/workflow_policy.py --write`.
The selector contract checks their materialized form and actual build arguments.

### Measuring CI cost

Quality Gate has an explicit `acceptance_cost` manual-dispatch input, defaulting
to false. Setting it to true runs only a disposable cost-measurement job. It
cannot satisfy or replace `CI Required Gate`, does not change normal PR coverage,
and never writes shared caches:

```sh
gh workflow run ci.yml --ref <branch> -f acceptance_cost=true
```

The measurement uses the same source revision, Linux x86_64 runner, toolchain,
linker, target path, and initial restored cache for the existing root build and
the proposed two-binary build. It fetches dependencies before timing and restores
the original target snapshot before each build in baseline/candidate/candidate/
baseline order. It then runs core, full, and negative controls using the measured
candidate binaries. Logs, individual timings, means, cache state, revision, and
application results are retained in the `acceptance-cost` artifact for seven days.
The job is bounded to 60 minutes and costs runner time only when requested.

Report elapsed time and summed runner-minutes separately. Include the added
ZeroCode compilation, test execution, artifact overhead, and any extra workflow
runs from risk-label edits. Convert measured usage to money using the project's
confirmed Blacksmith rate and allowances; public pricing cannot establish an
organization's OSS sponsorship or invoice terms.

Core has a five-minute execution budget and full has ten minutes, excluding
compilation. Individual waits are bounded. The full job also proves failure
detection using failed initialization, an incorrect model response, and an
unapproved SOP checkpoint. JSON/JUnit results, commit and binary hashes,
timings, selection reason, sanitized daemon logs, and terminal captures are
uploaded for seven days. Tokens, private keys, configs, and databases are not
uploaded. A selected scenario that never executes fails the run.

Coverage initially targets Linux and default features. Browser UI, other
operating systems, external vendor availability/model quality, and release
package installation need separate coverage. To roll back acceptance, revert
its workflow integration while retaining the general required quality gate.

## Picking a level for a new test

1. Testing one subsystem in isolation? → `tests/component/`
2. Testing multiple components wired together? → `tests/integration/`
3. Testing full message flow end to end? → `tests/system/`
4. Requires real API keys? → `tests/live/` with `#[ignore]`

For Rust tests, add the file to the level's `mod.rs` and use shared infrastructure
from `tests/support/`. Application acceptance uses its Python scenario registry
and runs through the shell entrypoint above, separately from `cargo test`.

## Shared infrastructure

Every test binary includes `mod support;`, making the shared mocks available as `crate::support::*`.

| Module | Contents |
|---|---|
| `mock_model_provider.rs` | `MockModelProvider` (FIFO scripted), `RecordingModelProvider` (captures requests), `TraceLlmModelProvider` (JSON fixture replay) |
| `mock_tools.rs` | `EchoTool`, `CountingTool`, `FailingTool`, `RecordingTool` |
| `mock_channel.rs` | `TestChannel` (captures sends, records typing events) |
| `helpers.rs` | `make_memory()`, `make_observer()`, `build_agent()`, `text_response()`, `tool_response()`, `StaticRecallMemory` |
| `trace.rs` | `LlmTrace`, `TraceTurn`, `TraceStep` types + `LlmTrace::from_file()` |
| `assertions.rs` | `verify_expects()` for declarative trace assertion |

Typical usage:

```rust
use crate::support::{MockModelProvider, EchoTool, CountingTool};
use crate::support::helpers::{build_agent, text_response, tool_response};
```

## JSON trace fixtures

Trace fixtures are canned LLM response scripts stored as JSON files in `tests/fixtures/traces/`. They replace inline mock setup with declarative conversation scripts, much easier to read and edit than `mockall` chains.

How it works:

1. `TraceLlmModelProvider` loads a fixture and implements the `ModelProvider` trait.
2. Each `provider.chat()` call returns the next step from the fixture in FIFO order.
3. Real tools execute normally (`EchoTool` actually processes its arguments).
4. After all turns, `verify_expects()` checks declarative assertions.
5. If the agent calls the provider more times than there are steps, the test fails.

Fixture format:

```json
{
  "model_name": "test-name",
  "turns": [
    {
      "user_input": "User message",
      "steps": [
        {
          "response": {
            "type": "text",
            "content": "LLM response",
            "input_tokens": 20,
            "output_tokens": 10
          }
        }
      ]
    }
  ],
  "expects": {
    "response_contains": ["expected text"],
    "tools_used": ["echo"],
    "max_tool_calls": 1
  }
}
```

Response types: `"text"` (plain text) or `"tool_calls"` (LLM requests tool execution).

Expects fields: `response_contains`, `response_not_contains`, `tools_used`, `tools_not_used`, `max_tool_calls`, `all_tools_succeeded`, `response_matches` (regex).

## Live test conventions

Live tests hit real external services and cost real money; they are `#[ignore]` by default and only run with explicit opt-in.

- Always `#[ignore]`. Never let a live test run on a normal `cargo test`.
- Read credentials from `env::var("ZEROCLAW_TEST_*")`. Don't read the operator's config; live tests should be hermetic.
- Run with `cargo test --test live -- --ignored --nocapture`.

## Database tests are integration tests

Don't mock SQLite for tests that exercise schema or SQL; integration tests must hit a real database. The mock-passes-but-prod-fails class of bug is real and we've eaten it before.

## Manual tests

`tests/manual/` holds scripts for human-driven testing that can't be automated via `cargo test`. Run them directly. Channel-specific manual smoke tests live under `tests/manual/<channel>/`.
