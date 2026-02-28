# ZeroClaw Release Process

This runbook defines the maintainers' standard release flow.

Last verified: **February 25, 2026**.

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

Modes:

- Tag push `v*`: publish mode.
- Manual dispatch: verification-only or publish mode.
- Weekly schedule: verification-only mode.
- Pre-release tags (`vX.Y.Z-alpha.N`, `vX.Y.Z-beta.N`, `vX.Y.Z-rc.N`): prerelease publish path.
- Canary gate (weekly/manual): promote/hold/abort decision path.

Publish-mode guardrails:

- Tag must match stable format `vX.Y.Z` (pre-release tags are handled by `Pub Pre-release`).
- Tag must already exist on origin.
- Tag must be annotated (lightweight tags are rejected).
- Tag commit must be reachable from `origin/main`.
- Publish trigger actor must be in `RELEASE_AUTHORIZED_ACTORS` allowlist.
- Optional tagger-email allowlist can be enforced via `RELEASE_AUTHORIZED_TAGGER_EMAILS`.
- Matching GHCR image tag (`ghcr.io/<owner>/<repo>:<tag>`) must be available before GitHub Release publish completes.
- Artifacts are verified before publish.
- Trigger provenance is recorded in `release-trigger-guard.json` and `audit-event-release-trigger-guard.json`.
- Multi-arch artifact contract is enforced by `.github/release/release-artifact-contract.json` through `release_artifact_guard.py`.
- Release notes include a generated supply-chain evidence preface (`release-notes-supply-chain.md`) plus GitHub-generated commit-window notes.

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
- `verify-artifacts` enforces archive completeness against `.github/release/release-artifact-contract.json`.
- No GitHub Release is published.
- `release-trigger-guard` artifact is emitted with authorization/provenance evidence.
- `release-artifact-guard-verify` artifact is emitted with `release-artifact-guard.verify.json`, `release-artifact-guard.verify.md`, and `audit-event-release-artifact-guard-verify.json`.

### 3) Cut release tag

From a clean local checkout synced to `origin/main`:

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

This script enforces:

- clean working tree
- `HEAD == origin/main`
- non-duplicate tag
- stable semver tag format (`vX.Y.Z`)

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
- `release-artifact-guard.publish.json` + `release-artifact-guard.publish.md`
- `audit-event-release-artifact-guard-publish.json` proving publish-stage artifact contract completeness
- `zeroclaw.sha256sums.intoto.json` + `audit-event-release-sha256sums-provenance.json` for checksum provenance linkage
- `release-notes-supply-chain.md` / `release-notes-supply-chain.json` with release-asset references (manifest, SBOM, provenance, guard audit artifacts)
- Docker publish evidence from `Pub Docker Img`: `ghcr-publish-contract.json` + `audit-event-ghcr-publish-contract.json` + `ghcr-vulnerability-gate.json` + `audit-event-ghcr-vulnerability-gate.json` + Trivy reports

### 5) Post-release validation

1. Verify GitHub Release assets are downloadable.
2. Verify GHCR tags for the released version (`vX.Y.Z`), release commit SHA tag (`sha-<12>`), and `latest`.
3. Verify GHCR digest parity evidence confirms:
   - `digest(vX.Y.Z) == digest(sha-<12>)`
   - `digest(latest) == digest(vX.Y.Z)`
4. Verify GHCR vulnerability gate evidence (`ghcr-vulnerability-gate.json`) reports `ready=true` and that `audit-event-ghcr-vulnerability-gate.json` is present.
5. Verify install paths that rely on release assets (for example bootstrap binary download).

### 5.1) Canary gate before broad rollout

Run `CI Canary Gate` (`.github/workflows/ci-canary-gate.yml`) in `dry-run` first, then `execute` when metrics are complete.

Required inputs:

- candidate tag/SHA
- observed error rate
- observed crash rate
- observed p95 latency
- observed sample size

Decision output:

- `promote`: thresholds satisfied
- `hold`: insufficient evidence or soft breach
- `abort`: hard threshold breach

Abort integration:

- In `execute` mode, if decision is `abort` and `trigger_rollback_on_abort=true`, canary gate dispatches `CI Rollback Guard` automatically.
- Default rollback branch is `dev` (override with `rollback_branch`).
- Optional explicit rollback target can be passed via `rollback_target_ref`.

### 5.2) Pre-release stage progression (alpha/beta/rc/stable policy)

For staged release confidence:

1. Cut and push stage tag (`vX.Y.Z-alpha.N`, then beta, then rc).
2. `Pub Pre-release` validates:
   - stage progression
   - stage matrix completeness (`alpha|beta|rc|stable` policy coverage)
   - monotonic same-stage numbering
   - origin/main ancestry
   - Cargo version/tag alignment
3. Guard artifacts publish transition audit evidence and stage history:
   - `transition.type` / `transition.outcome`
   - `transition.previous_highest_stage` and `transition.required_previous_tag`
   - `stage_history.per_stage` and `stage_history.latest_stage`
4. Publish prerelease assets only after guard passes.

## Emergency / Recovery Path

If tag-push release fails after artifacts are validated:

1. Fix workflow or packaging issue on `main`.
2. Re-run manual `Pub Release` in publish mode with:
   - `publish_release=true`
   - `release_tag=<existing tag>`
   - `release_ref` is automatically pinned to `release_tag` in publish mode
3. Re-validate released assets.

If prerelease/canary lanes fail:

1. Inspect guard artifacts (`prerelease-guard.json`, `canary-guard.json`).
2. For prerelease failures, inspect `transition` + `stage_history` fields first to classify promotion, stage iteration, or demotion-blocked attempts.
3. Fix stage-policy or quality regressions.
4. Re-run guard in `dry-run` before any execute/publish action.

## Operational Notes

- Keep release changes small and reversible.
- Prefer one release issue/checklist per version so handoff is clear.
- Avoid publishing from ad-hoc feature branches.
