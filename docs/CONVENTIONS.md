# ZeroClaw Codebase Conventions & Mindset

This document outlines the architectural philosophy, coding standards, and common patterns for the ZeroClaw project. It serves as a bridge between high-level documentation and the codebase.

⚠️ A lot of effort has gone into detailed and useful documentation, DO READ [CONTRIBUTING.md](../CONTRIBUTING.md) after this overview. ⚠️

-

## 🦀 The ZeroClaw Mindset

ZeroClaw is built on four core pillars: **Zero Overhead, Zero Compromise, 100% Rust, and 100% Agnostic.**

1.  **Efficiency is Feature #1:** We target $10 hardware with <5MB RAM. If a change increases the baseline memory footprint significantly without a massive feature win, it's a regression.
2.  **Trait-Driven Extensibility:** Everything is a [Trait](https://dev.to/shuttle_dev/intro-to-advanced-rust-traits-generics-4h87). Providers, Channels, Tools, Memory, and Peripherals are all pluggable. If you find yourself hardcoding logic for a specific service, you should probably be implementing a Trait.
3.  **Local-First, Cloud-Optional:** While we support cloud LLMs, the "brain" and control plane (the Gateway) should always be capable of running locally and privately.
4.  **Security by Default:** DMs are untrusted. Action requires approval by default. Sandboxing is a first-class citizen.

=

## 🛠 Architectural Patterns

### The Gateway as Orchestrator
The Gateway is not just a web server; it's the stateful coordinator for sessions, channels, and tools. It handles the lifecycle of the agent's "hands" and "senses."

### The Agent Loop ([`src/agent/loop_.rs`](../crates/zeroclaw-runtime/src/agent/loop_.rs)
The core reasoning engine. It follows a classic "Observe -> Plan -> Act -> Reflect" cycle, but optimized for low-latency Rust execution. 
*Note: This module is currently a "God Module" and is a candidate for functional decomposition.*

### Configuration (`Config` struct)
We use a single, unified TOML configuration. While convenient, this has led to a "God Struct" pattern. New features should aim to use sub-configurations or scoped traits where possible.

-

## ⚠️ Code Smells & Known Technical Debt

As of April 2026, the project is undergoing rapid expansion. Be aware of the following:

1.  **"God Modules":** Several files have exceeded 10,000 lines (e.g., [`zeroclaw-config/src/schema.rs`](../crates/zeroclaw-config/src/schema.rs), [`zeroclaw-channels/src/orchestrator/mod.rs`](../crates/zeroclaw-channels/src/orchestrator/mod.rs)`). Work to split these into smaller, domain-specific modules has not begun.
2.  **Unwrap & Panic Prevalence:** There is a high density of `.unwrap()` and `panic!` calls in the codebase (5,980+ and 210+ respectively). **New code MUST NOT use `unwrap()` or `panic!`** in production paths. Use `anyhow::Result` or `thiserror` for proper error propagation.
3.  **Global Lint Suppressions:** Crate-level `#![allow(...)]` in `lib.rs` and `main.rs` hides a dozen clippy warnings. We are moving towards per-module or per-function allows with justification comments.
4.  **Silent Error Swallowing:** There are over 1100 instances of `let _ = ...` on `Result` types. Move toward logging warnings using the `tracing` crate.

-

## 🧪 Testing Strategy

*   **Unit Tests:** Every module should have an inline `#[cfg(test)] mod tests`.
*   **Component Tests:** Located in [`tests/component/`](../tests/component/), these test subsystem interactions.
*   **Integration Tests:** Located in [`tests/integration/`](../tests/integration/), these test end-to-end flows.
*   **Reproduction First:** For bug fixes, always include a test case that reproduces the failure before applying the fix. 🙏

-

## 📚 Documentation Standards

*   **README-First:** Major features should be documented in the relevant [`docs/`](../) subdirectory before implementation.
*   **i18n:** We support 30+ languages. If you change a user-facing string, it must be updated in the [`tool_descriptions/`](../tool_descriptions/) and [`web/src/lib/i18n.ts`](../web/src/lib/i18n.ts) files (or marked for translation).
*   **ADRs:** Significant architectural decisions should be recorded as Architectural Decision Records (ADRs) in [`docs/architecture/`](architecture/).

- 

*“Zero overhead. Zero compromise. Deploy anywhere. Swap anything.”* 🦀

PS. Read the wiki too! https://github.com/zeroclaw-labs/zeroclaw/wiki
