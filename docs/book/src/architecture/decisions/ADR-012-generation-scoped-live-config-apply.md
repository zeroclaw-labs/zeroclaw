---
id: ADR-012
title: Live config application uses generation-scoped publication and results
date: 2026-07-19
status: proposed
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7897
  - docs/book/src/architecture/config-lifecycle.md
  - crates/zeroclaw-config/src/schema.rs
  - crates/zeroclaw-gateway/src/api_config.rs
  - crates/zeroclaw-channels/src/orchestrator/mod.rs
---

# ADR-012: Live Config Application Uses Generation-Scoped Publication And Results

## Context

ZeroClaw can save configuration through CLI, RPC, TUI, Quickstart, and gateway surfaces. A successful save does not mean every long-lived subsystem has adopted the new value. Gateway-visible state can change immediately while daemon-owned channels, sessions, providers, and other components continue using their previous runtime state until `/admin/reload` rebuilds the subsystem graph.

Accepted RFC [#7897](https://github.com/zeroclaw-labs/zeroclaw/issues/7897) targets a bounded improvement: selected security-policy and channel changes can apply without a full daemon reload, while operators receive a target-specific result for the generation that each subsystem actually processed. The accepted architecture requires one canonical published config, generation-specific results, narrow security overlays, and only proven channel transition modes.

This record defines that target and its implementation gates. It does not claim that live application already exists. Until those gates ship, the current saved-versus-applied behavior and `/admin/reload` fallback described in [Config lifecycle](../config-lifecycle.md) remain authoritative.

## Decision

### Publish one canonical config generation

The process has one canonical published config revision. In-process writers are serialized separately from readers. A writer clones the current revision, applies and validates its changes, persists the resulting config within the serialized write transaction, and only then atomically publishes the next generation. Coordination between independent processes that edit `config.toml` is outside this decision.

The reader-facing config lock is not held across asynchronous disk I/O. Publication does not create a second long-lived config cache or retain a previous config snapshot for general rollback.

Apply events identify the published generation and changed paths. They do not carry another full config copy or a previous config value. Each target reads the current canonical revision and applies it only when its generation matches the event. A superseded event is skipped and cannot overwrite the result for a newer generation.

### Record results for the generation actually applied

Every apply target records its own generation-specific result:

- `AppliedLive` means that target adopted the identified generation without a daemon reload.
- `QueuedForReload` means the change was saved but that target requires `/admin/reload`; the result includes a concrete reason.
- `Rejected` means the target refused live application for that generation; the result includes a concrete reason.

A target completion for an older generation cannot overwrite the status of a newer one. Config status surfaces report the target results rather than inferring one global applied state from changed-path prefixes.

### Keep the approved security overlay narrow

The approved security live-apply boundary covers only `allowed_commands` and `forbidden_paths`. It preserves the existing `SecurityPolicy` and its rate-limit tracker instead of reconstructing the policy when those fields change.

Execution receives a typed overlay for the applicable generation. The overlay is propagated explicitly into spawned tasks and `JoinSet` work; task-local state may be a convenience but is not the sole security authority. Missing execution scope or a generation mismatch fails closed rather than falling back to stale policy.

Broader risk-profile reassignment and other security-policy changes remain queued for reload unless a later architecture decision proves a safe live boundary.

### Limit channel changes to proven transition modes

The approved channel live-apply boundary supports only changes with a proven `InPlace` or `Handover` boundary. An in-place change is resolved by the running adapter at use time. A handover constructs and proves the replacement before detaching the existing channel instance.

Changes that cannot prove either boundary remain `QueuedForReload` with a concrete reason. This decision does not add a generic stop-start rollback mechanism because that would require retaining prior config state or accepting an interruption that the handover contract is meant to avoid.

### Preserve full reload as the fallback

`/admin/reload` remains the supported fallback for every config change outside the proven live-apply boundaries. This decision does not change reload authentication, authorization, or standalone-gateway behavior.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- canonical config publication and the generation-scoped applied-state ledger ship without enabling new live-apply behavior;
- every target result names the generation it processed, and stale completions cannot overwrite newer status;
- `allowed_commands` and `forbidden_paths` use a typed, explicitly propagated execution overlay that preserves the existing rate-limit tracker and fails closed for missing scope or generation mismatch;
- proven channel in-place and handover paths preserve the existing service until replacement readiness, while unsupported changes report a concrete reload reason; and
- config APIs and operator documentation report per-target live, queued, and rejected outcomes with concrete reasons and retain `/admin/reload` as the fallback.

Canonical publication and the result ledger must ship without new live behavior before the security and channel consumers are enabled. Security live application precedes channel live application.

## Consequences

Positive consequences:

- Config status can distinguish what was saved from what each subsystem actually applied.
- Concurrent in-process writes cannot silently publish from stale starting revisions.
- Slow completion from an older apply attempt cannot make a newer generation appear applied.
- The approved security and channel consumers have narrow, testable safety boundaries.
- Unsupported changes retain a clear and familiar full-reload path.

Negative consequences:

- Publication needs writer serialization and generation tracking in addition to the reader-facing config handle.
- Each live-apply target must own a result handler and generation-aware tests.
- Execution-scoped security state must be propagated through asynchronous task boundaries explicitly.
- Many config paths will continue to require reload; live application is an allowlist of proven behavior rather than a general promise.

## References

- [RFC #7897: Apply security policy and channel config updates without full daemon reload](https://github.com/zeroclaw-labs/zeroclaw/issues/7897)
- [Config lifecycle](../config-lifecycle.md)
- `crates/zeroclaw-config/src/schema.rs`
- `crates/zeroclaw-gateway/src/api_config.rs`
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`
