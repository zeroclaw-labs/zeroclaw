# CI & Actions

Every workflow lives in `.github/workflows/`. The sections below group them by trigger — automatic on git events, or manual via `workflow_dispatch`.

## Automatic workflows

### Quality Gate (`ci.yml`)

Fires on every PR targeting `master`. Composite job with multiple matrix legs:

- **lint** — `cargo fmt --check`, `cargo clippy --workspace --exclude zeroclaw-desktop --all-targets --features ci-all -- -D warnings`
- **build** — matrix: `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`
- **check** — all features + no-default-features
- **check-32bit** — `i686-unknown-linux-gnu` with no default features
- **bench** — benchmarks compile check
- **test** — `cargo nextest run --locked` on Linux
- **security** — `cargo deny check`

`CI Required Gate` is the composite job branch protection pins. A PR cannot merge until this is green.

### Daily Advisory Scan (`daily-audit.yml`)

Runs `cargo audit` nightly against the dependency tree. Opens an issue on findings. No action unless a vulnerability is reported.

### PR Path Labeler (`pr-path-labeler.yml`)

Auto-applies scope and risk labels based on changed file paths. Runs silently on every PR — if a PR is missing labels, check whether the paths in `.github/labeler.yml` cover the changes.

### Discord Release (`discord-release.yml`)

Fires after a successful stable release. Posts the release notes to the community Discord.

### Tweet Release (`tweet-release.yml`)

Fires after a successful stable release. Posts an announcement tweet.

### Sync Marketplace Templates (`sync-marketplace-templates.yml`)

Fires after every stable release. Auto-opens PRs to update version numbers in the downstream marketplace template repos (docker, k8s, compose).

Docs are built and published as part of the release pipeline rather than on every `master` push. Translation is a local-only workflow — run `cargo mdbook sync --provider <name>` before PRing. See [Docs & Translations](./docs-and-translations.md) for details.

## Manual workflows

### Cross-Platform Build (`cross-platform-build-manual.yml`)

Manual trigger for building release binaries across the full target matrix (Linux GNU/MUSL, macOS Intel/ARM, Windows, additional ARM Linux targets). Use this to verify a branch compiles cleanly on non-Linux targets before tagging.

### Release Stable (`release-stable-manual.yml`)

Manual trigger for the full release pipeline. Builds all targets, creates the GitHub Release, publishes to crates.io, pushes Docker images, and invokes downstream workflows. Three environment gates require maintainer approval mid-run: `github-releases`, `crates-io`, `docker`.

See the release runbook in the repo's `docs/maintainers/` directory for the full procedure (not yet migrated into this mdBook).

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
| `TWITTER_*` tokens | `tweet-release.yml` |
| `HOMEBREW_CORE_TOKEN` | `pub-homebrew-core.yml` |
| `CARGO_REGISTRY_TOKEN` | `release-stable-manual.yml` |
| `DOCKER_HUB_TOKEN` | `release-stable-manual.yml` |
| `GITHUB_TOKEN` (automatic) | All workflows that push commits or open PRs |

## Build cache behavior

Every job in `ci.yml` uses `Swatinem/rust-cache@v2`. Three behaviors are worth knowing when triaging cache-related flakes:

- **Cache writes are master-only.** `save-if` is conditioned on `github.ref == 'refs/heads/master'`, so PR runs read the master-seeded cache but never update it. PR branches can't pollute the shared cache with branch-specific artifacts.
- **Cache saves on failure.** `cache-on-failure: true` is set on every job, so a partial run still seeds the next attempt warm.
- **Windows has no Rust cache.** `if: runner.os != 'Windows'` skips the cache step on the Windows leg — `rust-cache`'s path handling poisons on Windows. Windows always runs cold.
- **Incremental compilation is disabled.** `CARGO_INCREMENTAL: 0` at the workflow level. Incremental builds inflate cache size and produce non-reproducible artifacts under partial-stale conditions.
- **`cargo-deny` is not cached.** The `security` job installs it fresh from source on every run. A future improvement is `taiki-e/install-action`, which already caches `cargo-nextest`.

## When the gate goes red

| Symptom | First thing to check |
|---|---|
| `CI Required Gate` red | Start with `lint` (fmt/clippy is the most common cause), then `test`, then `build` |
| Release `validate` failed | `Cargo.toml` version doesn't match the workflow input, or the tag already exists |
| Release build leg failed | The specific target's job log. Android is `experimental` and runs with `continue-on-error` |
| Environment gate timed out | Re-run only the timed-out job from the workflow run page |
| Distribution publisher failed | Re-run the corresponding sub-workflow manually with `dry_run: true` first |

## Allowed actions

The repository runs Actions in `selected` mode — only the actions in this allowlist may run. The allowlist must stay tight; new third-party actions need explicit maintainer approval before being added.

| Action | Used in | Purpose |
|---|---|---|
| `actions/checkout@v4` | All workflows | Repository checkout |
| `actions/upload-artifact@v4` | release | Upload build artifacts |
| `actions/download-artifact@v4` | release | Download build artifacts for packaging |
| `actions/labeler@v5` | `pr-path-labeler.yml` | Apply path/scope labels from `.github/labeler.yml` |
| `dtolnay/rust-toolchain@stable` | All workflows | Install Rust toolchain |
| `Swatinem/rust-cache@v2` | All workflows | Cargo build/dependency caching |
| `softprops/action-gh-release@v2` | release | Create GitHub Releases |
| `docker/setup-buildx-action@v3` | release | Docker Buildx setup |
| `docker/login-action@v3` | release | GHCR authentication |
| `docker/build-push-action@v6` | release | Multi-platform image build and push |

Equivalent allowlist patterns (kept narrow on purpose):

```
actions/*
dtolnay/rust-toolchain@*
Swatinem/rust-cache@*
softprops/action-gh-release@*
docker/*
```

Export the current effective policy:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

Any PR that adds or changes a `uses:` action source must include an allowlist impact note in its body. Avoid broad wildcard exceptions; expand the allowlist only for verified missing actions.

## Maintenance rules

- Keep `CI Required Gate` deterministic and small. Adding jobs to the gate needs a clear quality argument.
- All third-party action refs must be pinned to a full commit SHA (per the allowlist policy above).
- Keep `ci.yml`, `dev/ci.sh`, and `.githooks/pre-push` aligned — the same quality gates run locally and in CI.
- `docs-quality` checks are not in the required gate. Run them locally with `bash scripts/ci/docs_quality_gate.sh`.

## Emergency rollback

If the allowlist locks out a critical action mid-incident:

1. Temporarily set Actions policy back to `all`.
2. Restore `selected` allowlist after identifying the missing entry.
3. Record the incident and the final allowlist delta.

This is the only justified path to `all` mode — and it should never outlast the incident.
