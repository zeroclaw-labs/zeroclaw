# CI & Actions

Every workflow lives in `.github/workflows/`. The sections below group them by trigger: automatic on git events, or maintainer-invoked/advisory workflows via `workflow_dispatch` and schedules.

## Automatic workflows

### Quality Gate (`ci.yml`)

Fires on every PR targeting `master` and on trusted pushes to `master`.
Composite job with multiple matrix legs:

- **fmt**: `cargo fmt --all -- --check`
- **lint**: `cargo clippy --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings`, then `cargo doc --no-deps --workspace --exclude zeroclaw-desktop` (rustdoc warnings are fatal via `.cargo/config.toml` `build.rustdocflags`; desktop is excluded to match `xtask build_api` / docs-deploy and avoid GTK/`glib-sys` on the lint runner), and the comment hygiene gate
- **build**: matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
- **check**: three warnings-fatal passes over the workspace (excluding `zeroclaw-desktop`): all features; no default features; and default features with `--all-targets`, which is the only leg that compiles test targets on the default feature surface
- **check-32bit**: `i686-unknown-linux-gnu` with no default features
- **bench**: benchmarks compile check
- **test**: the standalone firmware protocol host gate from `scripts/ci/firmware_protocol_gate.sh` and `cargo nextest run --locked --workspace --exclude zeroclaw-desktop` on Linux, including the config-write isolation and Fluent coverage (no bare user-facing strings) architecture guards
- **parallel-runtime-test**: repeated same-process runtime/channel tests from `scripts/ci/parallel_runtime_test_gate.sh`, run in parallel with the main test job for relevant PR paths and unconditionally on `master` pushes and merge queue runs
- **security**: `cargo deny check`
- **nix-eval**: evaluates the NixOS module assertions (`nixos-module-eval` flake check)
- **docs-style**: markdown lint, em-dash prose check, and changed-line link gate via `scripts/ci/docs_quality_gate.sh` and `scripts/ci/docs_links_gate.sh`

`fmt` runs first as the cheap serial gate. Every other job declares `needs: [fmt]` directly or transitively and fans out after formatting passes; `CI Required Gate` aggregates every result. Branch protection pins the composite gate job. A PR cannot merge until this is green. The `master` push run keeps the same quality signal while seeding trusted Rust caches for later PR runs.

Fresh required CI is normally the shared evidence for the Cargo surfaces it actually runs. A local rerun of the same Cargo command on the same head, target, and feature set is duplicate confidence, not a stronger proof. Before asking for extra Cargo or Clippy, compare the changed surface with the current workflow files and the actual checks on the PR. Extra validation belongs where the required gate does not prove the thing under review:

- a platform received compile checks but not tests;
- a platform, crate, or path is outside the required lint job;
- a desktop change did not trigger the desktop workflow;
- a release target is outside the PR matrix and only covered by release/manual workflows;
- stale, cancelled, skipped, or unavailable CI is not fresh evidence.

When a definition or import is feature-gated, compare its `cfg` predicate with every consumer. Validate both the enabled configuration and each relevant disabled configuration: an enabled-feature pass proves the consumer still works, while the workspace-wide no-default-features check catches warning-producing mismatches such as unused private definitions or imports. That pass runs `cargo check` without `--all-targets`, so it never compiles test targets: a helper gated on plain `test` whose only callers sit behind a feature is caught by the default-features/all-targets leg instead. Targeted feature combinations remain necessary when neither required CI configuration exercises the changed predicate.

### Scheduled Platform Tests (`platform-tests.yml`)

Runs `cargo nextest run --locked --workspace --exclude zeroclaw-desktop --no-fail-fast` on `macos-14` and `windows-latest` after a cheap Linux formatting check. The matrix runs for:

- pull requests that change `platform-tests.yml` itself;
- manual dispatches; and
- the nightly 03:17 UTC schedule.

The jobs use `continue-on-error` and do not feed `CI Required Gate`. They are portability evidence, not merge requirements. Ordinary code PRs do not launch the matrix automatically; maintainers can manually dispatch it against a branch when focused platform proof is useful. The workflow does not run for ordinary `push` or `merge_group` events. Nightly and manually dispatched runs on `master` can write trusted caches; pull-request runs cannot. `--no-fail-fast` keeps every platform failure visible in a single run.

### Daily Advisory Scan (`daily-audit.yml`)

Runs `cargo deny check advisories` daily at 09:00 UTC against the dependency tree. Opens an issue on findings. No action unless a vulnerability is reported.

### Daily npm Audit (`daily-npm-audit.yml`)

Runs `npm audit --audit-level=high` daily at 09:23 UTC against `web/package-lock.json`. Opens one deduplicated `security` + `dependencies` issue when high-severity npm advisories affect the committed web lockfile.

### Weekly Trivy Image Scan (`trivy-scheduled.yml`)

Scans the published `dist` and `default-features` GHCR images every Saturday and uploads HIGH/CRITICAL findings to the Security tab as SARIF. The scan is report-first (`exit-code: 0` for findings), but a missing expected image fails the job before Trivy setup with the absent tag and the owning publisher workflow named in the error.

### Weekly Scoop Bucket Canary (`scoop-bucket-canary.yml`)

Rehearses the Scoop publish path against the current stable release every Monday. It resolves the latest `vX.Y.Z` tag and calls `pub-scoop.yml` with both `dry_run: true` and `credential_canary: true`, so it exercises the real `SCOOP_BUCKET_TOKEN` against the real bucket without writing anything.

`credential_canary` is the fail-closed part of that contract: a missing `SCOOP_BUCKET_REPO` or `SCOOP_BUCKET_TOKEN` fails the run, and configured credentials must reach the `git push --dry-run` authorization probe. A generic manual `pub-scoop.yml` run with only `dry_run: true` remains permissive for manifest generation and may skip that probe when credentials are unavailable; do not use the generic mode as credential-verification evidence.

This exists because `SCOOP_BUCKET_TOKEN` is account-bound: it expires, and it silently loses write when the owning identity's collaborator grant on the bucket changes. Both have happened. Before the canary, the only thing that exercised the credential was the post-publish `scoop` job, so a dead token was discovered after the release was already cut and announced, and the bucket had to be updated by hand.

The canary detects credential rot. It is deliberately not what keeps the bucket correct, and it is not wired into Release Stable: a dead package-manager credential must never gate or delay a release.

#### How the Scoop bucket stays correct

Today the release publisher is the only automated writer:

1. **`pub-scoop.yml` pushes on release.** Scoop users see the new version immediately when this succeeds. It needs the cross-repo `SCOOP_BUCKET_TOKEN`, which is the fragile part.
2. **Maintainers recover failed pushes.** Rotate or repair the token, dispatch Scoop Bucket Canary to verify it through the fail-closed `credential_canary` path, rerun the publisher with `dry_run: false`, and confirm the bucket manifest landed the release version.

A bucket-side Excavator is proposed in [scoop-zeroclaw#1](https://github.com/zeroclaw-labs/scoop-zeroclaw/pull/1). Once that workflow is merged, the bucket repository grants Actions read/write workflow permission, and a maintainer smoke test proves that it commits an update, it can become a credential-independent recovery layer. Until all three conditions are satisfied, do not assume a failed publisher will self-heal.

The `checkver` and `autoupdate` blocks are already load-bearing for the planned Excavator path. The current push path also uses `scripts/release/scoop_metadata.sh` to derive its release URL template from `autoupdate`, so both paths share one manifest contract. Do not remove those blocks, and do not hand-edit them out of `dist/scoop/zeroclaw.json`.

### PR Path Labeler (`pr-path-labeler.yml`)

Auto-applies path and scope labels based on changed files. It runs on PR open, reopen, and every pushed update to the PR branch. Because `sync-labels: true` is enabled, labels defined in `.github/labeler.yml` are recalculated from the current PR file set.

This workflow does not currently apply `risk:*`, `size:*`, `type:*`, contributor-tier, status, resolution, stale, or pickup labels. If a PR is missing a path/scope label, check whether the paths in `.github/labeler.yml` cover the changes.

Dependabot has separate label configuration in `.github/dependabot.yml` for its own PRs. Cargo update PRs start with `dependencies`; GitHub Actions and Docker update PRs start with `ci` and `dependencies`.

### Project Dashboard Planner (`project-dashboard-plan.yml`)

Runs manually for a single issue number. It reads issue state and labels, then writes a report-only step summary proposing the existing Project Status value that best matches the issue.

This workflow does not run automatically on issue events, write ProjectV2 fields, edit issues, add labels, post comments, or recalculate PR `risk:*`, `size:*`, or `type:*` labels. Live ProjectV2 mutation or automatic issue-event planning needs a separately approved field mapping, trigger policy, and project-scoped credential.

### Validate PR title (`pr-title.yml`)

Runs on every PR open/edit/synchronize. Runs the validator unit tests (`scripts/check-pr-title.test.sh`) and checks the PR title against Conventional Commits (`scripts/check-pr-title.sh`).

### Deploy mdBook docs to Pages (`docs-deploy.yml`)

Triggered on tag push (and `workflow_dispatch`); builds and publishes versioned docs to the `gh-pages` branch. See [Release Runbook → Versioned documentation deployment](./release-runbook.md#step-7-versioned-documentation-deployment) for the version-floor and bootstrap rules.

### Docker Image PR Check (`docker-image-pr.yml`)

Runs only when Docker image, Compose, or release-Docker context files change. It validates the merged default-plus-Alpine Compose configuration and, for changes beyond Compose-only edits, builds the default and Debian prebuilt smoke images plus the source Dockerfiles without pushing them. The default and Alpine source images build for `linux/amd64` and `linux/arm64`; the Debian source image builds for `linux/amd64`. Separate Alpine and Debian `linux/amd64` lanes enable `plugins-wasm-runtime-only` so their builder contexts continuously prove that the repository WIT contract is available to plugin-enabled source builds.

The all-features `Containerfile` source image builds for `linux/amd64` when that file or the Docker workflow changes. It uses an isolated cache scope and is neither loaded nor pushed. The Alpine amd64 lane runs both binaries, starts the built image through the merged Compose configuration, and checks the gateway health and dashboard surfaces. The Alpine arm64 lane is compile- and image-assembly coverage only. Compose-only changes use a reduced Alpine amd64 matrix so they still exercise the runtime contract without rebuilding unrelated images. All jobs have read-only repository permissions and no registry write permission.

### Docker Publish (`docker-publish.yml`)

Builds, signs, and scans the generated four-variant matrix from `dev/ci/docker-tags.toml`: `minimal`, `default-features`, `dist`, and `all-features`. A human-created `v*` tag starts this workflow directly. A stable release started with `workflow_dispatch` creates its tag with `GITHUB_TOKEN`, which does not emit another tag-push event, so `release-stable-manual.yml` calls Docker Publish synchronously at the immutable release tag after the canonical release and Docker jobs succeed.

This matrix supplements rather than replaces the stable release's prebuilt `latest`, versioned, and `debian` images. The two paths use different build inputs and publish distinct tags.

### Discord Release (`discord-release.yml`)

Fires after a successful stable release. Posts the release notes to the community Discord.

### Tweet Release (`tweet-release.yml`)

Fires after a successful stable release. Posts an announcement tweet.

### Weekly AUR Freshness Check (`aur-freshness-check.yml`)

Compares the published `zeroclawlabs` AUR version against the current stable GitHub release every Monday, and fails if the AUR is behind.

Publishing to the AUR is fire-and-forget: if `pub-aur.yml` fails, nothing re-checks, so the package silently falls behind. That is exactly what happened after v0.8.4. An `aur.archlinux.org` maintenance window overlapped the release, the single unretried clone failed with `The AUR is down due to maintenance`, and the package sat three weeks behind with no signal. The publisher now allows at most one active non-dry-run publish and retries to survive a short outage; GitHub may supersede an earlier queued real publish in the same concurrency group, while dry runs use a separate group. Every attempt reclones the authoritative package state and refuses to replace a newer `epoch:pkgver-pkgrel` tuple with an older one. A retry budget still cannot cover every failure, so this check is the backstop that turns a silent miss or superseded run into a visible one.

If the AUR RPC is unreachable the check warns and passes rather than failing. An AUR outage is an upstream availability problem, not package staleness, and the next scheduled run re-checks. Staleness is durable, so a delayed detection is acceptable; a weekly page about someone else's maintenance window is not.

Docs are built and published as part of the release pipeline rather than on every `master` push. Translation is a local-only workflow for dedicated translation-cache PRs, new locales, and release translation passes. Routine English docs PRs may defer broad generated `.po` churn. See [Docs & Translations](./docs-and-translations.md) for contributor guidance and the [Release Runbook](./release-runbook.md#refresh-and-pin-translations) for the release procedure.

## Manual and Advisory Workflows

### Monthly Outdated Scan (`monthly-outdated.yml`)

Scheduled monthly scan on the 1st of every month at 09:00 UTC. Runs `cargo outdated --workspace` across all workspace members. Opens a `dependencies`-labeled issue when stale deps are found. Permissions: `contents: read` + `issues: write`. Dedup guard prevents piling up if the previous issue is still open.

First triage step for a new issue: check if the reported outdated crates have semver-incompatible bumps and whether the consuming crate's API changed. If the bump is trivial (patch/minor), create a short dep-only PR. If the upgrade is blocked by semver breaks, close the issue with a note and the blocking crate name.

### Cross-Platform Build (`cross-platform-build-manual.yml`)

Manual trigger for building release binaries across the full target matrix: Linux x86_64/aarch64 GNU and MUSL plus armv7 and arm hard-float, macOS Intel/ARM, Windows x86_64, and `aarch64-linux-android` (built with the NDK). Use this to verify a branch compiles cleanly on non-Linux targets before tagging.

Every dispatch also runs a small release-tool smoke matrix independently of the builds. Set `release_tools_only` when only this evidence is needed; the web and release-build jobs are then skipped. On trusted GitHub-hosted Linux x86_64, the smoke installs the pinned `cross` archive, confirms both `cross` and `cross-util`, and records `cross --version`. On trusted GitHub-hosted Windows x86_64, it uses the same Rust version and Bash-to-Cargo path shape as the stable release workflow, then records both `cargo-tauri.exe --version` and `cargo tauri --version`. Each leg records the exact tested commit and runner architecture in the public job summary. The smoke uses read-only repository permissions and has no publishing job, environment, secret, or artifact upload.

MUSL build legs also install `cross` through `scripts/ci/install_release_tool.sh`, which downloads the exact pinned upstream release asset and verifies its SHA-256 before installing it. The required Repository Structure job tests the supported runner-to-asset mapping and the smoke workflow contract without making network calls.

### Cross-Platform Clippy (`cross-platform-clippy.yml`)

Manual and weekly scheduled advisory lint coverage on macOS aarch64 and Windows x86_64 targets. It mirrors the required PR lint command with `--target` set for each platform, but intentionally does not run on PRs and is not part of `CI Required Gate`.

Required Linux Clippy, advisory cross-platform Clippy, and targeted Windows Clippy call `scripts/ci/run_clippy.sh`. That runner owns the supported command shapes, Cargo exit-status propagation, and the shared duration, cache, compile-count, and download-count diagnostics. The workflow files continue to own triggers, runners, toolchains, caches, timeouts, and required-gate membership.

### Release Stable (`release-stable-manual.yml`)

Manual trigger for the full release pipeline. Builds all targets, creates the GitHub Release, pushes the prebuilt `latest`, versioned, and `debian` Docker images to GHCR, calls the generated Docker variant matrix at the release tag, triggers the website redeploy, and invokes the distribution sub-workflows (Scoop, AUR, Discord, tweet). Homebrew Core detects new releases through its own autobump service. Two environment gates require maintainer approval mid-run: `github-releases` (the `publish` job) and `docker`.

Downloadable assets use GitHub-hosted Build Level 2 attestations. Offline
bundles and trusted-root material ship inside one verification archive, and
both SBOM formats are checksummed and attested before the release is created.
Cosign remains limited to GHCR image signing.

See the [Release Runbook](./release-runbook.md) for the full procedure.

Release-only build tools do not compile from source on every run. The workflow
installs pinned upstream `cross` and Tauri CLI release binaries through
`scripts/ci/install_release_tool.sh`; that script verifies a repository-owned
SHA-256 for each runner-specific archive before placing the binary in Cargo's
bin directory. Updating either tool requires updating its version, asset name,
and checksum together, then running `scripts/ci/install_release_tool.test.sh`.

### Package Publishers

Each fires on `workflow_dispatch` with a version input. They are also invoked from the release workflow after a successful publish.

| Workflow | What it does |
|---|---|
| `pub-aur.yml` | Updates the Arch User Repository `PKGBUILD` and pushes to the AUR |
| `pub-scoop.yml` | Updates the Scoop manifest for Windows |

Homebrew Core's
[official autobump service](https://docs.brew.sh/Autobump) discovers stable
GitHub releases and opens formula bumps independently. Do not restore a
project-owned Homebrew publisher or fork token; that duplicates Homebrew's
authoritative automation.

## Required secrets

| Secret | Used by |
|---|---|
| `AUR_SSH_KEY` | `pub-aur.yml` |
| `DISCORD_WEBHOOK_URL` | `discord-release.yml` |
| `TWITTER_ACCESS_TOKEN`, `TWITTER_ACCESS_TOKEN_SECRET`, `TWITTER_CONSUMER_API_KEY`, `TWITTER_CONSUMER_API_SECRET_KEY` | `tweet-release.yml` |
| `SCOOP_BUCKET_TOKEN` | `pub-scoop.yml`, `release-stable-manual.yml`, `scoop-bucket-canary.yml`; fine-grained PAT limited to `zeroclaw-labs/scoop-zeroclaw` with Contents read/write |
| `WEBSITE_REPO_PAT` | `release-stable-manual.yml` (triggers the website repo redeploy) |
| `GITHUB_TOKEN` (automatic) | All workflows that push commits, open PRs, or push images to GHCR |

Docker images push to GHCR using the automatic `GITHUB_TOKEN`; there is no separate registry token. The release pipeline does not publish to crates.io, so no `CARGO_REGISTRY_TOKEN` is required.

The organization currently disables deploy keys on the Scoop bucket, and the
automatic `GITHUB_TOKEN` cannot write another repository. Keep
`SCOOP_BUCKET_TOKEN` narrowly scoped to the bucket; do not reuse a maintainer's
broad CLI token. The publisher checks write access with `git push --dry-run`,
then uses the same Git transport for the real update.

### Rotating `SCOOP_BUCKET_TOKEN`

Because deploy keys are unavailable, this credential is a personal access token
and therefore has two independent failure modes, both of which have bitten a
release:

1. **The token expires.** Fine-grained PATs have a maximum lifetime, so this
   recurs on a fixed schedule whether or not anything else changes.
2. **The owning identity loses write on the bucket.** The token can still be
   valid while the account behind it is only a `read` collaborator. This
   produces `remote: Permission to zeroclaw-labs/scoop-zeroclaw.git denied to
   <account>` and HTTP 403, not an auth error, so it reads as a code problem
   when it is a permissions problem.

Own the token with the `ZeroClaw-Bot` account, never a personal account, so the
release path does not depend on one maintainer's credentials. To rotate:

1. As `ZeroClaw-Bot`, create a fine-grained PAT with **Resource owner**
   `zeroclaw-labs`, **Repository access** limited to the single repository
   `zeroclaw-labs/scoop-zeroclaw`, and **Repository permissions → Contents:
   Read and write**. Nothing else.
2. Confirm the org approved the token. Fine-grained PATs against an org
   resource owner stay pending until approved, and a pending token authenticates
   but cannot push.
3. Confirm `ZeroClaw-Bot` still has `write` on the bucket:
   `gh api repos/zeroclaw-labs/scoop-zeroclaw/collaborators/ZeroClaw-Bot/permission --jq '.role_name'`.
   Step 1 does not grant repository access; it only scopes what the token may
   use. A token cannot exceed the permissions its owner already holds.
4. Set the secret:
   `gh secret set SCOOP_BUCKET_TOKEN --repo zeroclaw-labs/zeroclaw`.
5. Verify without touching the bucket by dispatching
   [Scoop Bucket Canary](#weekly-scoop-bucket-canary-scoop-bucket-canaryyml).
   A green run proves the new token can push.

Record the expiry date somewhere durable when you rotate. The canary will catch
an expired token within a week regardless, but only after it has already broken.

### AUR package ownership

The project-owned package is currently
[`zeroclawlabs`](https://aur.archlinux.org/packages/zeroclawlabs), maintained by
`zeroclaw-bot`. The canonical-name
[`zeroclaw`](https://aur.archlinux.org/packages/zeroclaw) package is a
third-party package and cannot be taken over by rotating `AUR_SSH_KEY`. If that
maintainer remains inactive, follow the
[AUR orphan-request process](https://wiki.archlinux.org/title/AUR_submission_guidelines#Requests)
before changing `pkgname` or the workflow clone target. After ownership
transfers, coordinate the package rename or merge in one reviewed change.

## Build cache behavior

Most Rust-heavy jobs in `ci.yml` cache through the local `./.github/actions/rust-cache` composite, which selects the cache backend from the same `CI_USE_BLACKSMITH` toggle that selects the runner: `useblacksmith/rust-cache` (Blacksmith NVMe sticky disk) when the job runs on a Blacksmith runner, and `Swatinem/rust-cache` otherwise. Any non-`true` toggle value (including unset, and every fork PR) falls back to `Swatinem/rust-cache` on GitHub-hosted runners, so caching is never lost when Blacksmith is off. Both action references live in the composite regardless of the toggle, so both must stay in the allowlist. The macOS and Windows build legs stay on `Swatinem/rust-cache`, and the `fmt`, `nix-eval`, and `docs-style` jobs (none of which compile the workspace) use no Rust cache. These behaviors are worth knowing when triaging cache-related flakes:

- **Cache writes are master-only.** `save-if` is conditioned on `github.ref == 'refs/heads/master'`, so PR runs read the master-seeded cache but never update it. PR branches can't pollute the shared cache with branch-specific artifacts. The `push` trigger on `master` is what gives the workflow a trusted cache-writing run after merges.
- **Cache saves on failure.** `cache-on-failure: true` is set on every job, so a partial run still seeds the next attempt warm.
- **Windows build cache is enabled.** The Windows build leg runs the same pinned Rust cache action as Linux and macOS. If Windows cache behavior flakes or regresses, revert the workflow change and document the failing restore/save evidence in the cache issue.
- **Incremental compilation is disabled.** `CARGO_INCREMENTAL: 0` at the workflow level. Incremental builds inflate cache size and produce non-reproducible artifacts under partial-stale conditions.
- **`cargo-deny` and `cargo-nextest` are installed fresh each run.** The `security` job runs `cargo install cargo-deny --locked`; the Linux `test` job and both scheduled `platform-tests.yml` legs pull the appropriate `cargo-nextest` binary from `get.nexte.st`. Neither tool is cached, so each install adds a fixed cost to its job. Switching either to `taiki-e/install-action` would let them be cached, but that action is not in the allowlist today.

## When the gate goes red

| Symptom | First thing to check |
|---|---|
| `Release Stable` dies at `startup_failure` with zero jobs after a `uses:` ref changed | Check the run summary and repository Actions policy. If GitHub reports a selected-actions rejection, compare the changed ref with the [allowlist](#allowed-actions), add only the rejected pattern, wait for settings propagation, then dispatch a fresh run. Otherwise, investigate the workflow definition or other repository policy; `startup_failure` alone does not identify the cause |
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
| `actions/cache` (`v4.2.3`, `v5.0.5`) | `docker-image-pr.yml`, `tweet-release.yml` | Generic dependency and Trivy database caching |
| `actions/setup-node` (`v7.0.0`) | `ci-sbom.yml`, `ci.yml`, `cross-platform-build-manual.yml`, `daily-npm-audit.yml`, `release-stable-manual.yml` | Node toolchain for npm SBOM generation, web tests/audit, and web/desktop builds |
| `actions/upload-artifact` (`v7.0.1`) | `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `docker-publish.yml`, `trivy-scheduled.yml` | Upload build artifacts and Trivy SARIF handoff artifacts |
| `actions/download-artifact` (`v8.0.1`) | `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `docker-publish.yml` | Download build artifacts and Trivy SARIF handoff artifacts |
| `actions/attest` (`v4.2.2`) | `release-stable-manual.yml` | Generate GitHub-hosted Build Level 2 provenance for release assets |
| `actions/labeler` (`v6.1.0`) | `pr-path-labeler.yml` | Apply path/scope labels from `.github/labeler.yml` |
| `dtolnay/rust-toolchain` (`stable`, `v1`) | `ci.yml`, `platform-tests.yml`, `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `cross-platform-clippy.yml`, `daily-audit.yml`, `docs-deploy.yml`, `codeql.yml` | Install Rust toolchain |
| `Swatinem/rust-cache` (`v2.9.2`) | `ci.yml` (GitHub-hosted path of `./.github/actions/rust-cache`), `platform-tests.yml`, `release-stable-manual.yml`, `cross-platform-build-manual.yml`, `cross-platform-clippy.yml`, `docs-deploy.yml` | Cargo build/dependency caching on GitHub-hosted runners |
| `useblacksmith/rust-cache` (`v3.0.1`) | `ci.yml` (Blacksmith path of `./.github/actions/rust-cache`) | Cargo build/dependency caching on Blacksmith sticky disk; selected only when `CI_USE_BLACKSMITH=true` |
| `docker/setup-buildx-action` (`v3.11.1`, `v4.0.0`) | `release-stable-manual.yml`, `docker-publish.yml` | Docker Buildx setup |
| `docker/login-action` (`v3.4.0`, `v4.1.0`) | `release-stable-manual.yml`, `docker-publish.yml`, `trivy-scheduled.yml` | GHCR authentication |
| `docker/build-push-action` (`v6.18.0`, `v7.1.0`) | `release-stable-manual.yml`, `docker-publish.yml` | Multi-platform image build and push |
| `sigstore/cosign-installer` (`v3.8.1`) | `release-stable-manual.yml`, `docker-publish.yml` | Install cosign for keyless GHCR container-image signing |
| `anchore/sbom-action` (`v0.24.0`) | `release-stable-manual.yml` | Generate SPDX + CycloneDX SBOMs for each release |
| `aquasecurity/trivy-action` (`v0.36.0`) | `docker-image-pr.yml`, `docker-publish.yml`, `trivy-scheduled.yml` | Report-only container vulnerability scanning |
| `github/codeql-action/upload-sarif` (`v3.36.2`) | `docker-publish.yml`, `trivy-scheduled.yml`, `ci-code-analysis.yml` | Upload Trivy and Semgrep SARIF reports to the Security tab |
| `github/codeql-action/init` (`v3.36.2`) | `codeql.yml` | Initialize CodeQL analysis (Rust and JS/TS) |
| `github/codeql-action/analyze` (`v3.36.2`) | `codeql.yml` | Upload CodeQL SARIF to the Security tab |

The GitHub Release itself is created with `gh release create` inside the `publish` job, not a release action.

Equivalent allowlist patterns (kept narrow on purpose):

```
actions/*
dtolnay/rust-toolchain@*
Swatinem/rust-cache@*
useblacksmith/rust-cache@*
docker/*
sigstore/cosign-installer@*
anchore/sbom-action@*
aquasecurity/trivy-action@*
github/codeql-action/upload-sarif@*
github/codeql-action/init@*
github/codeql-action/analyze@*
```

Export the current effective policy:

<div class="os-tabs-src">

### sh

```sh
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

</div>

Any PR that adds or changes a `uses:` action source must include an allowlist impact note in its body. Avoid broad wildcard exceptions; expand the allowlist only for verified missing actions.

## Maintenance rules

- Keep `CI Required Gate` deterministic and small. Adding jobs to the gate needs a clear quality argument.
- All third-party action refs must be pinned to a full commit SHA (per the allowlist policy above).
- Keep `ci.yml`, `dev/ci.sh`, and `.githooks/pre-push` aligned. Shared gates must live in `scripts/ci/`; each caller invokes the helper instead of copying its commands. For the standalone firmware protocol gate, the documented local entry point is `./dev/ci.sh firmware-protocol`.
- Keep `scripts/ci/prepare_docker_context.sh`, `docker-image-pr.yml`, and the Docker job in `release-stable-manual.yml` aligned so PR validation exercises the same context shape the release workflow publishes.
- Run `python3 scripts/ci/release_attestation_contract_test.py` after changing the release attestation, checksum, SBOM, or verification-archive sequence.
- The `docs-style` gate job runs `bash scripts/ci/docs_quality_gate.sh` (markdown lint + em-dash prose check) and `bash scripts/ci/docs_links_gate.sh` (changed-line link gate). Run both scripts locally before pushing docs changes.

## Emergency rollback

If the allowlist locks out a critical action mid-incident:

1. Temporarily set Actions policy back to `all`.
2. Restore `selected` allowlist after identifying the missing entry.
3. Record the incident and the final allowlist delta.

This is the only justified path to `all` mode, and it should never outlast the incident.
