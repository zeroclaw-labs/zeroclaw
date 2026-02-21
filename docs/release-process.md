# ZeroClaw Release Process

This runbook defines the maintainers' standard release flow.

Last verified: **February 21, 2026**.

## Release Goals

- Keep releases predictable and repeatable.
- Publish only from code already in `main`.
- Verify multi-target artifacts before publish.
- Keep release cadence regular even with high PR volume.

## Standard Cadence

- Patch/minor releases: weekly or bi-weekly.
- Emergency security fixes: out-of-band.
- Never wait for very large commit batches to accumulate.

## Workflow Contract

Release automation lives in:

- `.github/workflows/pub-release.yml`
- `.github/workflows/pub-homebrew-core.yml` (manual Homebrew formula PR, bot-owned)

Modes:

- Tag push `v*`: publish mode.
- Manual dispatch: verification-only or publish mode.
- Weekly schedule: verification-only mode.

Publish-mode guardrails:

- Tag must match semver-like format `vX.Y.Z[-suffix]`.
- Tag must already exist on origin.
- Tag commit must be reachable from `origin/main`.
- Matching GHCR image tag (`ghcr.io/<owner>/<repo>:<tag>`) must be available before GitHub Release publish completes.
- Artifacts are verified before publish.

## Maintainer Procedure

### 1) Preflight on `main`

1. Ensure required checks are green on latest `main`.
2. Confirm no high-priority incidents or known regressions are open.
3. Confirm installer and Docker workflows are healthy on recent `main` commits.

### 2) Run verification build (no publish)

Run `Pub Release` manually:

- `publish_release`: `false`
- `release_ref`: `main`

Expected outcome:

- Full target matrix builds successfully.
- `verify-artifacts` confirms all expected archives exist.
- No GitHub Release is published.

### 3) Cut release tag

From a clean local checkout synced to `origin/main`:

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

This script enforces:

- clean working tree
- `HEAD == origin/main`
- non-duplicate tag
- semver-like tag format

### 4) Monitor publish run

After tag push, monitor:

1. `Pub Release` publish mode
2. `Pub Docker Img` publish job

Expected publish outputs:

- release archives
- `SHA256SUMS`
- `CycloneDX` and `SPDX` SBOMs
- cosign signatures/certificates
- GitHub Release notes + assets

### 5) Post-release validation

1. Verify GitHub Release assets are downloadable.
2. Verify GHCR tags for the released version (`vX.Y.Z`) and release commit SHA tag (`sha-<12>`).
3. Verify install paths that rely on release assets (for example bootstrap binary download).

### 6) Publish Homebrew Core formula (bot-owned)

Run `Pub Homebrew Core` manually:

- `release_tag`: `vX.Y.Z`
- `dry_run`: `true` first, then `false`

Required repository settings for non-dry-run:

- secret: `HOMEBREW_CORE_BOT_TOKEN` (token from a dedicated bot account, not a personal maintainer account)
- variable: `HOMEBREW_CORE_BOT_FORK_REPO` (for example `zeroclaw-release-bot/homebrew-core`)
- optional variable: `HOMEBREW_CORE_BOT_EMAIL`

Workflow guardrails:

- release tag must match `Cargo.toml` version
- formula source URL and SHA256 are updated from the tagged tarball
- formula license is normalized to `Apache-2.0 OR MIT`
- PR is opened from the bot fork into `Homebrew/homebrew-core:master`

## Emergency / Recovery Path

If tag-push release fails after artifacts are validated:

1. Fix workflow or packaging issue on `main`.
2. Re-run manual `Pub Release` in publish mode with:
   - `publish_release=true`
   - `release_tag=<existing tag>`
   - `release_ref` is automatically pinned to `release_tag` in publish mode
3. Re-validate released assets.

## Operational Notes

- Keep release changes small and reversible.
- Prefer one release issue/checklist per version so handoff is clear.
- Avoid publishing from ad-hoc feature branches.
