---
id: ADR-007
title: Extract the gateway into a separate optional process
date: 2026-07-18
status: proposed
relates-to:
  - ADR-002
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8691#issuecomment-5009706612
  - crates/zeroclaw-gateway
  - crates/zeroclaw-runtime/src/rpc
---

# ADR-007: Extract the Gateway Into a Separate Optional Process

## Context

ZeroClaw's HTTP, WebSocket, webhook, and dashboard surface already has a dedicated `zeroclaw-gateway` crate. The main application still links and starts that crate in process behind the `gateway` feature. The crate boundary improves code ownership, but it does not provide process isolation or let an operator run, restart, upgrade, or omit the web surface independently from the agent runtime.

The runtime also has a JSON-RPC surface, but the complete stable local contract, transport, authentication model, compatibility policy, and process supervision needed by an external gateway have not shipped as one supported boundary. The current crate and RPC seams are therefore useful migration steps rather than proof that process extraction is complete.

The alternatives are to treat the in-process feature-gated crate as the final architecture or to make the gateway a separate optional process connected to the runtime through an explicit local IPC contract.

## Decision

We will make the gateway a separate optional `zeroclaw-gw` process.

The agent runtime remains the authority for agent execution, sessions, memory, configuration, tools, security policy, and other domain state. The gateway owns external HTTP and streaming protocols, dashboard delivery, pairing and transport-facing concerns, and generic webhook ingress. It must request runtime operations through the supported IPC contract rather than reach into process-local runtime state.

The runtime-to-gateway IPC boundary must be authenticated, versioned, and documented. Transport selection may vary by platform, but the default connection must remain local and must not silently expose the runtime control surface on a public interface.

The current feature-gated in-process `zeroclaw-gateway` crate is an intermediate seam. It may remain available while IPC parity and process lifecycle support are built, but new architecture should not make direct access to runtime internals a permanent requirement of the gateway.

Operators must be able to run the agent without `zeroclaw-gw`. A gateway failure or restart must not terminate otherwise healthy agent execution.

This ADR remains proposed until all of these conditions are met:

- a supported `zeroclaw-gw` binary communicates with the runtime through the documented local IPC contract;
- the IPC boundary covers the gateway capabilities required for the supported dashboard and API experience without direct process-local state access;
- authentication, version negotiation, reconnect, health, startup, and shutdown behavior are implemented and documented; and
- a supported runtime distribution can operate without linking or starting the gateway server.

## Consequences

Positive consequences:

- Headless and constrained deployments can omit the web surface completely.
- Gateway crashes, restarts, and upgrades are isolated from agent execution.
- The IPC contract gives desktop applications and other trusted local clients a defined integration boundary.
- Runtime and gateway ownership become easier to test and reason about because cross-boundary access is explicit.

Negative consequences:

- Multi-process installation, startup, authentication, logging, and diagnosis are more complex than an in-process function call.
- Streaming and large payloads must cross a serialized boundary.
- Compatibility policy is required whenever runtime and gateway versions can differ.
- Until parity is complete, maintainers must support both the current in-process seam and the target process boundary.

## References

- [ADR-002: Trait-driven extensibility](./ADR-002-trait-driven-extensibility.md)
- [FND-001: Intentional architecture](../../foundations/fnd-001-intentional-architecture.md)
- [Crate map](../crates.md)
- [Channel runtime lifecycle](../channel-runtime-lifecycle.md)
- [Gateway API](../../gateway/api.md)
- [ADR-006 and ADR-007 direction decision](https://github.com/zeroclaw-labs/zeroclaw/issues/8691#issuecomment-5009706612)
- `crates/zeroclaw-gateway`
- `crates/zeroclaw-runtime/src/rpc`
- `crates/zeroclaw-api/src/jsonrpc.rs`
