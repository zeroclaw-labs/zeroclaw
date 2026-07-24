---
id: ADR-006
title: Make runtime plugins the target for optional channels
date: 2026-07-18
status: proposed
relates-to:
  - ADR-002
  - ADR-009
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8850
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8691#issuecomment-5009706612
  - wit/v0/channel.wit
  - crates/zeroclaw-plugins/src/wasm_channel.rs
---

# ADR-006: Make Runtime Plugins the Target for Optional Channels

## Context

ZeroClaw currently compiles many messaging integrations behind Cargo feature flags. This keeps unused channel code out of selected builds, but adding or updating an optional integration still requires rebuilding the application. It also keeps vendor-specific dependencies and release cadence coupled to the main binary.

The WIT component model now defines a channel-plugin world, and the host adapter implements the shared `Channel` trait for a WASM component. Discovery and the host-side adapter exist, but a running daemon does not yet construct discovered channel plugins or provide every per-vendor listener they need. Runtime plugins therefore describe a real target with an incomplete operational path, not the current packaging model.

The alternatives are to retain a permanent compiled/runtime hybrid, require an immediate all-channel migration, or make runtime plugins the default target while allowing explicit native exceptions for capabilities the plugin boundary cannot yet support.

## Decision

We will make runtime-installable plugins the target packaging and execution model for optional channels.

An optional channel should migrate from a compiled feature to a runtime plugin when the WIT/WASI host boundary can provide its required networking, webhook or polling ingress, configuration, secrets, media, interaction, and lifecycle capabilities without weakening the runtime security model.

Native implementations may remain during the transition only as explicit, capability-based exceptions. An exception must identify the missing host capability or operational constraint, the native code path that depends on it, and the condition that permits migration. Vendor identity, existing feature flags, or implementation age are not by themselves reasons for a permanent exception.

The runtime owns plugin discovery, permission enforcement, configuration delivery, listener supervision, health reporting, and adaptation to the shared `Channel` contract. A channel plugin owns platform-specific protocol behavior and message translation. Gateway-hosted ingress may route generic HTTP traffic, but it should not become the permanent owner of vendor-specific channel logic.

Issue #8850 owns migration sequencing, capability-gap tracking, and the boundary for retiring compiled optional channels. This ADR does not select which channel migrates first or require removing a working native implementation before its runtime replacement has equivalent operational support.

This ADR remains proposed until all of these conditions are met:

- discovered channel plugins can be configured and registered by a running daemon;
- a supported distribution can install, configure, and run a channel plugin without a custom rebuild;
- inbound listener or webhook traffic can reach the plugin under supervised runtime ownership;
- at least one existing optional compiled channel has migrated end to end; and
- the exception and compatibility process is documented for channels that cannot yet migrate.

## Consequences

Positive consequences:

- Users can add and update optional integrations without rebuilding ZeroClaw.
- Vendor dependencies and release cadence can move out of the main binary.
- Channel implementations share one permissioned runtime boundary and one caller-visible trait contract.
- Native exceptions stay reviewable because they are tied to named capability gaps rather than becoming an undefined permanent second model.

Negative consequences:

- The host must supervise long-lived listeners and preserve channel health, interaction, media, and configuration behavior across the component boundary.
- Some platforms may remain native while WIT/WASI capabilities mature, so the transition still carries two packaging paths.
- Plugin distribution, compatibility, signing, and revocation become part of the operational channel lifecycle.
- Migration requires parity evidence; merely compiling a channel component is not enough to retire its native implementation.

## References

- [ADR-002: Trait-driven extensibility](./ADR-002-trait-driven-extensibility.md)
- [ADR-009: WIT and wasmtime plugin execution](./ADR-009-wit-wasmtime-plugin-execution.md)
- [FND-001: Intentional architecture](../../foundations/fnd-001-intentional-architecture.md)
- [Writing a channel plugin](../../plugins/writing-a-channel-plugin.md)
- [Channel runtime lifecycle](../channel-runtime-lifecycle.md)
- [Migration tracker #8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850)
- [ADR-006 and ADR-007 direction decision](https://github.com/zeroclaw-labs/zeroclaw/issues/8691#issuecomment-5009706612)
- `wit/v0/channel.wit`
- `crates/zeroclaw-plugins/src/wasm_channel.rs`
- `crates/zeroclaw-plugins/src/host.rs`
