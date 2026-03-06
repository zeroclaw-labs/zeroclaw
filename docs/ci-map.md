# CI Workflow Map

This document explains what each GitHub workflow does, when it runs, and whether it should block merges.

For event-by-event delivery behavior across PR, merge, push, and release, see [`.github/workflows/master-branch-flow.md`](../.github/workflows/master-branch-flow.md).

## Workflows

### CI (`.github/workflows/ci.yml`)

- **Trigger:** pull requests to `master`
- **Purpose:** run tests and build release binaries on Linux and macOS
- **Jobs:**
    - `test` — `cargo nextest run --locked` with mold linker
    - `build` — release build matrix (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`)
- **Merge gate:** both `test` and `build` must pass before merge

### Beta Release (`.github/workflows/release.yml`)

- **Trigger:** push to `master` (every merged PR)
- **Purpose:** build, package, and publish a beta pre-release with Docker image
- **Jobs:**
    - `version` — compute `vX.Y.Z-beta.<run_number>` from `Cargo.toml`
    - `build` — 5-target release matrix (linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64)
    - `publish` — create GitHub pre-release with archives + SHA256SUMS
    - `docker` — build and push multi-platform Docker image to GHCR (`beta` + version tag)

### CI Full Matrix (`.github/workflows/ci-full.yml`)

- **Trigger:** manual `workflow_dispatch` only
- **Purpose:** build release binaries on additional targets not covered by the PR CI
- **Jobs:**
    - `build` — 3-target matrix (`aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `x86_64-pc-windows-msvc`)
- **Note:** useful for verifying cross-compilation before a stable release

### Promote Release (`.github/workflows/promote-release.yml`)

- **Trigger:** manual `workflow_dispatch` with `version` input (e.g. `0.2.0`)
- **Purpose:** build, package, and publish a stable (non-pre-release) GitHub release with Docker image
- **Jobs:**
    - `validate` — confirm input version matches `Cargo.toml`, confirm tag does not already exist
    - `build` — same 5-target matrix as Beta Release
    - `publish` — create GitHub stable release with archives + SHA256SUMS
    - `docker` — build and push multi-platform Docker image to GHCR (`latest` + version tag)

## Trigger Map

| Workflow | Trigger |
|----------|---------|
| CI | Pull requests to `master` |
| Beta Release | Push to `master` |
| CI Full Matrix | Manual dispatch only |
| Promote Release | Manual dispatch only |

## Fast Triage Guide

1. **CI failing on PR:** inspect `.github/workflows/ci.yml` — check `test` and `build` job logs.
2. **Beta release failing after merge:** inspect `.github/workflows/release.yml` — check `version`, `build`, `publish`, and `docker` job logs.
3. **Promote release failing:** inspect `.github/workflows/promote-release.yml` — check `validate` job (version/tag mismatch) and `build`/`publish`/`docker` jobs.
4. **Cross-platform build issues:** run CI Full Matrix manually via `.github/workflows/ci-full.yml` to test additional targets.

## Maintenance Rules

- Keep merge-blocking checks deterministic and reproducible (`--locked` where applicable).
- Follow `docs/release-process.md` for release cadence and version discipline.
- Prefer explicit workflow permissions (least privilege).
- Keep Actions source policy restricted to approved allowlist patterns (see `docs/actions-source-policy.md`).
