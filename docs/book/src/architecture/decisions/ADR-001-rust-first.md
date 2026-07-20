---
id: ADR-001
title: Rust is the implementation language for ZeroClaw
date: 2026-07-04
status: accepted
relates-to:
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
  - docs/book/src/architecture/crates.md
  - Cargo.toml
---

# ADR-001: Rust Is The Implementation Language For ZeroClaw

This is a retroactive record of a decision made before the formal ADR
process. The exact original decision date is not available in this
record; the date above is the date this ADR was added to the
architecture docs.

This record was drafted from
[FND-002 §6.3](../../foundations/fnd-002-documentation-standards.md#63-foundational-adr-backlog)
and the current repository structure. It was not recovered from an
older ADR file.

## Context

ZeroClaw is a local-first agent runtime whose primary distribution
needs to run without a required language runtime installed by the user.
It integrates with many operating-system and network boundaries, and it
keeps strong control over security, memory, process, logging, and
configuration behavior.

The project also has adjacent agent-system experiments in other
languages. Those projects are useful for exploration, but the runtime
needs one implementation language for the code that ships as ZeroClaw:
providers, channels, tools, memory, config, gateway, hardware support,
and the user-facing CLI.

Rust fits the requirements that shape the runtime:

- predictable single-binary distribution;
- explicit ownership and error handling at IO and security boundaries;
- async networking and process supervision without a large runtime
  dependency;
- feature-gated builds for channels, hardware, gateway, and optional
  capabilities;
- one Cargo workspace for crates, tests, docs generation, and release
  workflows.

## Decision

ZeroClaw's runtime, first-party crates, CLI, gateway, tooling hosts,
provider integrations, channel integrations, memory backends, config
schema, and hardware support are implemented as Rust workspace
members.

Non-Rust code may exist at the edges when it is the correct boundary:
shell scripts, release and packaging helpers, generated web assets,
external CLIs, MCP servers, skill scripts, and plugin guests. These
surfaces may support, package, drive, or extend ZeroClaw, but they do not
become the implementation base for the core runtime unless a new
accepted ADR supersedes this one.

## Consequences

Positive consequences:

- Contributors can reason about runtime behavior through one typed
  workspace rather than several language runtimes.
- Build, lint, test, docs generation, release, and feature gating for
  the primary workspace all route through Cargo.
- Security-sensitive code benefits from Rust's ownership model and
  explicit error propagation.
- First-party integrations share crate boundaries, trait contracts, and
  logging/config conventions.

Negative consequences:

- Contributors who only know TypeScript, Python, Go, or shell must cross
  a Rust learning curve before changing core behavior.
- Web, UI, and external-service integrations need explicit boundary
  design instead of freely sharing application state with the runtime.
- Generated docs, localization catalogues, and release artifacts often
  depend on Rust tooling even when the visible output is Markdown,
  Fluent, HTML, or packaging metadata.
- Experiments in adjacent projects must be deliberately ported into Rust
  before they become ZeroClaw runtime behavior.

Follow-up decisions:

- [ADR-002](./ADR-002-trait-driven-extensibility.md) records how Rust
  extension points are exposed inside the workspace.
- [ADR-003](./ADR-003-wasm-plugin-model.md) records the plugin and
  component boundary.

## References

- [FND-001: Intentional architecture](../../foundations/fnd-001-intentional-architecture.md)
- [Architecture: Crates](../crates.md)
- [Plugin protocol](../../developing/plugin-protocol.md)
- `AGENTS.md`
- `Cargo.toml`
