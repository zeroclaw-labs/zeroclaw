# Release Runbook

> **Interim manual process.** This runbook covers how to ship a stable release
> today using `release-stable-manual.yml`. It remains the live process until
> release-plz lands and replaces it; that migration has not happened yet (see
> [Where this is going](#where-this-is-going)).
>
> If anything in here feels heavyweight, that is intentional friction, we do
> not yet have the automation discipline to remove it safely.

Last verified against the `0.8.0-beta` cycle.

---

## The process in seven steps

1. [Generate `CHANGELOG-next.md` using the changelog skill](#step-1-generate-changelog-nextmd)
2. [Open and merge a version bump PR](#step-2-bump-and-merge-the-version-pr)
3. [Dry-run the release workflows locally with `act`](#step-3-dry-run-the-release-workflows-locally-with-act)
4. [Trigger the `Release Stable` workflow via manual dispatch](#step-4-trigger-the-release)
5. [Approve the two environment gates when prompted](#step-5-approve-the-environment-gates)
6. [Verify the release exists and assets are downloadable](#step-6-verify-the-release)
7. [Versioned documentation deployment](#step-7-versioned-documentation-deployment)

That is the entire process. Everything else (Docker, website redeploy, Scoop,
AUR, Homebrew, Discord, tweet) runs automatically as downstream jobs. You do not
need to do anything for those unless a job explicitly fails.

---

## Step 1: Generate CHANGELOG-next.md

Run the `changelog-generation` skill to produce `CHANGELOG-next.md`. Its full
procedure lives at `.claude/skills/changelog-generation/SKILL.md`.

The skill generates the changelog from the git log between the last stable tag
and HEAD, resolves contributors via GitHub GraphQL, and writes the file. Commit
the result directly to a short-lived branch and include it in the version bump
PR (step 2), or open it as a separate preceding PR if the diff is large.

If `CHANGELOG-next.md` already exists from a previous aborted release cycle,
review it for accuracy before reusing it.

---

## Step 2: Bump and merge the version PR

Bump `workspace.package.version` in the workspace `Cargo.toml`, then run the two release scripts in order. First sync every version reference across the repo:

<div class="os-tabs-src">

#### sh

```sh
./scripts/release/bump-version.sh    # version from Cargo.toml
```

</div>

This updates README badges, the Tauri config, and workflow description
examples, then regenerates every spec-driven install surface via
`cargo generate installers`: setup.bat, `dist/aur/PKGBUILD`,
`dist/scoop/zeroclaw.json`, `flake.nix`, the Dockerfile/Containerfile feature
sets, and `dev/ci/docker-tags.toml`. Those surfaces derive their version and
feature lists from the canonical install spec (`Cargo.toml` plus
`[package.metadata.zeroclaw]`), so the bump keeps them in step automatically;
never hand-edit a generated region. This script also refreshes the Nix git
dependency hashes (`nix/hashes.json`) via `scripts/dev/refresh-nix-hashes.sh`.

Then refresh the docs translation catalogues and pin them to the matching tag:

<div class="os-tabs-src">

#### sh

```sh
./scripts/release/refresh-translations.sh    # init submodule, refresh catalogues, tag, pin
```

</div>

`refresh-translations.sh` reads the version from `Cargo.toml` (nothing typed by
hand), runs the translation pass, commits and pushes the catalogues to the
[`zeroclaw-labs/zeroclaw-docs-translations`](https://github.com/zeroclaw-labs/zeroclaw-docs-translations)
submodule, cuts the `v{version}` tag there, and stages the main-repo gitlink
pinned to that tag. It initialises the submodule if it is not already checked
out. Run it after `bump-version.sh` so the `Cargo.toml` version it reads is the
release version. Pass `--no-translate` to skip the sync pass when the catalogues
are already current, or an explicit version to override the `Cargo.toml` default.

Commit everything together:

```text
chore: bump version to vX.Y.Z
```

Open a PR. Label it `type:ci`, `size:XS`, and any path labels the PR labeler
adds. Get one maintainer review. Merge when CI is green. The **Installer Drift**
gate in CI fails the PR if a generated surface is out of sync with the spec, so
a missed regeneration cannot land. The
**Validate Translations Pin** gate resolves the submodule at the pinned commit
and validates catalogue format and msgid parity, so a bad pin cannot land
either. See [Docs & Translations](../maintainers/docs-and-translations.md) for
catalogue refresh details.

**Confirm the merge landed correctly:**

<div class="os-tabs-src">

#### sh

```sh
git fetch origin
git show origin/master:Cargo.toml | grep '^version'
# Must show: version = "X.Y.Z"
```

</div>

---

## Step 3: Dry-run the release workflows locally with `act`

The `Release Stable` workflow is a GitHub Actions job graph that consumes
your environment-gate approval window the moment you click **Run workflow**.
If a workflow step is broken: a missing build artifact, a stale path, a
codegen step that someone removed without updating CI, the failure surfaces
*after* you have committed to a release window, with the version PR already
merged and master at the new version. Recovery means landing an emergency
fix branch, re-running CI, and shipping under time pressure on a tree that
already advertises itself as a fully-released version.

The cheap insurance against this is to run the same job graph locally first,
on the exact merged master commit, before opening the GitHub Actions form.
[`act`](https://nektosact.com/) executes GitHub Actions workflows inside
Docker containers using the same `actions/*` ecosystem GitHub does. It does
not perfectly mirror the cloud runner; it cannot reach the artifact upload
runtime, GitHub-issued OIDC tokens, environment secrets, or jobs that depend
on a real release tag, but it does run the build and test steps that
account for nearly every release-time CI failure we have ever hit.

This step is a 15–20 minute investment per release. It has caught real
defects that the regular per-PR CI did not surface (because the failing
workflow only runs on `workflow_dispatch`, not on `push`).

### One-time setup

`act` runs the workflows. The cleanest install path is the GitHub CLI
extension, because it inherits your `gh` authentication and exposes a
real `GITHUB_TOKEN` to every workflow run:

1. Install the GitHub CLI from <https://cli.github.com> (Linux, macOS,
   Windows). Authenticate once: `gh auth login`.
2. Install the `act` extension:

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh extension install nektos/gh-act
   ```

   </div>

3. Install Docker Engine or Docker Desktop from
   <https://docs.docker.com/engine/install/>. On Linux, add yourself to
   the `docker` group so you don't need `sudo`. `act` also works with
   Podman and Colima; see the
   [act runners documentation](https://nektosact.com/usage/runners.html).

That's the whole setup. The repository's `.actrc` and
`scripts/dev/act-local.sh` handle everything else (runner image, secrets
file, artifact server, action SHA pre-fetching).

### Per-release dry-run

Make sure your working tree matches the merged master tip from step 2:

<div class="os-tabs-src">

#### sh

```sh
git fetch upstream
git checkout upstream/master
```

</div>

List what's runnable across every workflow file:

<div class="os-tabs-src">

#### sh

```sh
./scripts/dev/act-local.sh --list
```

</div>

Run a specific job, pick interactively, or run every dry-run-safe
job:

<div class="os-tabs-src">

#### sh

```sh
./scripts/dev/act-local.sh release-stable-manual:web   # one job
./scripts/dev/act-local.sh                              # interactive picker
./scripts/dev/act-local.sh --all                        # every dry-run-safe job
```

</div>

The first run pulls the runner image (~1.5 GB) and primes the Rust build
cache via `Swatinem/rust-cache`; subsequent runs are much faster. The
script auto-creates the gitignored `.secrets` file, pre-fetches every
pinned action SHA into `~/.cache/act/` (act's shallow clone can't
resolve arbitrary commits otherwise), threads `GITHUB_TOKEN` from your
`gh` auth into the run via the parent process environment (the token
value never lands in argv), and sets `--artifact-server-path` so
`actions/upload-artifact` and `actions/download-artifact` work between
jobs. All of that is plain `act` underneath; the script just removes
the flag soup.

### `--all` only runs jobs on a dry-run-safe allowlist

`act` does **not** honor GitHub's environment-protection gates. With
the maintainer's real `GITHUB_TOKEN` threaded into the run, a
successful local invocation of a job that writes to GitHub (a `publish`
that calls `gh release create`, a `docker` job that pushes to GHCR, a
`docs-deploy` that force-pushes `gh-pages`, a `daily-audit` that opens
an issue, a `tweet-release` or `discord-release` that posts to a
webhook) could perform the real-world side effect on first try.

`--all` therefore enforces a hardcoded allowlist of jobs proven safe
to run locally; currently the artifact-only build steps in
`release-stable-manual.yml` and `cross-platform-build-manual.yml`
(`validate`, `web`, `release-notes`, `build`, `build-desktop`).
Everything else is skipped with a logged reason:

```
==> skip release-stable-manual:publish (not on dry-run-safe allowlist)
==> skip release-stable-manual:docker (not on dry-run-safe allowlist)
==> skip release-stable-manual:redeploy-website (not on dry-run-safe allowlist)
==> skip docs-deploy:deploy (not on dry-run-safe allowlist)
==> skip daily-audit:advisories (not on dry-run-safe allowlist)
==> skip tweet-release:tweet (not on dry-run-safe allowlist)
```

The allowlist is **fail-closed**: a new workflow added to the repo is
treated as potentially mutating until a maintainer reviews it and adds
the safe job IDs to `DRY_RUN_SAFE_JOBS` in
`scripts/dev/act-local.sh`. This matters because `discover_jobs` walks
every `.github/workflows/*.yml`, not just the release workflows, a
denylist would silently let a future write-surface workflow through.

Two escape hatches exist for the rare case where you have a reason to
attempt a non-allowlisted job locally:

- `./scripts/dev/act-local.sh release-stable-manual:publish`: the
  explicit `<wf>:<job>` form runs what you ask for and prints a loud
  warning before invoking `act` if the target isn't on the allowlist.
- `./scripts/dev/act-local.sh --all --no-allowlist`: disables the
  allowlist filter for an entire `--all` run (used only when you've
  already verified the workflow steps will not reach a mutation
  surface, e.g. on a fork with no real registry credentials and an
  empty `.secrets` file).

### What's expected to fail under `act` (and is fine)

`act` cannot simulate a few GitHub-only surfaces. These failures are
not real defects:

- Jobs that depend on a real release tag (`publish` creating a GitHub
  Release).
- Environment-gated jobs (`publish`, `docker`): the
  approval UI doesn't exist locally.
- OIDC-based federated identity tokens.

Everything else, a `tsc` error, a missing file, a Rust compile
failure, a `cargo` lockfile mismatch, is a real defect. Do not click
**Run workflow** on the GitHub Actions form until those are fixed via a
standard PR off master.

---

## Step 4: Trigger the release

Go to:

```
https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/release-stable-manual.yml
```

Click **Run workflow**. Fill in:

- **Branch:** `master`
- **Stable version to release:** `X.Y.Z`, no `v` prefix

Click **Run workflow**.

The first job (`validate`) checks that the version matches `Cargo.toml` and
that no tag `vX.Y.Z` already exists. If it fails, fix the mismatch and
re-trigger. Do not try to work around it.

---

## Step 5: Approve the environment gates

Two jobs are gated by GitHub environment protection rules. When each becomes
pending you will see a **"Waiting for review"** banner in the workflow run.

Approve both when they appear:

| Environment | Job | What it does |
|---|---|---|
| `github-releases` | `publish` | Creates the GitHub Release and uploads assets |
| `docker` | `docker` | Pushes images to GHCR |

If you miss the approval window and a job times out, re-run only the failed
job from the workflow run page; you do not need to restart from scratch.

---

## Step 6: Verify the release

Once `publish` completes, confirm:

```text
[ ] GitHub Release exists at /releases/tag/vX.Y.Z and is marked Latest
[ ] Release notes are non-empty
[ ] SHA256SUMS asset is present and non-empty
[ ] At least one binary archive is downloadable (spot-check linux x86_64)
```

`CHANGELOG-next.md` is intentionally left on `master` after the release: the
publish job only reads it as the release body, it does not delete it. The next
release cycle overwrites it, so no manual cleanup is required.

You do not need to manually verify Docker or the distribution channels
unless a job in the workflow run shows red. Check the workflow run summary; if
all jobs are green, you are done.

Consumers who want to verify signatures, SBOMs, or SLSA provenance on the
published artifacts can follow
[Release artifact verification](./release-verification.md).

---

## Step 7: Versioned documentation deployment

ZeroClaw docs use a versioned structure on the `gh-pages` branch. When a tag is pushed, the `Deploy mdBook docs to Pages` workflow automatically builds and deploys the documentation for that version. This runs automatically after the tag from step 4 lands; the bootstrap and version-floor details below are reference material for when you need to recreate `gh-pages` or change the supported-version window.

### What happens automatically

- Pushing a tag (e.g., `v0.8.0`) triggers a build that lands in `/v0.8.0/`.
- "Stable" is a pointer, not a copy. The release tag deploy (e.g., `v0.8.0`) is what builds and publishes that version's docs directory. `bump-version.sh` writes the released version to `docs/book/stable-version.txt`; landing that change on master refreshes the stable metadata only. The master deploy does not rebuild or republish the release tag's docs; it copies `stable-version.txt` to the `gh-pages` root and regenerates the root `/` redirect and the version-selector's "Stable (latest release)" entry so both resolve to that release's already-published version dir. The deploy fails loudly if the named version dir is not present on `gh-pages`. There is no duplicate `/stable/` tree.
- `gh-pages` is ephemeral: every deploy force-pushes a single orphan commit (no accumulating history) and enforces retention via `DOCS_KEEP_VERSIONS` (master plus the newest N final releases; pre-releases and older finals are pruned). This keeps clone size bounded.
- The `_shared/` directory (containing UI CSS, JS, and favicons) is updated from the build so the theme cascades to all deployed versions.
- Translated locales (`es`, `fr`, `ja`, `zh-CN`) render from the `docs/book/po` submodule, which the deploy resolves via `submodules: recursive` at whatever commit the deployed ref pins. That pin is set during the version bump; see [Step 2](#step-2-bump-and-merge-the-version-pr) for the refresh, tag, and pin procedure. English needs no submodule.

### Bootstrapping `gh-pages`

If `gh-pages` is ever deleted or needs to be fully recreated, seed the versions in this specific order:

1. **Oldest supported release:** `workflow_dispatch` with tag `v0.7.5`
2. **Next releases:** `workflow_dispatch` with tag `v0.8.0-beta-1` etc.
3. **Current master:** `workflow_dispatch` with tag `master`

> [!IMPORTANT]
> `master` must be deployed **last** during bootstrapping. It writes the definitive `_shared/` chrome layer that all other versions use.

> [!NOTE]
> Stable resolves from `docs/book/stable-version.txt` (committed in source, published to the gh-pages root as `stable-version.txt`). After bootstrapping, confirm that file names the intended GA release; the root redirect and the "Stable (latest release)" selector entry follow it. No `/stable/` directory is created.

### Manual redeploys and the Version Floor

To manually re-deploy a specific version:
1. Go to **Actions** → **Deploy mdBook docs to Pages**
2. Click **Run workflow**
3. Enter the tag (e.g., `v0.7.5` or `master`)

**The `DOCS_MIN_VERSION` floor:**
To prevent accidentally deploying very old or unsupported versions, the workflow enforces a minimum version floor (currently `v0.7.5`).

- Tags older than `DOCS_MIN_VERSION` (like `v0.7.4`) are rejected by the workflow.
- `cargo mdbook gen-versions` (the xtask helper) ignores any directories on `gh-pages` below this floor, keeping them out of the version dropdown.

If you need to raise the floor to drop support for an older version:
1. Update the `DOCS_MIN_VERSION` environment variable in `.github/workflows/docs-deploy.yml`.
2. Old version directories are pruned automatically on the next deploy by the `DOCS_KEEP_VERSIONS` retention pass; no manual `gh-pages` edit is required to reclaim space.

---

## If something goes wrong

**validate failed: version mismatch:** The version bump PR was not merged, or
you typed the wrong version. Fix the mismatch and re-trigger.

**An environment gate timed out:** Re-run only the timed-out job. No need to
restart the workflow.

**A distribution channel job failed (Scoop, AUR, Homebrew):** Each has a
corresponding manually-triggerable sub-workflow. Re-run the specific one with
`dry_run: true` first to confirm the fix, then `dry_run: false`. These are
nice-to-have: a failed Scoop job does not invalidate the release itself.

---

## Removed legacy workflows

Several auto-publishing workflows that previously lived in `.github/workflows/`
have been deleted because they bypassed review or published irreversibly. They
are no longer present; if any reappears in a PR, treat it as a regression and
block it:

| Workflow | Why it was removed |
|---|---|
| `release-beta-on-push.yml` | Published automatically on every push to master |
| `publish-crates-auto.yml` | Auto-published to crates.io on any version change, irreversible |
| `version-sync.yml` | Committed directly to master as a bot, bypassing review |
| `checks-on-pr.yml` | Duplicate CI: produced confusing conflicting status |
| `pre-release-validate.yml` | Unused generated checklist; this runbook replaces it |

The full inventory of the workflows that remain (automatic and manual) lives
in [CI & Actions](./ci-and-actions.md).

---

## Where this is going

This runbook and `release-stable-manual.yml` are a bridge, not a destination.

The target end state:

- release-plz manages version bumps and changelogs automatically
- A single `release.yml` replaces the current patchwork of sub-workflows
- SLSA provenance is built into the pipeline
- The team cuts releases by merging a release PR, not by following a runbook

Until that lands, use this process. Every release you cut manually using this
runbook is practice that informs what the automation needs to do.

