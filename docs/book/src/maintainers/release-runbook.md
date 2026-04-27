# Release Runbook

The standard procedure for cutting a stable release. Predictable, repeatable, and only ever from `master`.

## When to release

- **Patch / minor**: weekly or bi-weekly cadence. Don't wait for huge commit batches to accumulate.
- **Emergency security fixes**: out-of-band, as needed.
- The release pipeline never publishes from a feature branch.

## Workflow surface

Release automation lives in:

| File | Role |
|---|---|
| `release-stable-manual.yml` | Full stable release pipeline. Tag-push and `workflow_dispatch` triggers. |
| `pub-aur.yml` | Pushes the updated PKGBUILD to AUR (workflow_call from release; manual dispatch with `dry_run`). |
| `pub-homebrew-core.yml` | Opens a PR against `Homebrew/homebrew-core` from the bot fork. |
| `pub-scoop.yml` | Updates the Scoop bucket manifest. |
| `discord-release.yml` | Posts the release notes to Discord `#releases` after publish. |
| `tweet-release.yml` | Posts an announcement tweet. |
| `sync-marketplace-templates.yml` | Updates downstream marketplace templates (Dokploy, EasyPanel) after publish. |

`release-stable-manual.yml` runs in two modes:

- **Tag push** (`v*` tag) — full publish.
- **Manual dispatch** — verification-only or publish, controlled by `publish_release` input.

A weekly schedule also fires the workflow in verification-only mode, so you'll see green CI on `master` even between releases.

## Publish-mode guardrails

The pipeline refuses to publish unless every guardrail holds:

- Tag matches semver-like `vX.Y.Z[-suffix]`.
- Tag exists on `origin`.
- Tag commit is reachable from `origin/master`.
- Matching GHCR image tag (`ghcr.io/<owner>/<repo>:<tag>`) is available before the GitHub Release is finalized.
- All artifact files exist before the Release is published.

If any of these fail, the run halts mid-pipeline and reports the specific gate.

## Procedure

### 1. Preflight on `master`

- Required checks are green on the latest `master` commit.
- No high-priority incidents or known regressions are open.
- Installer and Docker workflows are healthy on recent `master` commits.

### 2. Verification run (no publish)

Trigger `release-stable-manual.yml` manually with:

- `publish_release: false`
- `release_ref: master`

Expected outcome:

- Full target matrix builds.
- `verify-artifacts` confirms every expected archive exists.
- No GitHub Release is published.

### 3. Cut the release tag

From a clean local checkout synced to `origin/master`:

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

The script enforces:

- Clean working tree.
- `HEAD == origin/master`.
- Non-duplicate tag.
- Semver-like tag format.

### 4. Monitor the publish run

After the tag push, monitor:

1. `release-stable-manual.yml` (publish mode).
2. The Docker publish job within it.

Expected outputs:

- Release archives.
- `SHA256SUMS`.
- `CycloneDX` and `SPDX` SBOMs.
- `cosign` signatures and certificates.
- GitHub Release notes and assets.

### 5. Post-release validation

- GitHub Release assets are downloadable.
- GHCR tags exist for both `vX.Y.Z` and the release commit (`sha-<12>`).
- Install paths that depend on release assets work end-to-end (the bootstrap installer's binary download is the most common smoke test).

### 6. Publish Homebrew Core formula

Trigger `pub-homebrew-core.yml` manually:

- `release_tag: vX.Y.Z`
- `dry_run: true` first, then `false`

Required configuration for non-dry-run:

- Secret: `HOMEBREW_CORE_BOT_TOKEN` (from a dedicated bot account, not a personal maintainer account).
- Variable: `HOMEBREW_CORE_BOT_FORK_REPO` (e.g. `zeroclaw-release-bot/homebrew-core`).
- Optional: `HOMEBREW_CORE_BOT_EMAIL`.

Workflow guardrails:

- Release tag must match the `Cargo.toml` version.
- Formula source URL and SHA256 are computed from the tagged tarball.
- Formula license is normalized to `Apache-2.0 OR MIT`.
- The PR opens from the bot fork into `Homebrew/homebrew-core:master`.

### 7. Publish Scoop manifest (Windows)

Trigger `pub-scoop.yml`:

- `release_tag: vX.Y.Z`
- `dry_run: true` first, then `false`

Required configuration:

- Secret: `SCOOP_BUCKET_TOKEN` (PAT with push access to the bucket repo).
- Variable: `SCOOP_BUCKET_REPO` (e.g. `zeroclaw-labs/scoop-zeroclaw`).

Workflow guardrails:

- Release tag must be `vX.Y.Z` format.
- Windows binary SHA256 is extracted from the `SHA256SUMS` release asset.
- Manifest pushes to `bucket/zeroclaw.json` in the Scoop bucket repo.

### 8. Publish AUR package (Arch Linux)

Trigger `pub-aur.yml`:

- `release_tag: vX.Y.Z`
- `dry_run: true` first, then `false`

Required configuration:

- Secret: `AUR_SSH_KEY` (SSH private key registered with AUR).

Workflow guardrails:

- Release tag must be `vX.Y.Z` format.
- Source tarball SHA256 is computed from the tagged release.
- PKGBUILD and `.SRCINFO` push to the AUR `zeroclaw` package.

## Recovery: tag-push publish failed after artifacts validated

If artifacts are good but publishing failed:

1. Fix the workflow or packaging issue on `master`.
2. Re-run `release-stable-manual.yml` manually with:
   - `publish_release: true`
   - `release_tag: <existing tag>`
   - (`release_ref` is automatically pinned to `release_tag` in publish mode.)
3. Re-validate the released assets.

## Operational principles

- Keep release changes small and reversible.
- One release issue/checklist per version so handoff between maintainers is clean.
- Never publish from an ad-hoc feature branch.
- Don't extend the release pipeline mid-cycle. Pipeline changes go through their own review and ship in their own release.
