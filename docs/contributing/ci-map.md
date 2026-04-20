# CI Workflow Map

What each GitHub workflow does, when it runs, and whether it blocks merges.

Last updated: **May 2026** (post-v0.7.4 cleanup).

For event-by-event delivery behavior, see
[`.github/workflows/master-branch-flow.md`](../../.github/workflows/master-branch-flow.md).

---

## Merge-Blocking

### `ci.yml` — CI

**Trigger:** `pull_request` → `master`

**Required status check:** `CI Required Gate`

The single source of truth for PR quality. Jobs:

- `lint` — `cargo fmt --all -- --check`, `cargo clippy -D warnings`,
  `cargo check --features ci-all`, strict delta lint on changed lines (PRs only).
- `build` — matrix across `x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`,
  `x86_64-pc-windows-msvc`.
- `check` — matrix: all features + no default features.
- `check-32bit` — `i686-unknown-linux-gnu`, no default features.
- `bench` — benchmarks compile check.
- `test` — `cargo nextest run --locked` on `ubuntu-latest`.
- `security` — `cargo deny check`.
- `gate` — `CI Required Gate`: composite job; branch protection requires this.
  Passes only if all upstream jobs passed.

`lint` must pass before `build`, `check`, `check-32bit`, `bench`, `test`, and
`security` start.

**Concurrency:** in-flight runs are cancelled on new PR pushes.

---

## Non-Blocking — Release

### `release-stable-manual.yml` — Release Stable

**Trigger:** `workflow_dispatch` (version input), tag push `v[0-9]+.[0-9]+.[0-9]+`

**Frozen:** do not extend until v0.7.5.

Full stable release pipeline. Jobs in order:

1. `validate` — semver format, `Cargo.toml` version match, tag uniqueness guard.
2. `web` — builds the web dashboard artifact.
3. `release-notes` — generates release notes from git log.
4. `build` — 7-target cross-platform matrix (Linux x86\_64, Linux aarch64,
   ARMv7, ARM, macOS aarch64, Android (experimental), Windows x86\_64).
5. `build-desktop` — macOS universal Tauri app.
6. `publish` — GitHub Release, `SHA256SUMS`, `install.sh`. Removes
   `CHANGELOG-next.md` from master after release.
7. `crates-io` — publishes workspace crates to crates.io.
8. `docker` — pushes multi-arch images to GHCR.
9. Distribution: `scoop`, `aur`, `homebrew`, `marketplace`.
10. Announcements: `discord`, `tweet`.

`publish`, `crates-io`, and `docker` are gated by GitHub environment
protection rules requiring maintainer approval.

See [`docs/maintainers/release-runbook.md`](../maintainers/release-runbook.md)
for the step-by-step procedure.

---

## Non-Blocking — Utilities

### `cross-platform-build-manual.yml` — Cross-Platform Build

**Trigger:** `workflow_dispatch`

Manual build-only smoke check across targets not covered by the PR matrix.
No tests. No publish. Used to verify cross-compilation health before a release
or after a significant dependency change.

### `pr-path-labeler.yml` — PR Path Labeler

**Trigger:** `pull_request` lifecycle events

Applies scope and path labels to PRs automatically based on changed files.
All action refs are SHA-pinned. Low noise, no side effects beyond label
application.

---

## Frozen Sub-Workflows

Called by `release-stable-manual.yml`. Do not modify or extend until v0.7.5.
Each can also be triggered manually with `dry_run: true` for verification.

| File | Purpose |
|---|---|
| `pub-aur.yml` | Pushes updated PKGBUILD to AUR |
| `pub-homebrew-core.yml` | Opens Homebrew Core formula bump PR via bot account |
| `pub-scoop.yml` | Updates Scoop bucket manifest |
| `publish-crates.yml` | Manual crates.io publish (backup for the release pipeline) |
| `discord-release.yml` | Posts release announcement to Discord |
| `tweet-release.yml` | Posts release announcement to Twitter/X |
| `sync-marketplace-templates.yml` | Updates Dokploy and EasyPanel marketplace templates |

---

## Fast Triage

1. **`CI Required Gate` red on a PR** — start with the `lint` job (fmt/clippy
   failures are most common), then `test`, then `build`.
2. **Release `validate` failed** — `Cargo.toml` version does not match the
   workflow input, or the tag already exists.
3. **Release build failed** — check the specific target's job log. Android is
   `experimental` and runs with `continue-on-error`.
4. **Environment gate timed out** — re-run only the timed-out job from the
   workflow run page.
5. **Distribution channel failed** — re-run the corresponding sub-workflow
   manually with `dry_run: true` first.

---

## Maintenance Rules

- Keep `CI Required Gate` deterministic and small. Do not add jobs to the gate
  without a clear quality argument.
- All third-party action refs must be pinned to a full commit SHA
  (see [`actions-source-policy.md`](./actions-source-policy.md)).
- Keep `ci.yml`, `dev/ci.sh`, and `.githooks/pre-push` aligned — the same
  quality gates should run locally and in CI.
- Do not modify the frozen sub-workflows until the v0.7.5 structured release
  pipeline work begins.
- `docs-quality` checks are not in the required gate. They can be run locally
  with `bash scripts/ci/docs_quality_gate.sh`.