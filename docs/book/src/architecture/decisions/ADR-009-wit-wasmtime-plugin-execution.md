---
id: ADR-009
title: WIT components and direct wasmtime replace the Extism plugin bridge
date: 2026-07-04
status: accepted
relates-to:
  - ADR-003
  - crates/zeroclaw-plugins
  - wit/v0
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
---

# ADR-009: WIT Components And Direct Wasmtime Replace The Extism Plugin Bridge

This ADR supersedes [ADR-003](./ADR-003-wasm-plugin-model.md). ADR-003
recorded the initial Extism bridge. The current accepted architecture is
a WIT-defined WASM Component Model surface hosted directly by `wasmtime`.

## Context

Extism was a useful bootstrap for proving that external WASM plugins
could appear as ZeroClaw tools. It also gave the project a simple JSON
protocol and a permission-gated host-function model.

As the microkernel architecture matured, the plugin surface needed a
stronger compatibility boundary:

- plugin contracts needed to be explicit, versioned, and reviewable;
- tools, channels, and memory backends needed separate typed worlds;
- the host needed release-target-specific execution backends;
- host imports needed to attach to permissions at link time;
- plugin authors needed a durable ABI instead of ad hoc JSON exports;
- store limits and WASI host surfaces needed to be owned by ZeroClaw.

The WASM Component Model and WIT provide that boundary. Direct
`wasmtime` integration gives the host enough control to select backends,
attach WASI Preview 2 surfaces, enforce resource limits, and bridge
guest worlds into ZeroClaw's Rust traits.

## Decision

ZeroClaw's plugin ABI is based on WASM components described by WIT
interfaces under `wit/v0`. The host uses direct `wasmtime` component
model plumbing in `crates/zeroclaw-plugins`, with per-world bridges for
tools, channels, and memory backends.

The execution model is:

- `wit/v0/tool.wit`, `channel.wit`, and `memory.wit` define the guest
  contracts.
- `crates/zeroclaw-plugins/src/component.rs` owns shared component host
  plumbing, store state, resource limits, WIT bindings, and WASI wiring.
- `wasm_tool.rs`, `wasm_channel.rs`, and `wasm_memory.rs` bridge those
  worlds back into the Rust `Tool`, `Channel`, and `Memory` traits.
- A plugin `manifest.toml` declares the plugin name, version,
  capability type, permissions, config, and signature material.
- Ed25519 manifest verification remains part of the plugin host.

Execution backend selection is explicit:

- `plugins-wasm` enables the plugin host surface in the main workspace.
- `plugins-wasm-runtime-only` enables the smallest runtime-only host.
- `plugins-wasm-cranelift` enables Cranelift compilation where supported.
- `plugins-wasm-pulley` enables the Pulley interpreter for targets where
  Cranelift is unavailable or undesirable.

Host surfaces are permission-gated:

- `HttpClient` is the permission that attaches outbound HTTP state and
  links WASI HTTP.
- `ConfigRead` is required before the host injects resolved values under tool
  `__config` or serves channel `config.get`. A tool or channel consumer may mark
  top-level string properties `x-secret = true`; those values never enter the
  public object and are read through the instance-scoped `secrets` import.
  Tools receive secret access during `execute`. Channels receive `config.get`
  and `secrets.get` during `configure` and operational calls; both reads share
  one canonical config resolution per call. Instantiation and static metadata
  discovery remain unavailable. The host drops each call's materialized view;
  compliant channel guests must resolve at point of use and not retain returned
  config or plaintext, which the host cannot enforce after delivery. Static
  identity and capability exports are read at load, so changing those values
  requires channel lifecycle reconstruction.
- The host does not expose a raw environment-variable read function.
- Store limits, fuel, table limits, instance limits, and memory ceilings
  are resolved before the store is built.

## Consequences

Positive:

- Plugins share one typed contract surface with the Rust host instead of
  ad hoc JSON conventions per plugin type.
- WIT files become the compatibility boundary that can be frozen and
  reviewed.
- Tool, channel, and memory plugins can appear to the runtime as native
  trait implementations.
- Execution backends are selected by release target instead of hidden in
  one all-purpose feature flag.
- Permission checks are attached to host imports, not only documented in
  manifests.

Negative:

- Direct `wasmtime` component integration is more complex than the
  original Extism bridge.
- Release builds must choose the right execution backend for each target.
- Plugin authors need to build WASI Preview 2 components and follow the
  WIT interfaces rather than exporting JSON functions.
- The WIT surface now needs compatibility discipline. Changing it is a
  cross-plugin architecture decision, not a local crate edit.

Follow-up:

- WIT versioning and compatibility rules live with the WIT docs. If the
  compatibility policy changes, write a new ADR rather than silently
  editing this one.
- New guest worlds should be added as versioned WIT surfaces with
  matching host bridge code and permission review.

## References

- [ADR-003: WASM plugins use Extism as the initial execution bridge](./ADR-003-wasm-plugin-model.md)
- [FND-001: Intentional architecture](../../foundations/fnd-001-intentional-architecture.md)
- [Plugin protocol](../../developing/plugin-protocol.md)
- `wit/v0/`
- `wit/VERSIONING.md`
- `crates/zeroclaw-plugins/src/component.rs`
- `crates/zeroclaw-plugins/src/wasm_tool.rs`
- `crates/zeroclaw-plugins/src/wasm_channel.rs`
- `crates/zeroclaw-plugins/src/wasm_memory.rs`
- `crates/zeroclaw-plugins/src/host.rs`
- `crates/zeroclaw-plugins/src/signature.rs`
- `crates/zeroclaw-infra/src/net_guard.rs`
