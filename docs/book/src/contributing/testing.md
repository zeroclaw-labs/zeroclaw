# Testing

ZeroClaw uses a five-level testing taxonomy backed by filesystem layout. Each level has a different boundary and a different cost — pick the lowest level that proves what you need to prove.

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
| `tests/manual/` | Human-driven test scripts (shell, Python) — run directly, not via cargo |
| `tests/support/` | Shared mock infrastructure — not a test binary, included as `mod support;` from each level |

## Running tests

```bash
cargo test                                  # unit + component + integration + system
cargo test --lib                            # unit only
cargo test --test component                 # component only
cargo test --test integration               # integration only
cargo test --test system                    # system only
cargo test --test live -- --ignored         # live (requires API credentials)
cargo test --test integration agent         # filter within a level
./dev/ci.sh all                             # full CI battery
./dev/ci.sh test-component                  # level-specific CI commands
```

## Picking a level for a new test

1. Testing one subsystem in isolation? → `tests/component/`
2. Testing multiple components wired together? → `tests/integration/`
3. Testing full message flow end to end? → `tests/system/`
4. Requires real API keys? → `tests/live/` with `#[ignore]`

After creating the file, add it to the level's `mod.rs` and use shared infrastructure from `tests/support/`.

## Shared infrastructure

Every test binary includes `mod support;`, making the shared mocks available as `crate::support::*`.

| Module | Contents |
|---|---|
| `mock_provider.rs` | `MockProvider` (FIFO scripted), `RecordingProvider` (captures requests), `TraceLlmProvider` (JSON fixture replay) |
| `mock_tools.rs` | `EchoTool`, `CountingTool`, `FailingTool`, `RecordingTool` |
| `mock_channel.rs` | `TestChannel` (captures sends, records typing events) |
| `helpers.rs` | `make_memory()`, `make_observer()`, `build_agent()`, `text_response()`, `tool_response()`, `StaticMemoryLoader` |
| `trace.rs` | `LlmTrace`, `TraceTurn`, `TraceStep` types + `LlmTrace::from_file()` |
| `assertions.rs` | `verify_expects()` for declarative trace assertion |

Typical usage:

```rust
use crate::support::{MockProvider, EchoTool, CountingTool};
use crate::support::helpers::{build_agent, text_response, tool_response};
```

## JSON trace fixtures

Trace fixtures are canned LLM response scripts stored as JSON files in `tests/fixtures/traces/`. They replace inline mock setup with declarative conversation scripts — much easier to read and edit than `mockall` chains.

How it works:

1. `TraceLlmProvider` loads a fixture and implements the `Provider` trait.
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

Live tests hit real external services and cost real money — they are `#[ignore]` by default and only run with explicit opt-in.

- Always `#[ignore]`. Never let a live test run on a normal `cargo test`.
- Read credentials from `env::var("ZEROCLAW_TEST_*")`. Don't read from `~/.zeroclaw/config.toml` — live tests should be hermetic.
- Run with `cargo test --test live -- --ignored --nocapture`.

## Database tests are integration tests

Don't mock SQLite for tests that exercise schema or SQL — integration tests must hit a real database. The mock-passes-but-prod-fails class of bug is real and we've eaten it before.

## Manual tests

`tests/manual/` holds scripts for human-driven testing that can't be automated via `cargo test`. Run them directly. Channel-specific manual smoke tests live under `tests/manual/<channel>/`.
