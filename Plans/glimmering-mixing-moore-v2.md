# Conscience + Loop Refactor — v2 (Re-Scoped 2026-05-28)

> Supersedes `Plans/glimmering-mixing-moore.md`. The v1 plan was written when the
> codebase looked different — several of its steps have shipped, the surrounding
> APIs have grown, and the `run_tool_call_loop` parameter count has roughly
> doubled. This document audits what's done and re-scopes what's left.

## Status of the v1 ten-step plan

| v1 step | Description | Status |
|---|---|---|
| 1 | `LoopContext` struct to replace 18-arg loop signature | **Pending.** Signature is now **29 args** at `crates/zeroclaw-runtime/src/agent/loop_.rs:1281`. Refactor scope is larger than v1 assumed. |
| 2 | Extract conscience logic to `evaluate_tool_call` | **Done.** `src/conscience/gate.rs:160`. |
| 3 | Handle `GateVerdict::Ask` at the loop call site | **Blocked / N/A.** The loop **doesn't call the gate at all yet** — there is no call site to extend. Becomes part of the new Step 1 below. |
| 4 | Wire norms from config + `NormConfig` type | **Half done.** `NormConfig` exists at `src/conscience/types.rs:103`, `IntegrityLedger.evolved_norms` exists at `src/conscience/ledger.rs:39`. **What's missing:** `ConscienceConfig` does not yet carry `default_norms: Vec<NormConfig>`, and `evaluate_tool_call` is not yet called with norms sourced from config. |
| 5 | `ConscienceConfig::validate` | **Done.** PR #7 (2026-05-28). |
| 6 | `process_message` loads continuity from disk | **Pending.** `process_message` at `loop_.rs:4198` does not import or call `continuity::load_narrative` / `continuity::load_preferences`. |
| 7 | `sanitize_tool_name` | **Done.** `src/continuity/extraction.rs:3`. |
| 8 | Wire `IntegrityLedger` to gate | **Pending.** `IntegrityLedger` exists at `src/conscience/ledger.rs`; no caller of `evaluate_tool_call` constructs a `SelfState` from it. Becomes part of the new Step 1. |
| 9 | Tests | **Partly done.** Validation tests landed in PR #7; decay-boundary test still pending. |
| 10 | Final verification | Apply at the end of the v2 plan. |

## What's actually left — the v2 plan

Two delivery slices. **Slice A** (call-site wiring) is the headline gap — the gate is fully implemented but inert because nothing invokes it. **Slice B** (loop refactor + persistence) cleans up the surrounding signature debt now that the call site is settled.

### Slice A — Wire the conscience gate (smaller; can ship first)

**Goal:** `evaluate_tool_call` runs on every LLM-issued tool call, with norms read from config, an `IntegrityLedger`-backed `SelfState`, and proper handling for `Allow` / `Ask` / `Block` / `Revise`. Gated by `config.conscience.gate_enabled` (default `false`).

**Architectural finding (2026-05-28):** `run_tool_call_loop` lives in `crates/zeroclaw-runtime`, the conscience module lives in `src/conscience/` in the binary tree, and the runtime crate cannot depend on binary-local code. Wiring the gate requires one of:

1. **Move the conscience module into a new `zeroclaw-conscience` crate** that the runtime depends on (cleanest, most invasive).
2. **Add an Agent builder extension point** for "extra hooks" — the X0 binary builds a `ConscienceHook: HookHandler` and injects it. Agent::new currently constructs the HookRunner inline at `crates/zeroclaw-runtime/src/agent/agent.rs:1079` with no extension slot. The conscience hook's `before_tool_call` would call `conscience::gate::evaluate_tool_call` and return `HookResult::Cancel` for Block/Ask/Revise verdicts. Smallest API change but still needs an architectural decision about where extras hook in.
3. **Define an abstract `ConscienceGateFn` callback type in `zeroclaw-api`** and add a 30th parameter to `run_tool_call_loop`. Smallest call-site touch but compounds the param-surface debt the loop is already in.

**Shipped so far (post-PR with this plan):**
- `ObserverEvent::ConscienceVerdict { tool, verdict, score }` variant with all observer impls updated (log emits structured event; otel + prometheus no-op until backend mapping lands).
- `ConscienceConfig.default_norms: Vec<NormConfigSerde>` with four shipped defaults (`no_rm_rf_root`, `no_rm_rf_home`, `no_drop_table`, `no_curl_pipe_sh`). The serde mirror types (`NormConfigSerde`, `NormActionSerde`) decouple `zeroclaw-config` from the binary-local conscience types; the binary's gate adapter copies values at startup.
- Validation that the default norms list is non-empty and that severities are in `[0, 1]`.
- TOML round-trip test confirms operator-edited norms survive deserialise + reserialise.

**Deferred (need the architectural decision above):**

**Files:**

1. `crates/zeroclaw-config/src/x0_extensions.rs` — extend `ConscienceConfig`:
   ```rust
   #[serde(default = "default_conscience_norms")]
   pub default_norms: Vec<crate::NormConfig>,  // re-export from src/conscience/types
   ```
   Provide a `default_conscience_norms()` shipping the obvious universals (`rm -rf`, `drop table`, …).
2. `crates/zeroclaw-runtime/src/agent/loop_.rs` — in `run_tool_call_loop`, around the existing per-tool dispatch (currently right before `execute_tool` is invoked):
   ```rust
   #[cfg(feature = "x0-extended")]
   if cfg.conscience.gate_enabled {
       let self_state = integrity_ledger
           .as_ref()
           .map(|l| l.lock().to_self_state())
           .unwrap_or_default();
       let verdict = conscience::gate::evaluate_tool_call(
           &call.name,
           /* tool_affinity */ None,
           /* harm_estimate */ harm_estimate_for(&call.name),
           &self_state,
           &cfg.conscience.default_norms,
           &cfg.conscience,
       );
       match verdict {
           GateVerdict::Allow => {},
           GateVerdict::Block => { /* push <tool_result> blocked; record violation; continue; */ },
           GateVerdict::Ask   => { /* push <tool_result> awaiting-review; continue; */ },
           GateVerdict::Revise => { /* push <tool_result> revise; continue; */ },
       }
   }
   ```
3. `crates/zeroclaw-runtime/src/agent/agent.rs` — construct an `Arc<Mutex<IntegrityLedger>>` once per agent when `conscience.gate_enabled`, thread it down through the loop call sites (4 of them: `agent_turn`, `process_message` main path, `process_message` recovery path, `channels/mod.rs` handler).
4. `crates/zeroclaw-runtime/src/observability/traits.rs` — add `ObserverEvent::ConscienceVerdict { tool, verdict, score }`; update each observer impl (`prometheus`, `otel`, `log`) with a no-op match arm so `-D warnings` stays clean.
5. Tests in `src/conscience/tests.rs`:
   - `gate_blocks_when_block_threshold_exceeded`
   - `gate_asks_for_unknown_tool_with_default_norms`
   - `ledger_violation_recorded_on_block`

**Estimated size:** ~250 lines of code + ~120 lines of tests across 5 files. **Risk: low** — entirely gated by `gate_enabled = false` so it can soak.

### Slice B — Reduce `run_tool_call_loop` parameter surface

**Goal:** Bring the loop's 29 positional args under a single `LoopContext` so adding more cross-cutting concerns (the conscience ledger above; per-bot rate-limit hooks from the binary-seeking-umbrella plan) doesn't require touching every call site.

This is the v1 plan's Step 1, but the count is now 29, not 18. The mechanical refactor itself is still safe; the surrounding test suite (~1862 runtime tests) is the safety net.

**Approach:** Two commits.

1. **Mechanical introduction.** Define `LoopContext` in `loop_.rs` with one field per current argument. Build it inside each caller (`agent_turn`, `process_message` ×2, `channels::mod.rs`, plus the in-loop recursive call). New signature is `pub async fn run_tool_call_loop(ctx: LoopContext<'_>) -> Result<String>`. No behaviour change.
2. **Add cross-cutting fields.** Add `integrity_ledger: Option<&Mutex<IntegrityLedger>>` (consumed by Slice A) and `auto_snapshot: Option<&ShadowSnapshot>` (consumed by Task #17 below). Update Slice A and Task #17 wiring to read from `ctx` instead of separate parameters.

**Estimated size:** ~600 lines of pure-refactor diff across 6 files. **Risk: medium** — large rebase footprint, but no behaviour change in commit 1 so bisection is trivial if a regression slips in.

### `process_message` continuity persistence (formerly v1 Step 6)

Hoist the `narrative_store` / `preference_model` load block from `Agent::run` (loop_.rs lines ~1491-1548) into a shared helper, then call it from `process_message`. Currently `process_message` constructs a fresh `NarrativeStore` and `PreferenceModel` per invocation, throwing away every prior preference the agent learned.

Independent of Slices A and B; can ship as a separate small PR.

## Order of merge

1. This v2 plan (this file)
2. Slice A (gate wiring) — smallest, ships value
3. `process_message` continuity persistence — small, independent
4. Slice B commit 1 (mechanical `LoopContext` refactor)
5. Slice B commit 2 (cross-cutting fields, swap Slice A and snapshot wiring to read from ctx)

## Verification (same as v1 Step 10)

```
cargo fmt --all -- --check
cargo clippy --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings
cargo test --features ci-all
cargo test --features ci-all,x0-extended    # for the gate-wired tests
```

Expected: 912+ tests pass, 0 clippy warnings under `-D warnings`, fmt clean.
