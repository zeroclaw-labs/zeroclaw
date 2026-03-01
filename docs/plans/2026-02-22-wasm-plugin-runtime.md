# WASM Plugin Runtime Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan
> task-by-task.

**Goal:** Build a WASI Preview 2 + WIT plugin runtime that supports hook/tool/provider plugins, new
hook points, ObserverBridge, and hot-reload with safe fallback.

**Architecture:** Add a capability-segmented plugin subsystem (`src/plugins/**`) and route
hook/tool/provider dispatch through immutable plugin snapshots. Keep native implementations intact
as fallback. Enforce deny-by-default capability policy with host-side limits and deterministic
modifying-hook ordering.

**Tech Stack:** Rust, Tokio, Wasmtime (component model), WASI Preview 2, WIT, serde, notify,
existing ZeroClaw traits/factories.

---

## Task 1: Add plugin config schema and defaults

**Files:**

- Modify: `src/config/schema.rs`
- Modify: `src/config/mod.rs`
- Test: `src/config/schema.rs` (inline tests)

- Step 1: Write the failing test

```rust
#[test]
fn plugins_config_defaults_safe() {
    let cfg = HooksConfig::default();
    // replace with PluginConfig once added
    assert!(cfg.enabled);
}
```

- Step 2: Run test to verify it fails Run: `cargo test --locked config::schema -- --nocapture`
Expected: FAIL because `PluginsConfig` fields/assertions do not exist yet.

- Step 3: Write minimal implementation

- Add `PluginsConfig` with:
    - `enabled: bool`
    - `dirs: Vec<String>`
    - `hot_reload: bool`
    - `limits` (timeout/memory/concurrency)
    - capability allow/deny lists
- Add defaults: disabled-by-default runtime loading, deny-by-default capabilities.

- Step 4: Run test to verify it passes Run: `cargo test --locked config::schema -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/config/schema.rs src/config/mod.rs
git commit -m "feat(config): add plugin runtime config schema"
```

## Task 2: Scaffold plugin subsystem modules

**Files:**

- Create: `src/plugins/mod.rs`
- Create: `src/plugins/traits.rs`
- Create: `src/plugins/manifest.rs`
- Create: `src/plugins/runtime.rs`
- Create: `src/plugins/registry.rs`
- Create: `src/plugins/hot_reload.rs`
- Create: `src/plugins/bridge/mod.rs`
- Create: `src/plugins/bridge/observer.rs`
- Modify: `src/lib.rs`
- Test: inline tests in new modules

- Step 1: Write the failing test

```rust
#[test]
fn plugin_registry_empty_by_default() {
    let reg = PluginRegistry::default();
    assert!(reg.hooks().is_empty());
}
```

- Step 2: Run test to verify it fails Run: `cargo test --locked plugins:: -- --nocapture`
Expected: FAIL because modules/types do not exist.

- Step 3: Write minimal implementation

- Add module exports and basic structs/enums.
- Keep runtime no-op while preserving compile-time interfaces.

- Step 4: Run test to verify it passes Run: `cargo test --locked plugins:: -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/plugins src/lib.rs
git commit -m "feat(plugins): scaffold plugin subsystem modules"
```

## Task 3: Add WIT capability contracts and ABI version checks

**Files:**

- Create: `wit/zeroclaw/hooks/v1/*.wit`
- Create: `wit/zeroclaw/tools/v1/*.wit`
- Create: `wit/zeroclaw/providers/v1/*.wit`
- Modify: `src/plugins/manifest.rs`
- Test: `src/plugins/manifest.rs` inline tests

- Step 1: Write the failing test

```rust
#[test]
fn manifest_rejects_incompatible_wit_major() {
    let m = PluginManifest { wit_package: "zeroclaw:hooks@2.0.0".into(), ..Default::default() };
    assert!(validate_manifest(&m).is_err());
}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked manifest_rejects_incompatible_wit_major -- --nocapture` Expected: FAIL before
validator exists.

- Step 3: Write minimal implementation

- Add WIT package declarations and version policy parser.
- Validate major compatibility per capability package.

- Step 4: Run test to verify it passes Run:
`cargo test --locked manifest_rejects_incompatible_wit_major -- --nocapture` Expected: PASS.

- Step 5: Commit

```bash
git add wit src/plugins/manifest.rs
git commit -m "feat(plugins): add wit contracts and abi compatibility checks"
```

## Task 4: Hook runtime integration and missing lifecycle wiring

**Files:**

- Modify: `src/hooks/traits.rs`
- Modify: `src/hooks/runner.rs`
- Modify: `src/gateway/mod.rs`
- Modify: `src/agent/loop_.rs`
- Modify: `src/channels/mod.rs`
- Test: inline tests in `src/hooks/runner.rs`, `src/agent/loop_.rs`

- Step 1: Write the failing test

```rust
#[tokio::test]
async fn fire_gateway_stop_is_called_on_shutdown_path() {
    // assert hook observed stop signal
}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked fire_gateway_stop_is_called_on_shutdown_path -- --nocapture` Expected: FAIL due
to missing call site.

- Step 3: Write minimal implementation

- Add hook events: `BeforeCompaction`, `AfterCompaction`, `ToolResultPersist`.
- Wire `fire_gateway_stop` in graceful shutdown path.
- Trigger compaction hooks around compaction flows.

- Step 4: Run test to verify it passes Run: `cargo test --locked hooks::runner -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/hooks src/gateway/mod.rs src/agent/loop_.rs src/channels/mod.rs
git commit -m "feat(hooks): add compaction/persist hooks and gateway stop lifecycle wiring"
```

## Task 5: Implement built-in `session_memory` and `boot_script` hooks

**Files:**

- Create: `src/hooks/builtin/session_memory.rs`
- Create: `src/hooks/builtin/boot_script.rs`
- Modify: `src/hooks/builtin/mod.rs`
- Modify: `src/config/schema.rs`
- Modify: `src/agent/loop_.rs`
- Modify: `src/channels/mod.rs`
- Test: inline tests in new builtins

- Step 1: Write the failing test

```rust
#[tokio::test]
async fn session_memory_hook_persists_and_recalls_expected_context() {}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked session_memory_hook -- --nocapture` Expected: FAIL before hook exists.

- Step 3: Write minimal implementation

- Register both built-ins through `HookRunner` initialization paths.
- `session_memory`: persist/retrieve session-scoped summaries.
- `boot_script`: mutate prompt/context at startup/session begin.

- Step 4: Run test to verify it passes Run: `cargo test --locked hooks::builtin -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/hooks/builtin src/config/schema.rs src/agent/loop_.rs src/channels/mod.rs
git commit -m "feat(hooks): add session_memory and boot_script built-in hooks"
```

## Task 6: Add plugin tool registration and execution routing

**Files:**

- Modify: `src/tools/mod.rs`
- Modify: `src/tools/traits.rs`
- Modify: `src/agent/loop_.rs`
- Modify: `src/plugins/registry.rs`
- Modify: `src/plugins/runtime.rs`
- Test: `src/agent/loop_.rs` inline tests, `src/tools/mod.rs` tests

- Step 1: Write the failing test

```rust
#[tokio::test]
async fn plugin_tool_spec_is_visible_and_executable() {}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked plugin_tool_spec_is_visible_and_executable -- --nocapture` Expected: FAIL
before routing exists.

- Step 3: Write minimal implementation

- Merge plugin tool specs with native specs.
- Route execution by owner.
- Keep host security checks before plugin invocation.
- Apply `ToolResultPersist` before persistence/feedback.

- Step 4: Run test to verify it passes Run: `cargo test --locked agent::loop_ -- --nocapture`
Expected: PASS for plugin tool tests.

- Step 5: Commit

```bash
git add src/tools/mod.rs src/tools/traits.rs src/agent/loop_.rs src/plugins/registry.rs src/plugins/runtime.rs
git commit -m "feat(tools): support wasm plugin tool registration and execution"
```

## Task 7: Add plugin provider registration and factory integration

**Files:**

- Modify: `src/providers/mod.rs`
- Modify: `src/providers/traits.rs`
- Modify: `src/plugins/registry.rs`
- Modify: `src/plugins/runtime.rs`
- Test: `src/providers/mod.rs` inline tests

- Step 1: Write the failing test

```rust
#[test]
fn factory_can_create_plugin_provider() {}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked factory_can_create_plugin_provider -- --nocapture` Expected: FAIL before plugin
provider lookup exists.

- Step 3: Write minimal implementation

- Extend provider factory to resolve plugin providers after native map.
- Ensure resilient/routed providers can wrap plugin providers.

- Step 4: Run test to verify it passes Run: `cargo test --locked providers::mod -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/providers/mod.rs src/providers/traits.rs src/plugins/registry.rs src/plugins/runtime.rs
git commit -m "feat(providers): integrate wasm plugin providers into factory and routing"
```

## Task 8: Implement ObserverBridge

**Files:**

- Modify: `src/plugins/bridge/observer.rs`
- Modify: `src/observability/mod.rs`
- Modify: `src/agent/loop_.rs`
- Modify: `src/gateway/mod.rs`
- Test: `src/plugins/bridge/observer.rs` inline tests

- Step 1: Write the failing test

```rust
#[test]
fn observer_bridge_emits_hook_events_for_legacy_observer_stream() {}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked observer_bridge_emits_hook_events_for_legacy_observer_stream -- --nocapture`
Expected: FAIL before bridge wiring.

- Step 3: Write minimal implementation

- Implement adapter mapping observer events into hook dispatch.
- Wire where observer is created in agent/channel/gateway flows.

- Step 4: Run test to verify it passes Run: `cargo test --locked plugins::bridge -- --nocapture`
Expected: PASS.

- Step 5: Commit

```bash
git add src/plugins/bridge/observer.rs src/observability/mod.rs src/agent/loop_.rs src/gateway/mod.rs
git commit -m "feat(observability): add observer-to-hook bridge for plugin runtime"
```

## Task 9: Implement hot reload with immutable snapshots

**Files:**

- Modify: `src/plugins/hot_reload.rs`
- Modify: `src/plugins/registry.rs`
- Modify: `src/plugins/runtime.rs`
- Modify: `src/main.rs`
- Test: `src/plugins/hot_reload.rs` inline tests

- Step 1: Write the failing test

```rust
#[tokio::test]
async fn reload_failure_keeps_previous_snapshot_active() {}
```

- Step 2: Run test to verify it fails Run:
`cargo test --locked reload_failure_keeps_previous_snapshot_active -- --nocapture` Expected: FAIL
before atomic swap logic.

- Step 3: Write minimal implementation

- File watcher rebuilds candidate snapshot.
- Validate fully before publish.
- Atomic swap on success; rollback on failure.
- Preserve in-flight snapshot handles.

- Step 4: Run test to verify it passes Run:
`cargo test --locked plugins::hot_reload -- --nocapture` Expected: PASS.

- Step 5: Commit

```bash
git add src/plugins/hot_reload.rs src/plugins/registry.rs src/plugins/runtime.rs src/main.rs
git commit -m "feat(plugins): add safe hot-reload with immutable snapshot swap"
```

## Task 10: Documentation and verification pass

**Files:**

- Create: `docs/plugins-runtime.md`
- Modify: `docs/config-reference.md`
- Modify: `docs/commands-reference.md`
- Modify: `docs/troubleshooting.md`
- Modify: locale docs where equivalents exist (`fr`, `vi` minimum for
  config/commands/troubleshooting)

- Step 1: Write the failing doc checks

- Define link/consistency checks and navigation parity expectations.

- Step 2: Run doc checks to verify failures (if stale links exist) Run: project markdown/link
checks used in repo CI. Expected: potential FAIL until docs updated.

- Step 3: Write minimal documentation updates

- Plugin config keys, lifecycle, safety model, hot reload behavior, operator troubleshooting.

- Step 4: Run full validation Run:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

Expected: PASS.

- Step 5: Commit

```bash
git add docs src
git commit -m "docs(plugins): document wasm plugin runtime config lifecycle and operations"
```

## Final Integration Checklist

- Ensure plugins disabled mode preserves existing behavior.
- Ensure security defaults remain deny-by-default.
- Ensure hook ordering and cancellation semantics are deterministic.
- Ensure provider/tool fallback behavior is unchanged for native implementations.
- Ensure hot-reload failures are non-fatal and reversible.
