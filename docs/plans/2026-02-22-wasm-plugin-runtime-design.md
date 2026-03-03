# WASM Plugin Runtime Design (Capability-Segmented, WASI Preview 2)

## Context

ZeroClaw currently uses in-process trait/factory extension points for providers, tools, channels, memory, runtime adapters, observers, peripherals, and hooks. Hook interfaces exist, but several lifecycle events are either missing or not wired in runtime paths.

## Objective

Design and implement a production-safe system WASM plugin runtime that supports:
- hook plugins
- tool plugins
- provider plugins
- `BeforeCompaction` / `AfterCompaction` hook points
- `ToolResultPersist` modifying hook
- `ObserverBridge` (legacy observer -> hook adapter)
- `fire_gateway_stop` runtime wiring
- built-in `session_memory` and `boot_script` hooks
- hot-reload without service restart

## Chosen Direction

Capability-segmented plugin API on WASI Preview 2 + WIT.

Why:
- cleaner authoring surface than a monolithic plugin ABI
- stronger permission boundaries per capability
- easier long-term compatibility/versioning
- lower blast radius for failures and upgrades

## Architecture

### 1. Plugin Subsystem

Add `src/plugins/` as first-class subsystem:
- `src/plugins/mod.rs`
- `src/plugins/traits.rs`
- `src/plugins/manifest.rs`
- `src/plugins/runtime.rs`
- `src/plugins/registry.rs`
- `src/plugins/hot_reload.rs`
- `src/plugins/bridge/observer.rs`

### 2. WIT Contracts

Define separate contracts under `wit/zeroclaw/`:
- `hooks/v1`
- `tools/v1`
- `providers/v1`

Each contract has independent semver policy and compatibility checks.

### 3. Capability Model

Manifest-declared capabilities are deny-by-default.
Host grants capability-specific rights through config policy.
Examples:
- `hooks`
- `tools.execute`
- `providers.chat`
- optional I/O scopes (network/fs/secrets) via explicit allowlists

### 4. Runtime Lifecycle

1. Discover plugin manifests in configured directories.
2. Validate metadata (ABI version, checksum/signature policy, capabilities).
3. Instantiate plugin runtime components in immutable snapshot.
4. Register plugin-provided hook handlers, tools, and providers.
5. Atomically publish snapshot.

### 5. Dispatch Model

#### Hooks

- Void hooks: bounded parallel fanout + timeout.
- Modifying hooks: deterministic ordered pipeline (priority desc, stable plugin-id tie-breaker).

#### Tools

- Merge native and plugin tool specs.
- Route tool calls by ownership.
- Keep host-side security policy enforcement before plugin execution.
- Apply `ToolResultPersist` modifying hook before final persistence and feedback.

#### Providers

- Extend provider factory lookup to include plugin provider registry.
- Plugin providers participate in existing resilience and routing wrappers.

### 6. New Hook Points

Add and wire:
- `BeforeCompaction`
- `AfterCompaction`
- `ToolResultPersist`
- `fire_gateway_stop` call site on graceful gateway shutdown

### 7. Built-in Hooks

Provide built-ins loaded through same hook registry:
- `session_memory`
- `boot_script`

This keeps runtime behavior consistent between native and plugin hooks.

### 8. ObserverBridge

Add adapter that maps observer events into hook events, preserving legacy observer flows while enabling hook-based plugin processing.

### 9. Hot Reload

- Watch plugin files/manifests.
- Rebuild and validate candidate snapshot fully.
- Atomic swap on success.
- Keep old snapshot if reload fails.
- In-flight invocations continue on the snapshot they started with.

## Safety and Reliability

- Per-plugin memory/CPU/time/concurrency limits.
- Invocation timeout and trap isolation.
- Circuit breaker for repeatedly failing plugins.
- No plugin error may crash core runtime path.
- Sensitive payload redaction at host observability boundary.

## Compatibility Strategy

- Independent major-version compatibility checks per WIT package.
- Reject incompatible plugins at load time with clear diagnostics.
- Preserve native implementations as fallback path.

## Testing Strategy

### Unit

- manifest parsing and capability policy
- ABI compatibility checks
- hook ordering and cancellation semantics
- timeout/trap handling

### Integration

- plugin tool registration/execution
- plugin provider routing + fallback
- compaction hook sequence
- gateway stop hook firing
- hot-reload swap/rollback behavior

### Regression

- native-only mode unchanged when plugins disabled
- security policy enforcement remains intact

## Rollout Plan

1. Foundation: subsystem + config + ABI skeleton.
2. Hook integration + new hook points + built-ins.
3. Tool plugin routing.
4. Provider plugin routing.
5. Hot reload + ObserverBridge.
6. SDK + docs + example plugins.

## Non-goals (v1)

- dynamic cross-plugin dependency resolution
- distributed remote plugin registries
- automatic plugin marketplace installation

## Risks

- ABI churn if contracts are not tightly scoped.
- runtime overhead with poorly bounded plugin execution.
- operational complexity from hot-reload races.

## Mitigations

- capability segmentation + strict semver.
- hard limits and circuit breakers.
- immutable snapshot architecture for reload safety.
