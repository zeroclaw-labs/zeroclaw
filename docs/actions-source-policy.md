# Actions Source Policy

This document defines the current GitHub Actions source-control policy for this repository.

## Current Policy

- Repository Actions permissions: enabled
- Allowed actions mode: selected

Selected allowlist (all actions currently used across CI, Beta Release, and Promote Release workflows):

| Action | Used In | Purpose |
|--------|---------|---------|
| `actions/checkout@v4` | All workflows | Repository checkout |
| `actions/upload-artifact@v4` | release, promote-release | Upload build artifacts |
| `actions/download-artifact@v4` | release, promote-release | Download build artifacts for packaging |
| `dtolnay/rust-toolchain@stable` | All workflows | Install Rust toolchain (1.92.0) |
| `Swatinem/rust-cache@v2` | All workflows | Cargo build/dependency caching |
| `docker/setup-buildx-action@v3` | release, promote-release | Docker Buildx setup |
| `docker/login-action@v3` | release, promote-release | GHCR authentication |
| `docker/build-push-action@v6` | release, promote-release | Multi-platform Docker image build and push |

Equivalent allowlist patterns:

- `actions/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`
- `docker/*`

## Workflows

| Workflow | File | Trigger |
|----------|------|---------|
| CI | `.github/workflows/ci.yml` | Pull requests to `master` |
| Daily Beta Release | `.github/workflows/release.yml` | Daily schedule (08:00 UTC) + manual `workflow_dispatch` |
| Promote Release | `.github/workflows/promote-release.yml` | Manual `workflow_dispatch` |

## Change Control

Record each policy change with:

- change date/time (UTC)
- actor
- reason
- allowlist delta (added/removed patterns)
- rollback note

Use these commands to export the current effective policy:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

## Guardrails

- Any PR that adds or changes `uses:` action sources must include an allowlist impact note.
- New third-party actions require explicit maintainer review before allowlisting.
- Expand allowlist only for verified missing actions; avoid broad wildcard exceptions.

## Change Log

- 2026-03-05: Complete workflow overhaul — replaced 22 workflows with 3 (CI, Beta Release, Promote Release)
    - Removed patterns no longer in use: `DavidAnson/markdownlint-cli2-action@*`, `lycheeverse/lychee-action@*`, `EmbarkStudios/cargo-deny-action@*`, `rustsec/audit-check@*`, `rhysd/actionlint@*`, `sigstore/cosign-installer@*`, `Checkmarx/vorpal-reviewdog-github-action@*`, `useblacksmith/*`
    - Added: `Swatinem/rust-cache@*` (replaces `useblacksmith/*` rust-cache fork)
    - Retained: `actions/*`, `dtolnay/rust-toolchain@*`, `softprops/action-gh-release@*`, `docker/*`
- 2026-03-05: CI build optimization — added mold linker, cargo-nextest, CARGO_INCREMENTAL=0
    - sccache removed due to fragile GHA cache backend causing build failures
- 2026-03-07: Release pipeline overhaul
    - Removed: `softprops/action-gh-release@*` (replaced with built-in `gh` CLI)
    - Beta trigger changed from push-on-master to daily schedule + workflow_dispatch
    - Added default-branch guard on beta workflow_dispatch
    - Added build targets: `armv7-unknown-linux-gnueabihf`, `x86_64-apple-darwin` (cross-compiled from macos-14)

## Rollback

Emergency unblock path:

1. Temporarily set Actions policy back to `all`.
2. Restore selected allowlist after identifying missing entries.
3. Record incident and final allowlist delta.
