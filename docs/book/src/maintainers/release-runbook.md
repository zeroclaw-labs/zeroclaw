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

Install `act` via any of the methods documented at
[nektosact.com/installation](https://nektosact.com/installation/index.html).
Common paths:

```bash
# Bash script (Linux / macOS)
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/nektos/act/master/install.sh | sudo bash

# macOS (Homebrew)
brew install act

# Windows (winget or Chocolatey)
winget install nektos.act
choco install act-cli

# As a gh extension
gh extension install https://github.com/nektos/gh-act
```

Or download a static binary directly from
[github.com/nektos/act/releases](https://github.com/nektos/act/releases) and
drop it on your `PATH`.

`act` requires a Docker-compatible container runtime. Install Docker Engine
or Docker Desktop using the official instructions at
[docs.docker.com/engine/install](https://docs.docker.com/engine/install/),
then ensure your user can reach the daemon without `sudo` (typically by
adding yourself to the `docker` group and re-logging in). `act` also
supports Podman and Colima as alternatives — see the act runners
documentation for configuration.

The repository ships a pre-configured `.actrc` at the root that pins the
Linux runner image to `catthehacker/ubuntu:act-latest`, forces amd64
architecture, and references a `.secrets` file. Create the secrets file
once (it is gitignored, never committed):

```bash
touch .secrets
```

Add provider-specific secrets only if you are testing a workflow that
actually uses them.

### Per-release dry-run

Make sure your working tree matches the merged master tip from step 2:

```bash
git fetch upstream
git checkout upstream/master
```

List the workflows so you can pick the right job:

```bash
act -W .github/workflows/release-stable-manual.yml -l
```

Run the build-shaped jobs that exercise codegen and compilation. The first
run pulls the runner image (~1.5 GB) and primes the Rust cache; subsequent
runs are much faster.

```bash
# Builds the web dashboard end-to-end (gen-api → tsc → vite).
act -j web -W .github/workflows/release-stable-manual.yml

# Cross-platform binary builds (slowest job; consider running only one
# matrix entry locally if you want to validate compile shape without
# burning an hour on every target).
act -j build -W .github/workflows/release-stable-manual.yml
```

What you are looking for: the build steps complete successfully. Two
classes of failure are *expected* under `act` and do not indicate a real
problem:

- `actions/upload-artifact@v4` failing with
  `Unable to get the ACTIONS_RUNTIME_TOKEN env variable`. `act` does not
  implement GitHub's runtime artifact API; uploads only work on real CI.
- Jobs that depend on a release tag, a GitHub-issued OIDC token, or an
  environment secret you have not put in `.secrets`. Skip these with
  `act -j <other-job>` or accept that they will not run locally.

Anything else — a `tsc` error, a missing file, a Rust compile failure, a
`cargo` lockfile mismatch — is a real defect. Do not click **Run workflow**
on the GitHub Actions form until those are fixed and merged. If a fix is
required, it goes through the standard PR flow on a branch off master, just
like any other CI fix.

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

