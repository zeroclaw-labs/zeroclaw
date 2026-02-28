# Actions Source Policy (Phase 1)

This document defines the current GitHub Actions source-control policy for this repository.

Phase 1 objective: lock down action sources with minimal disruption, before full SHA pinning.

## Current Policy

- Repository Actions permissions: enabled
- Allowed actions mode: selected
- SHA pinning required: false (deferred to Phase 2)

Selected allowlist patterns:

- `actions/*` (covers `actions/cache`, `actions/checkout`, `actions/upload-artifact`, `actions/download-artifact`, and other first-party actions)
- `docker/*`
- `dtolnay/rust-toolchain@*`
- `DavidAnson/markdownlint-cli2-action@*`
- `lycheeverse/lychee-action@*`
- `EmbarkStudios/cargo-deny-action@*`
- `rustsec/audit-check@*`
- `rhysd/actionlint@*`
- `softprops/action-gh-release@*`
- `sigstore/cosign-installer@*`
- `Checkmarx/vorpal-reviewdog-github-action@*`
- `Swatinem/rust-cache@*`

## Change Control Export

Use these commands to export the current effective policy for audit/change control:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

Record each policy change with:

- change date/time (UTC)
- actor
- reason
- allowlist delta (added/removed patterns)
- rollback note

## Why This Phase

- Reduces supply-chain risk from unreviewed marketplace actions.
- Preserves current CI/CD functionality with low migration overhead.
- Prepares for Phase 2 full SHA pinning without blocking active development.

## Agentic Workflow Guardrails

Because this repository has high agent-authored change volume:

- Any PR that adds or changes `uses:` action sources must include an allowlist impact note.
- New third-party actions require explicit maintainer review before allowlisting.
- Expand allowlist only for verified missing actions; avoid broad wildcard exceptions.
- Keep rollback instructions in the PR description for Actions policy changes.

## Validation Checklist

After allowlist changes, validate:

1. `CI`
2. `Docker`
3. `Security Audit`
4. `Workflow Sanity`
5. `Release` (when safe to run)

Failure mode to watch for:

- `action is not allowed by policy`

If encountered, add only the specific trusted missing action, rerun, and document why.

Latest sweep notes:

- 2026-02-21: Added manual Vorpal reviewdog workflow for targeted secure-coding checks on supported file types
    - Added allowlist pattern: `Checkmarx/vorpal-reviewdog-github-action@*`
    - Workflow uses pinned source: `Checkmarx/vorpal-reviewdog-github-action@8cc292f337a2f1dea581b4f4bd73852e7becb50d` (v1.2.0)
- 2026-02-26: Standardized runner/action sources for cache and Docker build paths
    - Added allowlist pattern: `Swatinem/rust-cache@*`
    - Docker build jobs use `docker/setup-buildx-action` and `docker/build-push-action`
- 2026-02-16: Hidden dependency discovered in `release.yml`: `sigstore/cosign-installer@...`
    - Added allowlist pattern: `sigstore/cosign-installer@*`
- 2026-02-17: Security audit reproducibility/freshness balance update
    - Added allowlist pattern: `rustsec/audit-check@*`
    - Replaced inline `cargo install cargo-audit` execution with pinned `rustsec/audit-check@69366f33c96575abad1ee0dba8212993eecbe998` in `security.yml`
    - Supersedes floating-version proposal in #588 while keeping action source policy explicit

## Rollback

Emergency unblock path:

1. Temporarily set Actions policy back to `all`.
2. Restore selected allowlist after identifying missing entries.
3. Record incident and final allowlist delta.
