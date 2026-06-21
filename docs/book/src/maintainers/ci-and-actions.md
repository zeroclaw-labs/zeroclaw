# CI & Actions

Every workflow lives in `.github/workflows/`. The sections below group them by trigger: automatic on git events, or maintainer-invoked/advisory workflows via `workflow_dispatch` and schedules.

## Automatic workflows

### Quality Gate (`ci.yml`)

Fires on every PR targeting `master` and on trusted pushes to `master`.
Composite job with multiple matrix legs:

- **fmt**: `cargo fmt --all -- --check`
- **lint**: `cargo clippy --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings`, plus two architecture guards (`cargo test --test architecture`): config-write isolation and Fluent coverage (no bare user-facing strings)
- **build**: matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
- **check**: all features + no-default-features
- **check-32bit**: `i686-unknown-linux-gnu` with no default features
- **bench**: benchmarks compile check
- **test**: `cargo nextest run --locked --workspace --exclude zeroclaw-desktop` on Linux
- **security**: `cargo deny check`
- **nix-eval**: evaluates the NixOS module assertions (`nixos-module-eval` flake check)
- **docs-style**: markdown lint + em-dash prose check via `scripts/ci/docs_quality_gate.sh`

`fmt` runs first as the cheap serial gate. Every other job declares `needs: [fmt]` and fans out after formatting passes; `CI Required Gate` aggregates every result. Branch protection pins the composite gate job. A PR cannot merge until this is green. The `master` push run keeps the same quality signal while seeding trusted Rust caches for later PR runs.

### Daily Advisory Scan (`daily-audit.yml`)

Runs `cargo deny check advisories` daily at 09:00 UTC against the dependency tree. Opens an issue on findings. No action unless a vulnerability is reported.

### PR Path Labeler (`pr-path-labeler.yml`)

Auto-applies path and scope labels based on changed files. It runs on PR open, reopen, and every pushed update to the PR branch. Because `sync-labels: true` is enabled, labels defined in `.github/labeler.yml` are recalculated from the current PR file set.

This workflow does not currently apply `risk:*`, `size:*`, `type:*`, contributor-tier, status, resolution, stale, or pickup labels. If a PR is missing a path/scope label, check whether the paths in `.github/labeler.yml` cover the changes.

Dependabot has separate label configuration in `.github/dependabot.yml` for its own PRs. Cargo update PRs start with `dependencies`; GitHub Actions and Docker update PRs start with `ci` and `dependencies`.

### Validate PR title (`pr-title.yml`)

Runs on every PR open/edit/synchronize. Runs the validator unit tests (`scripts/check-pr-title.test.sh`) and checks the PR title against Conventional Commits (`scripts/check-pr-title.sh`).

### Deploy mdBook docs to Pages (`docs-deploy.yml`)

Triggered on tag push (and `workflow_dispatch`); builds and publishes versioned docs to the `gh-pages` branch. See [Release Runbook → Versioned documentation deployment](./release-runbook.md#step-7-versioned-documentation-deployment) for the version-floor and bootstrap rules.

### Docker Image PR Check (`docker-image-pr.yml`)

Runs only when Docker image or release-Docker context files change. It prepares a smoke `docker-ctx` with the same helper used by the stable release workflow, then builds the default and Debian compatibility images without pushing either image. This catches image dependency and `COPY` path breakage before release without giving PR runs registry write permission or running on every PR.

### Discord Release (`discord-release.yml`)

Fires after a successful stable release. Posts the release notes to the community Discord.

### Tweet Release (`tweet-release.yml`)

Fires after a successful stable release. Posts an announcement tweet.

Docs are built and published as part of the release pipeline rather than on every `master` push. Translation is a local-only workflow: run `cargo mdbook sync --provider <name>` for dedicated translation-cache PRs, new locales, and release translation passes. Routine English docs PRs may defer broad generated `.po` churn. See [Docs & Translations](./docs-and-translations.md) for details.

## Manual and Advisory Workflows

### Cross-Platform Build (`cross-platform-build-manual.yml`)

Manual trigger for building release binaries across the full target matrix: Linux x86_64/aarch64 GNU plus armv7 and arm hard-float, macOS Intel/ARM, Windows x86_64, and `aarch64-linux-android` (built with the NDK). Use this to verify a branch compiles cleanly on non-Linux targets before tagging.

### Cross-Platform Clippy (`cross-platform-clippy.yml`)

Manual and weekly scheduled advisory lint coverage on macOS aarch64 and Windows x86_64 targets. It mirrors the required PR lint command with `--target` set for each platform, but intentionally does not run on PRs and is not part of `CI Required Gate`.

### Release Stable (`release-stable-manual.yml`)

Manual trigger for the full release pipeline. Builds all targets, creates the GitHub Release, pushes Docker images to GHCR, triggers the website redeploy, and invokes the distribution sub-workflows (Scoop, AUR, Homebrew, Discord, tweet). Two environment gates require maintainer approval mid-run: `github-releases` (the `publish` job) and `docker`.

See the [Release Runbook](./release-runbook.md) for the full procedure.

### Package Publishers

Each fires on `workflow_dispatch` with a version input. They are also invoked from the release workflow after a successful publish.

| Workflow | What it does |
|---|---|
| `pub-aur.yml` | Updates the Arch User Repository `PKGBUILD` and pushes to the AUR |
| `pub-homebrew-core.yml` | Opens a PR against `homebrew/homebrew-core` with the new version |
| `pub-scoop.yml` | Updates the Scoop manifest for Windows |

## Required secrets

| Secret | Used by |
|---|---|
| `AUR_SSH_KEY` | `pub-aur.yml` |
| `DISCORD_WEBHOOK_URL` | `discord-release.yml` |
| `TWITTER_ACCESS_TOKEN`, `TWITTER_ACCESS_TOKEN_SECRET`, `TWITTER_CONSUMER_API_KEY`, `TWITTER_CONSUMER_API_SECRET_KEY` | `tweet-release.yml` |
| `HOMEBREW_CORE_BOT_TOKEN`, `HOMEBREW_UPSTREAM_PR_TOKEN` | `pub-homebrew-core.yml` |
| `SCOOP_BUCKET_TOKEN` | `pub-scoop.yml` |
| `WEBSITE_REPO_PAT` | `release-stable-manual.yml` (triggers the website repo redeploy) |
| `GITHUB_TOKEN` (automatic) | All workflows that push commits, open PRs, or push images to GHCR |

Docker images push to GHCR using the automatic `GITHUB_TOKEN`; there is no separate registry token. The release pipeline does not publish to crates.io, so no `CARGO_REGISTRY_TOKEN` is required.

## Build cache behavior

Most Rust-heavy jobs in `ci.yml` use `Swatinem/rust-cache@v2`. The `fmt`, `nix-eval`, and `docs-style` jobs (none of which compile the workspace) do not. These behaviors are worth knowing when triaging cache-related flakes:

- **Cache writes are master-only.** `save-if` is conditioned on `github.ref == 'refs/heads/master'`, so PR runs read the master-seeded cache but never update it. PR branches can't pollute the shared cache with branch-specific artifacts. The `push` trigger on `master` is what gives the workflow a trusted cache-writing run after merges.
- **Cache saves on failure.** `cache-on-failure: true` is set on every job, so a partial run still seeds the next attempt warm.
- **Windows build cache is enabled.** The Windows build leg runs the same pinned Rust cache action as Linux and macOS. If Windows cache behavior flakes or regresses, revert the workflow change and document the failing restore/save evidence in the cache issue.
- **Incremental compilation is disabled.** `CARGO_INCREMENTAL: 0` at the workflow level. Incremental builds inflate cache size and produce non-reproducible artifacts under partial-stale conditions.
- **`cargo-deny` and `cargo-nextest` are installed fresh each run.** The `security` job runs `cargo install cargo-deny --locked`; the `test` job pulls the `cargo-nextest` binary from `get.nexte.st`. Neither is cached, so both add a fixed install cost to every run. Switching either to `taiki-e/install-action` would let them be cached, but that action is not in the allowlist today.

## When the gate goes red

| Symptom | First thing to check |
|---|---|
| `CI Required Gate` red | Start with `fmt`, then `lint`, then `test`, then `build` |
| Release `validate` failed | `Cargo.toml` version doesn't match the workflow input, or the tag already exists |
| Release build leg failed | The specific target's job log. Android is `experimental` and runs with `continue-on-error` |
| Environment gate timed out | Re-run only the timed-out job from the workflow run page |
| Distribution publisher failed | Re-run the corresponding sub-workflow manually with `dry_run: true` first |

## Allowed actions

The repository runs Actions in `selected` mode, only the actions in this allowlist may run. The allowlist must stay tight; new third-party actions need explicit maintainer approval before being added.

All third-party refs are pinned to a full commit SHA with a trailing version comment; the version column below records that comment.

| Action | Used in | Purpose |
|---|---|---|
| `actions/checkout` (`v6.0.2`) | Most workflows | Repository checkout |
| `actions/cache` (`v5.0.5`) | `tweet-release.yml` | Generic dependency caching |
| `actions/setup-node` (`v6.4.0`) | `release-stable-manual.yml`, `cross-platform-build-manual.yml` | Node toolchain for the web-dashboard build |
| `actions/upload-artifact` (`v7.0.1`) | `release-stable-manual.yml`, `cross-platform-build-manual.yml` | Upload build artifacts |
| `actions/download-artifact` (`v8.0.1`) | `release-stable-manual.yml`, `cross-platform-build-manual.yml` | Download build artifacts for packaging |
| `actions/labeler` (`v6.1.0`) | `pr-path-labeler.yml` | Apply path/scope labels from `.github/labeler.yml` |
| `dtolnay/rust-toolchain` (`stable`) | `ci.yml`, `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `cross-platform-clippy.yml`, `daily-audit.yml`, `docs-deploy.yml` | Install Rust toolchain |
| `Swatinem/rust-cache` (`v2.9.1`) | `ci.yml`, `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `cross-platform-clippy.yml`, `docs-deploy.yml` | Cargo build/dependency caching |
| `docker/setup-buildx-action` (`v4.0.0`) | `release-stable-manual.yml` | Docker Buildx setup |
| `docker/login-action` (`v4.1.0`) | `release-stable-manual.yml` | GHCR authentication |
| `docker/build-push-action` (`v7.1.0`) | `release-stable-manual.yml` | Multi-platform image build and push |

The GitHub Release itself is created with `gh release create` inside the `publish` job, not a release action.

Equivalent allowlist patterns (kept narrow on purpose):

```
actions/*
dtolnay/rust-toolchain@*
Swatinem/rust-cache@*
docker/*
```

Export the current effective policy:

<div class="os-tabs-src">

#### sh

```sh
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

</div>

Any PR that adds or changes a `uses:` action source must include an allowlist impact note in its body. Avoid broad wildcard exceptions; expand the allowlist only for verified missing actions.

## Maintenance rules

- Keep `CI Required Gate` deterministic and small. Adding jobs to the gate needs a clear quality argument.
- All third-party action refs must be pinned to a full commit SHA (per the allowlist policy above).
- Keep `ci.yml`, `dev/ci.sh`, and `.githooks/pre-push` aligned, the same quality gates run locally and in CI.
- Keep `scripts/ci/prepare_docker_context.sh`, `docker-image-pr.yml`, and the Docker job in `release-stable-manual.yml` aligned so PR validation exercises the same context shape the release workflow publishes.
- The `docs-style` gate job runs `bash scripts/ci/docs_quality_gate.sh` (markdown lint + em-dash prose check). Run the same script locally before pushing docs changes.

## Emergency rollback

If the allowlist locks out a critical action mid-incident:

1. Temporarily set Actions policy back to `all`.
2. Restore `selected` allowlist after identifying the missing entry.
3. Record the incident and the final allowlist delta.

This is the only justified path to `all` mode, and it should never outlast the incident.
