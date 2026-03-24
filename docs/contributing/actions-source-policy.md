# Actions Source Policy

This document defines the current GitHub Actions source-control policy for this repository.

## Current Policy

- Repository Actions permissions: enabled
- Allowed actions mode: selected

Selected allowlist (all actions currently used across Quality Gate, Release Beta, and Release Stable workflows):

| Action | Used In | Purpose |
|--------|---------|---------|
| `actions/checkout@v4` | All workflows | Repository checkout |
| `actions/upload-artifact@v4` | release, promote-release | Upload build artifacts |
| `actions/download-artifact@v4` | release, promote-release | Download build artifacts for packaging |
| `dtolnay/rust-toolchain@stable` | All workflows | Install Rust toolchain (1.92.0) |
| `Swatinem/rust-cache@v2` | All workflows | Cargo build/dependency caching |
| `softprops/action-gh-release@v2` | release, promote-release | Create GitHub Releases |
| `docker/setup-buildx-action@v3` | release, promote-release | Docker Buildx setup |
| `docker/login-action@v3` | release, promote-release | GHCR authentication |
| `docker/build-push-action@v6` | release, promote-release | Multi-platform Docker image build and push |
| `actions/labeler@v5` | pr-path-labeler | Apply path/scope labels from `labeler.yml` |

Equivalent allowlist patterns:

- `actions/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`
- `softprops/action-gh-release@*`
- `docker/*`

## Workflows

| Workflow | File | Trigger |
|----------|------|---------|
| Quality Gate | `.github/workflows/checks-on-pr.yml` | Pull requests to `master` |
| Release Beta | `.github/workflows/release-beta-on-push.yml` | Push to `master` |
| Release Stable | `.github/workflows/release-stable-manual.yml` | Manual `workflow_dispatch` |
| PR Path Labeler | `.github/workflows/pr-path-labeler.yml` | `pull_request_target` (opened, synchronize, reopened) |

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

- 2026-03-23: Added PR Path Labeler (`pr-path-labeler.yml`) using `actions/labeler@v5`. No allowlist change needed — covered by existing `actions/*` pattern.
- 2026-03-10: Renamed workflows — CI → Quality Gate (`checks-on-pr.yml`), Beta Release → Release Beta (`release-beta-on-push.yml`), Promote Release → Release Stable (`release-stable-manual.yml`). Added `lint` and `security` jobs to Quality Gate. Added Cross-Platform Build (`cross-platform-build-manual.yml`).
- 2026-03-05: Complete workflow overhaul — replaced 22 workflows with 3 (CI, Beta Release, Promote Release)
    - Removed patterns no longer in use: `DavidAnson/markdownlint-cli2-action@*`, `lycheeverse/lychee-action@*`, `EmbarkStudios/cargo-deny-action@*`, `rustsec/audit-check@*`, `rhysd/actionlint@*`, `sigstore/cosign-installer@*`, `Checkmarx/vorpal-reviewdog-github-action@*`, `useblacksmith/*`
    - Added: `Swatinem/rust-cache@*` (replaces `useblacksmith/*` rust-cache fork)
    - Retained: `actions/*`, `dtolnay/rust-toolchain@*`, `softprops/action-gh-release@*`, `docker/*`
- 2026-03-05: CI build optimization — added mold linker, cargo-nextest, CARGO_INCREMENTAL=0
    - sccache removed due to fragile GHA cache backend causing build failures

## Rollback

Emergency unblock path:

1. Temporarily set Actions policy back to `all`.
2. Restore selected allowlist after identifying missing entries.
3. Record incident and final allowlist delta.
