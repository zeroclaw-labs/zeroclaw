# ZeroClaw Release Process

This runbook defines the maintainers' standard release flow.

Last verified: **March 6, 2026**.

## Release Goals

- Keep releases predictable and repeatable.
- Publish only from code already in `master`.
- Verify multi-target artifacts before publish.
- Keep release cadence regular even with high PR volume.

## Release Model

ZeroClaw uses a two-tier release model:

- **Beta releases** are automatic — every merge to `master` triggers a beta pre-release (`vX.Y.Z-beta.<run_number>`).
- **Stable releases** are manual — a maintainer runs `Promote Release` via workflow dispatch to cut a non-pre-release version.

## Workflow Contract

Release automation lives in:

- `.github/workflows/release.yml` — automatic beta release on push to `master`
- `.github/workflows/promote-release.yml` — manual stable release via workflow dispatch

### Beta Release (automatic)

- Triggered on every push to `master`.
- Computes version from `Cargo.toml`: `vX.Y.Z-beta.<run_number>`.
- Builds 5-target matrix (linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64).
- Publishes GitHub pre-release with archives + SHA256SUMS.
- Builds and pushes multi-platform Docker image to GHCR (`beta` + version tag).

### Stable Release (manual)

- Triggered via `workflow_dispatch` with a `version` input (e.g. `0.2.0`).
- Validates version matches `Cargo.toml` and tag does not already exist.
- Builds same 5-target matrix as beta.
- Publishes GitHub stable release with archives + SHA256SUMS.
- Builds and pushes Docker image to GHCR (`latest` + version tag).

## Maintainer Procedure

### 1) Preflight on `master`

1. Ensure CI is green on latest `master`.
2. Confirm no high-priority incidents or known regressions are open.
3. Confirm recent beta releases are healthy (check GitHub Releases page).

### 2) Optional: run CI Full Matrix

If verifying cross-compilation before a stable release:

- Run `CI Full Matrix` manually (`.github/workflows/ci-full.yml`).
- Confirms builds succeed on `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, and `x86_64-pc-windows-msvc`.

### 3) Bump version in `Cargo.toml`

1. Create a branch, update `version` in `Cargo.toml` to the target version.
2. Open PR, merge to `master`.
3. The merge triggers a beta release automatically.

### 4) Promote to stable release

1. Run `Promote Release` workflow manually:
   - `version`: the target version (e.g. `0.2.0`) — must match `Cargo.toml`
2. Workflow validates version, builds all targets, and publishes.

### 5) Post-release validation

1. Verify GitHub Release assets are downloadable.
2. Verify GHCR tags for the released version.
3. Verify install paths that rely on release assets (for example bootstrap binary download).

## Standard Cadence

- Patch/minor releases: weekly or bi-weekly.
- Emergency security fixes: out-of-band.
- Beta releases ship continuously with every merge.

## Emergency / Recovery Path

If a stable release fails:

1. Fix the issue on `master`.
2. Re-run `Promote Release` with the same version (if the tag was not yet created).
3. If the tag already exists, bump to the next patch version.

## Operational Notes

- Keep release changes small and reversible.
- Prefer one release issue/checklist per version so handoff is clear.
- Avoid publishing from ad-hoc feature branches.
