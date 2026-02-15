# AGENTS.md — ZeroClaw Agent Coding Guide

This file defines the default working protocol for coding agents in this repository.
Scope: entire repository.

## 1) Project Snapshot (Read First)

ZeroClaw is a Rust-first autonomous agent runtime optimized for:

- high performance
- high efficiency
- high stability
- high extensibility
- high sustainability
- high security

Core architecture is trait-driven and modular. Most extension work should be done by implementing traits and registering in factory modules.

Key extension points:

- `src/providers/traits.rs` (`Provider`)
- `src/channels/traits.rs` (`Channel`)
- `src/tools/traits.rs` (`Tool`)
- `src/memory/traits.rs` (`Memory`)
- `src/observability/traits.rs` (`Observer`)
- `src/runtime/traits.rs` (`RuntimeAdapter`)

## 2) Repository Map (High-Level)

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module exports and shared command enums
- `src/config/` — schema + config loading/merging
- `src/agent/` — orchestration loop
- `src/gateway/` — webhook/gateway server
- `src/security/` — policy, pairing, secret store
- `src/memory/` — markdown/sqlite memory backends + embeddings/vector merge
- `src/providers/` — model providers and resilient wrapper
- `src/channels/` — Telegram/Discord/Slack/etc channels
- `src/tools/` — tool execution surface (shell, file, memory, browser)
- `src/runtime/` — runtime adapters (currently native)
- `docs/` — architecture + process docs
- `.github/` — CI, templates, automation workflows

## 3) Non-Negotiable Engineering Constraints

### 3.1 Performance and Footprint

- Prefer minimal dependencies; avoid adding crates unless clearly justified.
- Preserve release-size profile assumptions in `Cargo.toml`.
- Avoid unnecessary allocations, clones, and blocking operations.
- Keep startup path lean; avoid heavy initialization in command parsing flow.

### 3.2 Security and Safety

- Treat `src/security/`, `src/gateway/`, `src/tools/` as high-risk surfaces.
- Never broaden filesystem/network execution scope without explicit policy checks.
- Never log secrets, tokens, raw credentials, or sensitive payloads.
- Keep default behavior secure-by-default (deny-by-default where applicable).

### 3.3 Stability and Compatibility

- Preserve CLI contract unless change is intentional and documented.
- Prefer explicit errors over silent fallback for unsupported critical paths.
- Keep changes local; avoid cross-module refactors in unrelated tasks.

## 4) Agent Workflow (Required)

1. **Read before write**
   - Inspect existing module and adjacent tests before editing.
2. **Define scope boundary**
   - One concern per PR; avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch**
   - Follow KISS/YAGNI/DRY; no speculative abstractions.
4. **Validate by risk**
   - Docs-only: keep checks lightweight.
   - Code changes: run relevant checks and tests.
5. **Document impact**
   - Update docs/PR notes for behavior, risk, rollback.

## 5) Change Playbooks

### 5.1 Adding a Provider

- Implement `Provider` in `src/providers/`.
- Register in `src/providers/mod.rs` factory.
- Add focused tests for factory wiring and error paths.

### 5.2 Adding a Channel

- Implement `Channel` in `src/channels/`.
- Ensure `send`, `listen`, and `health_check` semantics are consistent.
- Cover auth/allowlist/health behavior with tests.

### 5.3 Adding a Tool

- Implement `Tool` in `src/tools/` with strict parameter schema.
- Validate and sanitize all inputs.
- Return structured `ToolResult`; avoid panics in runtime path.

### 5.4 Security / Runtime / Gateway Changes

- Include threat/risk notes and rollback strategy.
- Add or update tests for boundary checks and failure modes.
- Keep observability useful but non-sensitive.

## 6) Validation Matrix

Default local checks for code changes:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If full checks are impractical, run the most relevant subset and document what was skipped and why.

For workflow/template-only changes, at least ensure YAML/template syntax validity.

## 7) Collaboration and PR Discipline

- Follow `.github/pull_request_template.md`.
- Keep PR descriptions concrete: problem, change, non-goals, risk, rollback.
- Use conventional commit titles.
- Prefer small PRs (`size: XS/S/M`) when possible.

Reference docs:

- `CONTRIBUTING.md`
- `docs/pr-workflow.md`

## 8) Anti-Patterns (Do Not)

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.

## 9) Handoff Template (Agent -> Agent / Maintainer)

When handing off work, include:

1. What changed
2. What did not change
3. Validation run and results
4. Remaining risks / unknowns
5. Next recommended action

## 10) Vibe Coding Guardrails

When working in a fast iterative "vibe coding" style:

- Keep each iteration reversible (small commits, clear rollback).
- Validate assumptions with code search before implementing.
- Prefer deterministic behavior over clever shortcuts.
- Do not "ship and hope" on security-sensitive paths.
- If uncertain, leave a concrete TODO with verification context, not a hidden guess.
