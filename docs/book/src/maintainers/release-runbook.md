# Release Runbook

> **Interim manual process.** This runbook covers how to ship a stable release
> today using `release-stable-manual.yml`. It exists only until release-plz
> lands in v0.7.5 and replaces this entirely.
>
> If anything in here feels heavyweight, that is intentional friction — we do
> not yet have the automation discipline to remove it safely.

Last verified: **May 2026** (v0.7.4 cycle).

---

## The process in six steps

1. Generate `CHANGELOG-next.md` using the changelog skill
2. Open and merge a version bump PR
3. Dry-run the release workflows locally with `act`
4. Trigger the `Release Stable` workflow via manual dispatch
5. Approve the three environment gates when prompted
6. Verify the release exists and assets are downloadable

That is the entire process. Everything else (Docker, crates.io, Scoop, AUR,
Homebrew, Discord, tweet) runs automatically as downstream jobs. You do not
need to do anything for those unless a job explicitly fails.

---

## Step 1 — Generate CHANGELOG-next.md

Use the changelog-generation skill to produce `CHANGELOG-next.md`:

```text
.claude/skills/changelog-generation/SKILL.md
```

The skill generates the changelog from the git log between the last stable tag
and HEAD, resolves contributors via GitHub GraphQL, and writes the file. Commit
the result directly to a short-lived branch and include it in the version bump
PR (step 2), or open it as a separate preceding PR if the diff is large.

If `CHANGELOG-next.md` already exists from a previous aborted release cycle,
review it for accuracy before reusing it.

---

## Step 2 — Bump and merge the version PR

Edit the workspace `Cargo.toml`:

```toml
[workspace.package]
version = "X.Y.Z"
```

Sync all other version references:

```bash
bash scripts/release/bump-version.sh X.Y.Z
```

This updates README badges, the Tauri config, marketplace templates, and
workflow description examples. Commit everything together:

```text
chore: bump version to vX.Y.Z
```

Open a PR. Label it `chore`, `size: XS`. Get one maintainer review. Merge when
CI is green.

**Confirm the merge landed correctly:**

```bash
git fetch origin
git show origin/master:Cargo.toml | grep '^version'
# Must show: version = "X.Y.Z"
```

---

## Step 3 — Dry-run the release workflows locally with `act`

The `Release Stable` workflow is a GitHub Actions job graph that consumes
your environment-gate approval window the moment you click **Run workflow**.
If a workflow step is broken — a missing build artifact, a stale path, a
codegen step that someone removed without updating CI — the failure surfaces
*after* you have committed to a release window, with the version PR already
merged and master at the new version. Recovery means landing an emergency
fix branch, re-running CI, and shipping under time pressure on a tree that
already advertises itself as a fully-released version.

The cheap insurance against this is to run the same job graph locally first,
on the exact merged master commit, before opening the GitHub Actions form.
[`act`](https://nektosact.com/) executes GitHub Actions workflows inside
Docker containers using the same `actions/*` ecosystem GitHub does. It does
not perfectly mirror the cloud runner — it cannot reach the artifact upload
runtime, GitHub-issued OIDC tokens, environment secrets, or jobs that depend
on a real release tag — but it does run the build and test steps that
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

   ```bash
   gh extension install nektos/gh-act
   ```

3. Install Docker Engine or Docker Desktop from
   <https://docs.docker.com/engine/install/>. On Linux, add yourself to
   the `docker` group so you don't need `sudo`. `act` also works with
   Podman and Colima — see the
   [act runners documentation](https://nektosact.com/usage/runners.html).

That's the whole setup. The repository's `.actrc` and
`scripts/dev/act-local.sh` handle everything else (runner image, secrets
file, artifact server, action SHA pre-fetching).

### Per-release dry-run

Make sure your working tree matches the merged master tip from step 2:

```bash
git fetch upstream
git checkout upstream/master
```

List what's runnable across every workflow file:

```bash
./scripts/dev/act-local.sh --list
```

Run a specific job, pick interactively, or run every dry-run-safe
job:

```bash
./scripts/dev/act-local.sh release-stable-manual:web   # one job
./scripts/dev/act-local.sh                              # interactive picker
./scripts/dev/act-local.sh --all                        # every dry-run-safe job
```

The first run pulls the runner image (~1.5 GB) and primes the Rust build
cache via `Swatinem/rust-cache`; subsequent runs are much faster. The
script auto-creates the gitignored `.secrets` file, pre-fetches every
pinned action SHA into `~/.cache/act/` (act's shallow clone can't
resolve arbitrary commits otherwise), threads `GITHUB_TOKEN` from your
`gh` auth into the run via the parent process environment (the token
value never lands in argv), and sets `--artifact-server-path` so
`actions/upload-artifact` and `actions/download-artifact` work between
jobs. All of that is plain `act` underneath — the script just removes
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
to run locally — currently the artifact-only build steps in
`release-stable-manual.yml` and `cross-platform-build-manual.yml`
(`validate`, `web`, `release-notes`, `build`, `build-desktop`).
Everything else is skipped with a logged reason:

```
==> skip release-stable-manual:publish (not on dry-run-safe allowlist)
==> skip release-stable-manual:docker (not on dry-run-safe allowlist)
==> skip release-stable-manual:redeploy-website (not on dry-run-safe allowlist)
==> skip docs-deploy:deploy (not on dry-run-safe allowlist)
==> skip daily-audit:audit (not on dry-run-safe allowlist)
==> skip tweet-release:tweet (not on dry-run-safe allowlist)
```

The allowlist is **fail-closed**: a new workflow added to the repo is
treated as potentially mutating until a maintainer reviews it and adds
the safe job IDs to `DRY_RUN_SAFE_JOBS` in
`scripts/dev/act-local.sh`. This matters because `discover_jobs` walks
every `.github/workflows/*.yml`, not just the release workflows — a
denylist would silently let a future write-surface workflow through.

Two escape hatches exist for the rare case where you have a reason to
attempt a non-allowlisted job locally:

- `./scripts/dev/act-local.sh release-stable-manual:publish` — the
  explicit `<wf>:<job>` form runs what you ask for and prints a loud
  warning before invoking `act` if the target isn't on the allowlist.
- `./scripts/dev/act-local.sh --all --no-allowlist` — disables the
  allowlist filter for an entire `--all` run (used only when you've
  already verified the workflow steps will not reach a mutation
  surface, e.g. on a fork with no real registry credentials and an
  empty `.secrets` file).

### What's expected to fail under `act` (and is fine)

`act` cannot simulate a few GitHub-only surfaces. These failures are
not real defects:

- Jobs that depend on a real release tag (`publish` creating a GitHub
  Release).
- Environment-gated jobs (`crates-io`, `docker`, `publish`) — the
  approval UI doesn't exist locally.
- OIDC-based federated identity tokens.

Everything else — a `tsc` error, a missing file, a Rust compile
failure, a `cargo` lockfile mismatch — is a real defect. Do not click
**Run workflow** on the GitHub Actions form until those are fixed via a
standard PR off master.

---

## Step 4 — Trigger the release

Go to:

```
https://github.com/zeroclaw-labs/zeroclaw/actions/workflows/release-stable-manual.yml
```

Click **Run workflow**. Fill in:

- **Branch:** `master`
- **Stable version to release:** `X.Y.Z` — no `v` prefix

Click **Run workflow**.

The first job (`validate`) checks that the version matches `Cargo.toml` and
that no tag `vX.Y.Z` already exists. If it fails, fix the mismatch and
re-trigger. Do not try to work around it.

---

## Step 5 — Approve the environment gates

Three jobs are gated by GitHub environment protection rules. When each becomes
pending you will see a **"Waiting for review"** banner in the workflow run.

Approve all three when they appear:

| Environment | Job | What it does |
|---|---|---|
| `github-releases` | `publish` | Creates the GitHub Release and uploads assets |
| `crates-io` | `crates-io` | Publishes crates to crates.io |
| `docker` | `docker` | Pushes images to GHCR |

If you miss the approval window and a job times out, re-run only the failed
job from the workflow run page — you do not need to restart from scratch.

---

## Step 6 — Verify the release

Once `publish` completes, confirm:

```text
[ ] GitHub Release exists at /releases/tag/vX.Y.Z and is marked Latest
[ ] Release notes are non-empty
[ ] SHA256SUMS asset is present and non-empty
[ ] At least one binary archive is downloadable (spot-check linux x86_64)
[ ] CHANGELOG-next.md is gone from master (the publish job removes it automatically)
```

You do not need to manually verify Docker, crates.io, or distribution channels
unless a job in the workflow run shows red. Check the workflow run summary — if
all jobs are green, you are done.

---

## If something goes wrong

**validate failed — version mismatch:** The version bump PR was not merged, or
you typed the wrong version. Fix the mismatch and re-trigger.

**An environment gate timed out:** Re-run only the timed-out job. No need to
restart the workflow.

**publish succeeded but CHANGELOG-next.md is still on master:** Remove it
manually:

```bash
git checkout master && git pull --ff-only origin master
git rm CHANGELOG-next.md
git commit -m "chore: remove CHANGELOG-next.md after vX.Y.Z release"
git push origin master
```

**A distribution channel job failed (Scoop, AUR, Homebrew):** Each has a
corresponding manually-triggerable sub-workflow. Re-run the specific one with
`dry_run: true` first to confirm the fix, then `dry_run: false`. These are
nice-to-have — a failed Scoop job does not invalidate the release itself.

---

## Workflows you must not touch

The following workflows exist in `.github/workflows/` but are dangerous and
scheduled for deletion in v0.7.4 (#5915). Do not trigger them. Do not extend
them.

| Workflow | Why it is dangerous |
|---|---|
| `release-beta-on-push.yml` | Publishes automatically on every push to master |
| `publish-crates-auto.yml` | Auto-publishes to crates.io on any version change — irreversible |
| `version-sync.yml` | Commits directly to master as a bot, bypasses review |
| `checks-on-pr.yml` | Duplicate CI — produces confusing conflicting status |
| `pre-release-validate.yml` | Unused generated checklist — this runbook replaces it |

All other workflows not listed above are either frozen until v0.7.5 or
actively maintained. See `docs/contributing/ci-map.md` for the full inventory
once it is rewritten in #5917.

---

## Where this is going

This runbook and `release-stable-manual.yml` are a bridge, not a destination.

In v0.7.5 the goal is:

- release-plz manages version bumps and changelogs automatically
- A single `release.yml` replaces the current patchwork of sub-workflows
- SLSA provenance is built into the pipeline
- The team cuts releases by merging a release PR, not by following a runbook

Until that lands, use this process. Every release you cut manually using this
runbook is practice that informs what the automation needs to do.

