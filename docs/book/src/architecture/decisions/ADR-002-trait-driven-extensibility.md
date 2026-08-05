---
id: ADR-002
title: First-party extension surfaces use trait contracts
date: 2026-07-04
status: accepted
relates-to:
  - crates/zeroclaw-api/src/model_provider.rs
  - crates/zeroclaw-api/src/channel.rs
  - crates/zeroclaw-api/src/tool.rs
  - crates/zeroclaw-api/src/memory_traits.rs
  - crates/zeroclaw-api/src/observability_traits.rs
  - crates/zeroclaw-api/src/runtime_traits.rs
  - crates/zeroclaw-api/src/peripherals_traits.rs
  - docs/book/src/architecture/crates.md
  - docs/book/src/developing/tool-inventory.md
---

# ADR-002: First-Party Extension Surfaces Use Trait Contracts

This is a retroactive record of a decision made before the formal ADR
process. The exact original decision date is not available in this
record; the date above is the date this ADR was added to the
architecture docs.

This record was drafted from
[FND-002 §6.3](../../foundations/fnd-002-documentation-standards.md#63-foundational-adr-backlog),
the current `zeroclaw-api` trait surfaces, and the crate and
tool-boundary docs. It was not recovered from an older ADR file.

## Context

ZeroClaw needs many extension families: model providers, messaging
channels, tools, memory backends, observability sinks, runtime adapters,
and hardware peripherals. Each family has different IO, error,
configuration, security, and lifecycle constraints, but each must still
compose into the same agent runtime.

Without explicit contracts, every integration would pressure the runtime
loop to grow special cases. That would make new integrations faster at
first and harder to maintain later: provider routing would leak into
channels, tool policy would leak into providers, channel authentication
would leak into the agent loop, and memory or logging behavior would be
copied across unrelated crates.

The repository already uses `zeroclaw-api` as the public contract layer.
The architecture docs describe that crate as the kernel ABI and state
that the runtime depends on traits rather than concrete
implementations.

## Decision

First-party extension families use explicit Rust trait contracts in
`zeroclaw-api` and are wired through the existing factory, registry,
composition, or host-provided boundary for that surface.

The primary contracts include:

- `ModelProvider` for model-provider clients;
- `Channel` for inbound and outbound messaging surfaces;
- `Tool` for agent-callable capabilities;
- `Memory` and `MemoryStrategy` for persistence and recall;
- `Observer` for runtime telemetry;
- `RuntimeAdapter` for host-runtime capabilities;
- `Peripheral` for hardware and board surfaces.

Shared behavior belongs at the trait, factory, registry, policy, config,
logging, or lower-layer helper boundary when multiple implementations
need it. An individual integration should not patch the runtime loop or
add parallel state only to make one provider, channel, tool, or backend
work.

This ADR covers first-party in-process extension surfaces. It does not
replace the plugin, WIT, MCP, or skill-package boundaries for
out-of-process or independently distributed capabilities.

## Consequences

Positive consequences:

- New first-party integrations can be reviewed against an existing
  contract instead of as bespoke runtime changes.
- Runtime code can stay focused on orchestration, policy, state, and
  lifecycle rather than vendor-specific behavior.
- Tests can target factory, registry, or host-boundary wiring, trait
  behavior, and edge cases without needing a full end-to-end runtime for
  every integration.
- Documentation and review guidance can name concrete extension surfaces
  before implementation starts.

Negative consequences:

- Trait changes have broad blast radius and require careful migration.
- A trait that is too narrow forces integrations to tunnel behavior
  through config, logs, or ad hoc helper paths.
- A trait that is too broad can become a lowest-common-denominator API
  that hides important capability differences.
- Default trait methods can mask unsupported behavior unless docs and
  tests make the defaults explicit.

Follow-up decisions:

- [ADR-003](./ADR-003-wasm-plugin-model.md) governs independently distributed WASM plugin capabilities, not only first-party Rust implementations.
- [ADR-005](./ADR-005-pluggable-memory-backends.md) records the backend-neutral memory storage contract and SQLite default.
- ADR-006 and ADR-007 remain reserved for the implementation-gated channel-plugin and gateway-extraction decisions.

## References

- [Architecture: Crates](../crates.md)
- [Built-in tool inventory](../../developing/tool-inventory.md)
- [Plugin protocol](../../developing/plugin-protocol.md)
- `crates/zeroclaw-api/src/model_provider.rs`
- `crates/zeroclaw-api/src/channel.rs`
- `crates/zeroclaw-api/src/tool.rs`
- `crates/zeroclaw-api/src/memory_traits.rs`
- `crates/zeroclaw-api/src/observability_traits.rs`
- `crates/zeroclaw-api/src/runtime_traits.rs`
- `crates/zeroclaw-api/src/peripherals_traits.rs`
