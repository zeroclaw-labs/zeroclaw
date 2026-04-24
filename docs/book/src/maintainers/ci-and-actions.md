# CI & Actions

## Automatic workflows

### Quality Gate (`ci.yml`)

Fires on every PR targeting `master`. Two sequential stages:

1. **Lint** — `cargo fmt --check` + `cargo clippy --features ci-all -D warnings`. Fails fast before burning compute on the build stage.
2. **Build** — cross-platform matrix build. Runs only if lint passes.

A PR cannot merge until this workflow is green.

### Deploy Docs (`docs-deploy.yml`)

Fires automatically on push to `master` (and `feat/fluent-i18n` during development) when any of the following change: `docs/book/**`, `src/**`, `crates/**`, `Cargo.toml`, `Cargo.lock`.

What it does:

1. Generates `docs/book/src/reference/cli.md` and `config.md` from the live code
2. Generates the rustdoc API reference
3. Validates `.po` format — **fails the deploy if any `.po` file is malformed**
4. Builds mdBook for every locale in `locales.toml` → `docs/book/book/{locale}/`
5. Assembles the Pages artifact (rustdoc + locale redirect `index.html`)
6. Deploys to GitHub Pages

**If it fails:** most common causes are a malformed `.po` file (run `cargo mdbook check` locally) or a broken reference page (run `cargo mdbook refs` and check for panics in `markdown-help` or `markdown-schema`).

### Daily Audit (`daily-audit.yml`)

Runs `cargo audit` nightly against the dependency tree. Opens an issue on findings.

### PR Path Labeler (`pr-path-labeler.yml`)

Auto-applies scope and risk labels based on changed file paths. No action needed — runs silently on every PR.

## Manual workflows

### Translate Docs (`docs-translate.yml`)

Run this before a release to AI-fill any untranslated or fuzzy strings in the `.po` files. It commits updated `.po` files back to the current branch, which then triggers `docs-deploy.yml`.

**Trigger:** Actions → *Translate docs (manual)* → Run workflow

| Input | Options | Default | Notes |
|---|---|---|---|
| `locales` | Space-separated codes | all (from `locales.toml`) | Leave blank for all configured locales |
| `force` | true / false | false | Re-translate everything, not just the delta |
| `model` | haiku / sonnet / opus | `claude-haiku-4-5-20251001` | Haiku is fast and cheap for delta fills; use sonnet or opus for release quality passes |

Requires the `ANTHROPIC_API_KEY` repository secret to be set (CI uses Anthropic; local fills use `--provider`).

**Expected output:** the workflow commits a `chore(i18n): sync translations via <model>` commit and pushes it. If all strings were already translated, it exits cleanly with "No translation changes".

### Cross-Platform Build (`cross-platform-build-manual.yml`)

Manual trigger for building release binaries across all target platforms. Use this to verify a branch builds cleanly on non-Linux targets before tagging.

### Release (`release-stable-manual.yml`)

Manual trigger for the full release pipeline. See the release checklist before running.

## Required secrets

| Secret | Used by |
|---|---|
| `ANTHROPIC_API_KEY` | `docs-translate.yml` |
| `GITHUB_TOKEN` (automatic) | `docs-translate.yml` (push), `docs-deploy.yml` (Pages) |
