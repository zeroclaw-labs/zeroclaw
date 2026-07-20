# AGENTS.md - ZeroClaw

Core instructions for AI coding assistants working in this repository. Use `docs/book/src/contributing/architecture-map.md` to load only the references needed for a non-trivial task.

## Single Source Of Truth

Do not duplicate state. Before adding a struct field, config entry, schema field, runtime cache, or parallel lookup table, identify the canonical source:

1. If the new field creates the fact, state that explicitly.
2. If the fact already exists, resolve it from that source at use time.

Prefer borrowed config, getters, resolver closures over live config, on-demand materialized views, or generated surfaces from one input. Do not snapshot live policy into long-lived handles. A restart-only snapshot is not a substitute for resolving canonical state.

## Safety And Privacy

- Never commit secrets, tokens, credentials, personal data, or real identities.
- Do not weaken permissions, allowlists, sandboxing, approvals, or other trust boundaries without making the behavior and risk explicit.
- New external surfaces default closed. Prefer allowlists to blocklists.
- Do not hide behavior changes inside refactors or bypass failing checks.
- Production paths must propagate errors. Avoid `unwrap()` and `expect()` unless a documented invariant makes panic impossible.
- Do not suppress unused production code with underscore names or `#[allow(dead_code)]`; remove it, connect it, or track it. Underscore names remain valid for required but intentionally unused API, trait, or callback parameters.

## Working Rules

1. Read the owning module, factory wiring, adjacent tests, and relevant docs before editing.
2. For architecture, config, security, workflow, governance, CI, release, or agent-assisted changes, start with `docs/book/src/contributing/architecture-map.md`.
3. Name the source of truth before introducing state.
4. Keep one concern per PR. Avoid unrelated cleanup and do not mix broad formatting changes with functional changes.
5. Do not add heavy dependencies for minor convenience, speculative abstractions, or config keys and feature flags without a concrete use case.
6. Add the smallest useful implementation and tests at the real behavior boundary.
7. Validate at the change's risk level, report commands actually run, and document behavior, risk, side effects, and rollback.
8. Use a non-`master` branch, open a PR to `master`, and never push directly to `master`.
9. Use conventional commits and the full PR template. Prefer small PRs and do not add bot or AI attribution footers.
10. Declare stacked work with `Depends on #...` and replacement work with `Supersedes #...`.

Subagents must set their working directory to the repository root before shell or filesystem work. Do not assume an inherited working directory.

## User-Facing Text

- User-facing runtime CLI, tool, and onboarding text uses Fluent `fl!()` keys rather than bare literals.
- Zerocode uses its independent Fluent catalogue through its documented `crate::i18n` helpers. Web dashboard text follows the TypeScript `web/src/lib/i18n.ts` contract, not Rust `fl!()`.
- Logs, tracing fields, and panic text remain English and use stable error keys where the logging contract requires them.
- English Markdown is the documentation source of truth. Follow the documented localization workflow instead of editing generated translations by hand.

## Validation

Choose checks that match the changed surface. Common code checks are:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Use `./dev/ci.sh all` for full pre-PR validation when the scope warrants it. Docs-only changes use `scripts/ci/docs_quality_gate.sh` and `scripts/ci/docs_links_gate.sh`. Bootstrap script changes add `bash -n install.sh`.

## Task References

The architecture map routes task-specific documentation. Consult `docs/book/src/contributing/agent-guidelines.md` only for detailed agent examples, risk and stability policy, skill discovery, and protected operational documents. Do not skip a required contract because it is no longer embedded in this bootstrap file.
