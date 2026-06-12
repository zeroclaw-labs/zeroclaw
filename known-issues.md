# Phase 1.1 — Known Issues

Compiled by Wave 0 preflight. Items here are flagged for Wave 2 (deny.toml) and downstream phases (1.4 telemetry strip, 1.5 source strips).

## opentelemetry-otlp optional dep in `crates/zeroclaw-runtime/Cargo.toml`

**Status:** Optional dep behind feature `observability-otel`; NOT in workspace defaults.

**Phase 1.1 mitigation (deny.toml):**
- Set `[graph].all-features = false` and `no-default-features = false` so cargo-deny only resolves the default feature set.
- Add `opentelemetry-otlp` to `[bans].deny` with a comment: `# TODO(phase-1.4): delete the optional dep entirely`.
- This means `cargo deny check bans` exits 0 on Phase 1.1 PR because the optional dep isn't in the resolved graph.

**Phase 1.4 follow-up:**
- Delete the `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp` entries from `crates/zeroclaw-runtime/Cargo.toml`.
- Delete the `observability-otel` feature definition.
- Update `deny.toml` to remove the `# TODO(phase-1.4)` marker once the dep is gone.

## Other phone-home crate names searched

None found as direct or workspace-wide deps:
- `sentry`
- `posthog-rust` / `posthog`
- `honeycomb`
- `datadog`

If Phase 1.4 telemetry audit surfaces additional crates (e.g., usage of `reqwest` to a non-customer-controlled domain in source), they get added here.

## Tooling expectations for Wave 1/2

- **`gh` CLI is not installed.** Wave 1 fork operations use raw `git` + the local credential helper. The GitHub fork relationship was created manually by the user via the GitHub UI before execution started. Branch protection (a `PUT /repos/.../branches/.../protection` API call) is **deferred** — either the user sets it via GitHub Settings UI, or a future phase wires it via curl with a `$GITHUB_PAT` env var.
- **`jq` is not installed.** Validation scripts use `python -m json.tool` or `python -c "import json"` instead.
