# FND-004: Engineering Infrastructure — CI/CD Pipeline and Release Automation
### Supporting v0.7.0 → v1.0.0 · Type: Architecture · Rev. 1

> **Canonical reference** · Ratified by the team · Rev. 1
> Discussion thread and full revision history: [#5579](https://github.com/zeroclaw-labs/zeroclaw/issues/5579)


---

> **A note to the team before you read this.**
>
> This document is about the scaffolding around the code — the automation that builds it, tests it, audits it, and ships it. That scaffolding is invisible when it works well and painful when it does not. Most teams do not think about it until it is painful, and by then it has grown into something nobody fully understands. This RFC is an attempt to get ahead of that. If you have never thought deeply about CI/CD before, this is a good place to start. If you have, you will recognise the patterns. Either way, the goal is the same: a pipeline that gives the team confidence without getting in the way.

---

## Table of Contents

1. [Context: Pipelines Are Architecture](#1-context-pipelines-are-architecture)
2. [Honest Assessment: Where We Are Today](#2-honest-assessment-where-we-are-today)
3. [The Target Pipeline Design](#3-the-target-pipeline-design)
4. [Security Scanning as a Lifecycle](#4-security-scanning-as-a-lifecycle)
5. [Release Automation Aligned to the Distribution Model](#5-release-automation-aligned-to-the-distribution-model)
6. [Standards We Should Adopt](#6-standards-we-should-adopt)
7. [Phased Roadmap](#7-phased-roadmap)
8. [What This Means for Contributors](#8-what-this-means-for-contributors)

---

## Revision History

| Rev | Date | Summary |
|---|---|---|
| 1 | 2026-04-09 | Initial draft |

---

## 1. Context: Pipelines Are Architecture

The architecture RFC (#5574) established a principle: *dependencies flow inward, and structure is enforced by the compiler.* The same principle applies to the pipeline that surrounds the code. A pipeline is not just automation — it is a set of architectural decisions about what you trust, what you verify, when you verify it, and how you ship.

Those decisions have consequences. A pipeline that was designed for a monolith will actively resist a microkernel. A security gate that has no triage process will either block everything or get bypassed. A release workflow built around one binary will not survive a distribution model with five artifact types. These are not configuration problems. They are design problems, and they deserve the same intentional treatment as the code architecture.

The current pipeline grew reactively, the same way `loop_.rs` grew to 9,500 lines. Nobody chose the current state. It accumulated. PR #5559 — the first major step of the microkernel transition — exposed several places where the pipeline's assumptions no longer hold. That is a useful signal. It means now is exactly the right moment to stop, assess, and design intentionally.

This RFC does for the pipeline what the architecture RFC does for the codebase: names what exists, identifies the structural problems, and proposes a path forward that is consistent with where the project is going.

---

## 2. Honest Assessment: Where We Are Today

This section is not criticism. It is a diagnosis. The current pipeline reflects the decisions that made sense at the time. The goal is to understand it clearly enough to improve it.

### 2.1 Two Workflows Doing the Same Work

The repository currently has two separate workflows that run on pull requests against `master`:

- `checks-on-pr.yml` — branded as "Quality Gate"
- `ci-run.yml` — branded as "CI"

Both run Lint, Build, Test, and Security jobs independently on every PR. This means every PR triggers two full pipeline runs in parallel. For a monolith with a single compilation unit, this was expensive but manageable. For a multi-crate workspace, it doubles an already significant CI budget with no additional signal.

The duplication has a subtler cost beyond compute minutes: when a check fails in one workflow but not the other, contributors do not know which result to trust. When a new check needs to be added, it must be added in two places. When behaviour needs to change, it must change in two places. Two sources of truth is the same problem as two sources of truth in code.

### 2.2 Single-Binary Assumptions Are Baked In Everywhere

The release automation — `release-stable-manual.yml`, `release-beta-on-push.yml`, `publish-crates.yml`, `pub-aur.yml`, `pub-homebrew-core.yml`, `pub-scoop.yml`, `discord-release.yml`, `tweet-release.yml` — was designed around the assumption that a release is one binary. You build it, sign it, push it to package managers, and announce it.

The architecture RFC defines a distribution model with five distinct artifact types: the kernel binary (multiple platform targets), the hardware-variant kernel binary, the gateway binary, WASM plugin files, and the Tauri desktop installer. None of the current release workflows account for this structure. When the architecture transition reaches Phase 3 and Phase 4, every one of these workflows will need to change — unless they are redesigned now with that model in mind.

### 2.3 Security Scanning Without a Lifecycle

The security job runs `cargo audit` as a hard gate. If any advisory is present in the dependency tree, the gate fails and the PR cannot merge. The intent is correct. The implementation has a structural problem.

`cargo audit` reports all advisories in the dependency tree: active vulnerabilities, unmaintained crates, and informational notices. It does not distinguish between:

- A critical vulnerability in a crate the project actively calls
- A vulnerability in a transitive dependency three levels deep in an optional feature
- An "unmaintained" notice for a crate the project depends on indirectly through a third-party library it cannot control
- A pre-existing advisory that was present before this PR was opened

When all of these produce the same hard failure, the gate becomes noise. The realistic response to noise is to lower the gate, ignore the failures, or suppress the checks. All three of those responses make the project less secure, not more. A security gate that cannot be maintained will not be maintained.

PR #5559 surfaced twelve RUSTSEC-2026 advisories simultaneously. Without tooling to distinguish "new advisory introduced by this PR" from "pre-existing advisory present on master," the PR author and reviewers cannot know whether this PR made the security posture worse.

### 2.4 The Strict Delta Lint Script

`ci-run.yml` includes a job that runs `scripts/ci/rust_strict_delta_gate.sh` — a custom script that compares clippy output against the base SHA of the PR. The concept is sound: you want to know whether this PR introduced new warnings, not just whether warnings exist in the codebase. The implementation works well for small, focused PRs against a monolithic crate.

A PR that moves 260,000 lines of code across 10 new crates, touching hundreds of files, puts this script in territory it was not designed for. The changed-file surface is too large for an incremental comparison to produce a meaningful signal. The script needs to understand workspace structure — specifically that a change to a file in `crates/zeroclaw-channels/` should be evaluated in the context of that crate, not the root.

### 2.5 No Workspace-Aware Caching or Scoping

The current Rust cache configuration (`Swatinem/rust-cache`) is adequate for a single crate. For a multi-crate workspace, cache effectiveness depends on understanding which crates changed and which compiled artifacts can be reused. Without explicit workspace scoping, a change to any crate can invalidate caches that other crates depend on, producing full recompilation on every PR.

More significantly, there is no mechanism for running CI only against the crates affected by a given change. A PR that fixes a typo in `zeroclaw-tool-call-parser` does not need to rebuild and retest the gateway. As the workspace grows toward the 30+ crate model the architecture RFC envisions, the cost of running the full pipeline on every PR becomes a meaningful obstacle to contribution.

### 2.6 Action Pinning Is Good — But Undocumented

The existing workflows do pin actions to full commit SHAs, which is correct security practice and worth acknowledging. But there is no documented policy explaining why, no process for reviewing when those SHAs should be updated, and no automation for keeping them current. Good behaviour without a policy is fragile — the next contributor to add a workflow step may not know why SHA pinning matters and will use a mutable tag instead.

---

## 3. The Target Pipeline Design

### 3.1 One Pipeline, One Source of Truth

The two parallel workflows should be consolidated into a single, well-structured pipeline. The distinction between "Quality Gate" and "CI" is not meaningful to contributors — both are checks a PR must pass. The consolidation creates one place to find check results, one place to update when behaviour changes, and one place to document what each check is doing and why.

The consolidated pipeline follows a staged structure where fast, cheap checks run first and gate slower, more expensive ones:

```
Stage 1: Format + Lint (2–5 min)
  └── cargo fmt --check
  └── cargo clippy --workspace --all-targets -- -D warnings
  └── Docs quality gate

Stage 2: Build + Check (parallel, 5–15 min)
  └── Build matrix (Linux x86_64, macOS ARM, Windows)
  └── cargo check --features ci-all
  └── cargo check --no-default-features (kernel profile)
  └── cargo check --target i686 (32-bit)

Stage 3: Test (10–30 min, gated on Stage 1)
  └── cargo nextest run --workspace

Stage 4: Security (parallel with Stage 3)
  └── cargo deny check (licenses, sources, advisories)
  └── Advisory triage gate (see §4)

Stage 5: Required Gate
  └── Composite status — branch protection requires only this job
```

Stages 2, 3, and 4 run in parallel after Stage 1 passes. This means a formatting error fails fast without burning compute on a build that will be thrown away. The Required Gate job aggregates all results so branch protection needs to track only one job name — a pattern already present in both current workflows.

### 3.2 Workspace-Aware Clippy

The current clippy invocation runs against the default feature set of the root crate. The correct invocation for a multi-crate workspace is:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

The `--workspace` flag ensures every crate in the workspace is linted, not just the root. The `--all-targets` flag includes tests, benchmarks, and examples. Combined with `--features ci-all` for the feature-gated check, this gives a complete picture.

The strict delta lint concept — checking whether this PR introduced new warnings rather than whether warnings exist at all — is worth preserving. The implementation should move from a shell script comparing diff output to a proper workspace-aware invocation that evaluates each affected crate independently. A simpler and more reliable approach: require `--workspace -D warnings` to pass clean at all times, making the delta concept implicit. If the baseline is always clean, any PR that introduces a warning fails. This removes the need for a custom comparison script entirely.

### 3.3 Changed-Crate Detection

For a workspace growing toward 30+ crates, running the full test suite on every PR regardless of what changed is wasteful. The pipeline should detect which crates were affected by the PR and scope test execution accordingly.

The mechanism is straightforward: compare the files changed in the PR against the workspace member list, identify which crates contain changed files, expand the set to include all crates that depend on any changed crate (downstream impact), and run tests only for that set.

```
PR changes: crates/zeroclaw-tool-call-parser/src/lib.rs

Affected crates:
  zeroclaw-tool-call-parser     ← directly changed
  zeroclaw-misc                 ← depends on it
  zeroclawlabs (root)           ← depends on it

Not affected:
  zeroclaw-channels             ← no dependency path
  zeroclaw-memory               ← no dependency path
  zeroclaw-providers            ← no dependency path
```

This is implemented using `cargo metadata` to extract the dependency graph and a short script to walk it. The full test suite continues to run on pushes to `master` and on release branches. PRs run the affected-crate subset.

### 3.4 Caching Strategy

`Swatinem/rust-cache` supports workspace-aware caching through its `workspaces` configuration. The cache key should incorporate the workspace member list so that adding a new crate invalidates appropriately without invalidating unrelated crate caches.

```yaml
- uses: Swatinem/rust-cache@<sha>
  with:
    workspaces: |
      . -> target
    cache-on-failure: true
    save-if: ${{ github.ref == 'refs/heads/master' }}
```

The `save-if` condition means cache is only written on `master` pushes, not on every PR. PRs read from the cache but do not write competing versions. This avoids cache thrashing when multiple PRs are open simultaneously.

---

## 4. Security Scanning as a Lifecycle

### 4.1 The Problem With a Binary Gate

A security gate that blocks on any advisory, without context, trains the team to treat security failures as noise. That is the opposite of the intended effect. The goal is a gate that is:

- **High signal** — failures mean something real that this PR affected
- **Actionable** — the contributor knows what to do and why
- **Sustainable** — the gate can be maintained without constant manual intervention

`cargo audit` alone does not achieve this. `cargo deny` does.

### 4.2 cargo-deny as the Primary Security Tool

`cargo deny` is a more capable successor to `cargo audit` for project-level dependency policy. It enforces:

- **Advisories** — RUSTSEC database, with the ability to deny, warn, or explicitly ignore specific advisories with a documented justification
- **Licenses** — ensures all dependencies use acceptable licenses (important as the workspace grows and new contributors add deps)
- **Sources** — ensures dependencies come only from approved registries (crates.io, path, git with specific hosts)
- **Duplicates** — warns when multiple versions of the same crate appear in the dependency tree

The key capability is the `[advisories]` section of `deny.toml`, which allows explicit ignores:

```toml
[advisories]
db-urls = ["https://github.com/rustsec/advisory-db"]
vulnerability = "deny"
unmaintained = "warn"
unsound = "deny"
notice = "warn"

ignore = [
    # RUSTSEC-2024-0388: `derivative` unmaintained — pulled in transitively
    # through matrix-sdk; no direct usage, no active exploit. Tracked in #XXXX.
    { id = "RUSTSEC-2024-0388", reason = "Transitive dep via matrix-sdk; no direct usage" },
]
```

This approach transforms security scanning from a binary pass/fail into a documented, auditable policy. Every ignored advisory has a written justification and a tracking issue. Reviewers can see exactly which advisories are being suppressed and why. When a suppressed advisory escalates (a new exploit is found, a fix is available), the tracking issue is the reminder.

### 4.3 Advisory Triage Process

When a new advisory appears in the dependency tree — whether from a PR or from the daily advisory database update — the process is:

1. **Classify the advisory**: Is the affected crate a direct dependency or transitive? Does ZeroClaw call the vulnerable code path? Is there a fixed version available?
2. **Determine the response**:
   - *Vulnerability in a direct dep with a fix available* → update the dep, no ignore needed
   - *Vulnerability in a transitive dep with a fix available* → pin the transitive version or wait for the direct dep to update; open a tracking issue
   - *Unmaintained notice, no active exploit* → add to `deny.toml` ignore list with justification and tracking issue
   - *Critical vulnerability with no fix* → assess workaround; may block the PR
3. **Record the decision** in `deny.toml` with the advisory ID, a brief rationale, and a link to the tracking issue

This process means a PR like #5559 that surfaces twelve pre-existing advisories does not fail the gate without context. The advisories are triaged, the pre-existing ones are documented, and the gate reports only on new un-triaged advisories introduced by the PR.

### 4.4 Daily Advisory Scan

Security advisories are published continuously. A PR that passed the security gate when it merged may contain a vulnerability published the following week. The pipeline should include a scheduled daily run against `master` that checks the advisory database and opens a GitHub Issue if new un-triaged advisories are found.

```yaml
on:
  schedule:
    - cron: '0 9 * * *'  # 09:00 UTC daily
```

This separates the advisory triage cycle from the PR merge cycle. Contributors are not blocked by advisories that appeared after their PR was written. The security team (or whoever is on rotation) handles the daily scan output as a regular maintenance task.

---

## 5. Release Automation Aligned to the Distribution Model

### 5.1 The Current Mismatch

The architecture RFC §4.4.2 defines the following release artifacts:

| Artifact | Build target | Published to |
|---|---|---|
| Kernel binary (standard) | x86_64-linux-musl, aarch64-linux-gnu, armv7-linux-gnueabihf, x86_64-darwin, aarch64-darwin, x86_64-windows | GitHub Releases |
| Kernel binary (hardware) | aarch64-linux-gnu, armv7-linux-gnueabihf | GitHub Releases |
| Gateway binary | Same platform matrix | GitHub Releases |
| WASM plugin files | wasm32-wasip1 | Plugin registry |
| Desktop installer | x86_64 + aarch64, macOS/Windows/Linux | GitHub Releases, platform stores |

The current release workflows know about exactly one of these: the standard binary. The rest do not exist in the automation yet. This is appropriate for now — the plugin system is not yet complete. But the release workflows should be designed with this model in mind so they do not need to be rewritten as each new artifact type is introduced.

### 5.2 A Release Pipeline Structure

The target release pipeline is a directed graph of jobs, not a monolithic workflow:

```
version-bump (release-plz PR merged)
    │
    ├── build-kernel-standard (matrix: 6 targets)
    ├── build-kernel-hardware (matrix: 2 ARM targets + hardware flags)
    ├── build-gateway (matrix: 6 targets)
    ├── build-plugins-wasm (matrix: all plugin crates → wasm32-wasip1)
    └── build-desktop (matrix: macOS, Windows, Linux AppImage/deb)
            │
            ├── publish-github-release (attaches all kernel + gateway binaries)
            ├── publish-plugin-registry (uploads WASM files)
            ├── publish-aur (kernel binary for Arch Linux)
            ├── publish-homebrew (kernel binary for macOS)
            ├── publish-scoop (kernel binary for Windows)
            └── announce (Discord, social)
```

Each build job is independent and can be triggered separately for hotfix releases. The publish jobs depend on all relevant build jobs succeeding. The announce job runs last.

This structure means a plugin-only release (a new version of `channel-discord.wasm`) can run only the `build-plugins-wasm` and `publish-plugin-registry` jobs without triggering a full kernel rebuild. A kernel patch release runs `build-kernel-*` and the downstream publish jobs without touching the plugin registry.

### 5.3 Release-plz for Workspace-Aware Version Management

The architecture RFC §4.4.1 specifies `release-plz` as the release automation tool. `release-plz` integrates directly with this pipeline model:

- On push to `master`, `release-plz` opens a "Release PR" that bumps the workspace version, updates changelogs from conventional commit history, and lists all crates that have changed since the last release
- When the Release PR is merged, the release pipeline triggers automatically
- Crates with `version.workspace = true` are bumped together; independently-versioned crates (`zeroclaw-api`, hardware library crates) are handled separately per the versioning policy

The Release PR serves as a review checkpoint: the team sees exactly what version will be published and what the changelog says before anything goes out. This replaces manual version bumps and the `version-sync.yml` workflow.

### 5.4 Action Pinning Policy

The current workflows already pin actions to full commit SHAs. This is correct and should be formalised as an explicit policy so it survives contributor turnover:

**Policy:** All `uses:` references in workflow files must be pinned to a full commit SHA with a version comment. Mutable tags (`@v4`, `@main`, `@latest`) are not permitted. No exceptions.

```yaml
# Correct
- uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683 # v4.2.2

# Not permitted
- uses: actions/checkout@v4
- uses: actions/checkout@main
```

**Rationale:** A mutable tag is a promise from a third party that the action's behaviour will not change. That promise has been broken repeatedly across the GitHub Actions ecosystem. A SHA pin means the workflow runs exactly what was reviewed, regardless of what the action author does after the fact. This is especially important for actions that have write permissions or access to secrets.

The update process: use `dependabot` or `renovate` configured for GitHub Actions to open PRs when new SHA versions are available. The team reviews and merges those PRs. This keeps actions current without requiring manual monitoring.

---

## 6. Standards We Should Adopt

### 6.1 SLSA: Supply Chain Security Framework

SLSA (Supply-chain Levels for Software Artifacts, pronounced "salsa") is a framework developed by Google and adopted across the industry for securing the software supply chain. It defines four levels of build integrity, from basic to hermetic.

For ZeroClaw's current scale and team size, **SLSA Level 2** is the appropriate target:

- Builds run on a hosted CI platform (already true — GitHub Actions)
- Build scripts are version-controlled (already true)
- Build provenance is generated and attached to release artifacts (the step to add)

SLSA Level 2 provenance means each release artifact ships with a cryptographically signed attestation that records: what source commit produced it, which workflow produced it, and that the workflow ran on the expected platform. Users and package managers can verify this attestation. It closes the gap between "we say this binary came from this source" and "this binary provably came from this source."

GitHub Actions supports SLSA Level 2 provenance generation natively through the `actions/attest-build-provenance` action. The cost to add it is one step per build job.

### 6.2 Conventional Commits (Already Implied — Formalise It)

The architecture RFC's versioning policy and release-plz integration both depend on conventional commit format for changelog generation. The governance RFC already references PR title conventions. This RFC formalises the connection: conventional commit format in commit messages and PR titles is a requirement, not a suggestion, because it is the input that drives automated changelog generation.

The categories that matter for ZeroClaw's changelog:

| Prefix | Changelog section | Version impact |
|---|---|---|
| `feat:` | New features | MINOR |
| `fix:` | Bug fixes | PATCH |
| `feat!:` or `fix!:` | Breaking changes | MAJOR |
| `chore:` | Maintenance | No release entry |
| `docs:` | Documentation | No release entry |
| `perf:` | Performance | PATCH |
| `security:` | Security fixes | PATCH (at minimum) |

CI enforces this with a PR title lint job that validates the title matches the conventional commit format before any other check runs.

### 6.3 Reusable Workflows

As the number of crates and artifact types grows, workflow duplication becomes a maintenance problem. GitHub Actions supports reusable workflows — a workflow that can be called from another workflow like a function. The build matrix, the security scan, and the test runner should each be extracted as reusable workflows.

```
.github/
  workflows/
    ci.yml               ← PR checks (calls reusable workflows)
    release.yml          ← Release pipeline (calls reusable workflows)
    daily-audit.yml      ← Scheduled security scan
  _workflows/            ← Reusable workflow definitions
    build-rust.yml       ← Parameterised build job
    test-workspace.yml   ← Parameterised test job
    security-scan.yml    ← cargo-deny invocation + triage
    publish-release.yml  ← Parameterised publish job
```

A reusable workflow is called with parameters:

```yaml
jobs:
  build-kernel:
    uses: ./.github/_workflows/build-rust.yml
    with:
      target: x86_64-unknown-linux-musl
      features: ""
      profile: dist
```

This means the CI workflow and the release workflow share the same build definition. A fix to the build process applies everywhere at once.

---

## 7. Phased Roadmap

The pipeline migration follows the same Strangler Fig approach as the code migration: build alongside, migrate steadily, never break the existing gate.

---

### Phase 1 · v0.7.0 — "Rationalise"

**Theme:** One pipeline, clean signal, no duplication.

**Why this phase:** The architectural transition is already underway. The pipeline needs to stop fighting it before it makes implementation work harder than it needs to be.

#### Deliverables

**D1: Consolidate `checks-on-pr.yml` and `ci-run.yml` into a single workflow**

Merge the two PR workflows into one. The consolidated workflow keeps the staged structure defined in §3.1. The `Quality Gate` and `CI` naming distinction disappears. There is one workflow, one set of results, one place to look.

The composite gate job (`CI Required Gate`) is preserved. Branch protection continues to require only that single job. This means the internal structure of the pipeline can change without requiring branch protection rule updates.

**D2: Replace `cargo audit` with `cargo deny`**

Add `deny.toml` to the repository root. Configure the `[advisories]`, `[licenses]`, and `[sources]` sections. Triage all current RUSTSEC advisories on `master`: update what can be updated, document what cannot with justification and tracking issues. The security gate passes clean on `master` before this phase is complete.

**D3: Fix workspace-aware clippy invocation**

Change `cargo clippy --all-targets -- -D warnings` to `cargo clippy --workspace --all-targets -- -D warnings` in the consolidated workflow. Remove the `rust_strict_delta_gate.sh` script — with `--workspace -D warnings` always enforced clean, the delta concept is implicit.

**D4: Formalise action pinning policy**

Add a `SECURITY.md` note and a CI check that validates all `uses:` references in workflow files are SHA-pinned. Add `dependabot` configuration for GitHub Actions updates.

**D5: Add daily advisory scan workflow**

Add `daily-audit.yml` as a scheduled workflow running `cargo deny check advisories` against `master` at 09:00 UTC. On failure, open a GitHub Issue with the advisory details using `gh issue create`.

#### Success Metrics for Phase 1

- Single PR workflow file, no duplication
- Security gate passes clean on `master` with documented triage for all pre-existing advisories
- `cargo clippy --workspace` runs and passes clean
- No mutable action tag references in any workflow file
- Daily advisory scan operational

---

### Phase 2 · v0.8.0 — "Workspace-Aware"

**Theme:** The pipeline understands the workspace. Fast feedback for focused changes.

**Why this phase:** By v0.8.0 the workspace will have grown further. Running the full pipeline on every PR will be increasingly expensive. Contributors to `zeroclaw-tool-call-parser` should not wait 30 minutes for a gateway rebuild.

#### Deliverables

**D1: Changed-crate detection**

Add a `scripts/ci/affected_crates.sh` script that uses `cargo metadata` to build the dependency graph and returns the set of crates affected by the PR's changed files. The CI workflow uses this output to scope test execution.

**D2: Per-crate test scoping**

Add `--package` flags to `cargo nextest` based on the affected-crate output. Full workspace tests continue to run on `master` pushes and nightly. PRs run the affected subset.

**D3: Workspace-aware cache configuration**

Update `Swatinem/rust-cache` configuration with explicit workspace scoping and `save-if: ${{ github.ref == 'refs/heads/master' }}` to prevent cache thrashing from concurrent PRs.

**D4: Extract reusable workflow definitions**

Extract the build, test, and security jobs into reusable workflow files under `.github/_workflows/`. Update `ci.yml` and the new `release.yml` skeleton to call them.

#### Success Metrics for Phase 2

- A PR touching only `zeroclaw-tool-call-parser` runs tests for that crate and its dependents, not the full workspace
- Cache hit rate on CI above 80% for incremental builds
- Reusable workflows in place for build, test, and security jobs

---

### Phase 3 · v0.9.0 — "Release Pipeline"

**Theme:** Release automation that matches the distribution model.

**Why this phase:** Phase 3 of the architecture RFC extracts `zeroclaw-gw` as a separate binary. The first multi-artifact release happens here. The release pipeline must be ready before it is needed.

#### Deliverables

**D1: Introduce `release-plz` and remove `version-sync.yml`**

Configure `release-plz` for the workspace. Workspace application crates use `version.workspace = true`. `zeroclaw-api` and hardware library crates are configured with independent release settings. The `version-sync.yml` workflow is retired.

**D2: Build the structured release pipeline in `release.yml`**

Implement the directed release graph from §5.2: `build-kernel-standard`, `build-kernel-hardware`, `build-gateway`, with downstream publish jobs. Plugin build jobs are stubbed — they succeed with no-op until Phase 4.

**D3: Add SLSA Level 2 provenance**

Add `actions/attest-build-provenance` to each build job. Provenance attestations are attached to GitHub Release assets. Document verification instructions in `SECURITY.md`.

**D4: Retire redundant release workflows**

Consolidate `release-stable-manual.yml`, `release-beta-on-push.yml`, `pub-aur.yml`, `pub-homebrew-core.yml`, `pub-scoop.yml`, `discord-release.yml`, `tweet-release.yml` into the structured `release.yml` pipeline. These workflows grew independently; the structured pipeline replaces them with a single, auditable flow.

#### Success Metrics for Phase 3

- `release-plz` opens and manages Release PRs on `master`
- Kernel and gateway binaries are built and published from a single `release.yml` workflow
- SLSA Level 2 provenance attached to all release assets
- Redundant release workflows retired

---

### Phase 4 · v1.0.0 — "Platform Pipeline"

**Theme:** The pipeline ships the platform, not just the binary.

**Why this phase:** v1.0.0 is when WASM plugins become publishable. The pipeline must handle plugin publishing, registry upload, and the Tauri desktop installer as first-class release artifacts.

#### Deliverables

**D1: Activate WASM plugin build jobs**

Implement `build-plugins-wasm` in the release pipeline. Each plugin crate builds to `wasm32-wasip1` in a dedicated job. Plugin manifests are generated and signed. The `publish-plugin-registry` job uploads signed WASM files to the plugin registry.

**D2: Desktop installer build and publish**

Complete the Tauri build jobs for macOS, Windows, and Linux. The installer bundles the kernel and gateway binaries. Code signing credentials for macOS and Windows are documented as required repository secrets with a setup guide.

**D3: Publish the CI/CD standards to `docs/book/src/maintainers/ci-and-actions.md`**

The action pinning policy, advisory triage process, conventional commit requirements, and release pipeline structure defined in this RFC are extracted to `docs/book/src/maintainers/ci-and-actions.md` as a standing reference. This RFC remains the historical record of the decisions; the extracted document is what contributors look up day-to-day.

**D4: Contributor onboarding for the pipeline**

Add a `Running CI Locally` section to the contributing documentation that shows contributors how to replicate the CI checks on their own machine before pushing:

```bash
# What CI runs — run these before pushing
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo deny check
```

#### Success Metrics for Phase 4

- WASM plugin files are published to the registry as part of the release pipeline
- Tauri desktop installer is built and published automatically on release
- `docs/book/src/maintainers/ci-and-actions.md` exists and covers action pinning, advisory triage, and conventional commits
- A contributor can replicate all CI checks locally with four commands

---

## 8. What This Means for Contributors

### For contributors opening PRs

The consolidated pipeline means one place to look for results. Stage 1 (format and lint) fails fast — if you have a formatting error, you know in two minutes without waiting for a build. If Stage 1 passes, the build and test stages run in parallel and you have a full result in under 30 minutes for most changes.

The conventional commit requirement on PR titles is enforced by CI. If your title does not match the format, the lint job fails immediately with a clear message. This is not bureaucracy — it is the input that generates the changelog automatically, which means releases happen faster and with less manual work.

### For contributors adding dependencies

Every new dependency passes through `cargo deny`. If the dependency has a known vulnerability, an unacceptable license, or comes from an untrusted source, the security gate fails and tells you why. This is by design. The right response is to investigate the dependency, not to suppress the check.

If a dependency carries an advisory that is not fixable (a transitive dep with no available update), the triage process in §4.3 is how you document that. Open a tracking issue, add the ignore entry to `deny.toml` with your justification, and move forward. The security posture is maintained through documentation, not through hoping the advisory goes away.

### For contributors adding workflow files

New workflow files follow three rules without exception:
1. All `uses:` references are SHA-pinned with a version comment
2. New jobs are extracted as reusable workflows if they duplicate logic from an existing job
3. New release-related jobs are added to `release.yml`, not as new workflow files

When in doubt, ask before adding. Workflow files are high-risk changes — they run with elevated permissions on CI infrastructure and can affect supply chain security. They deserve the same review standard as `src/security/`.

### For maintainers

The daily advisory scan means security is a regular maintenance task, not a crisis. When a new advisory fires, the triage process is well-defined and the outcome is documented in `deny.toml` and a tracking issue. Reviewers can audit the full history of advisory decisions in git history.

The Release PR from `release-plz` is the release review checkpoint. Before anything is published, the team sees the version, the changelog, and the list of changed crates. Releases do not happen by accident.

---


## Appendix A: Glossary

**SLSA (Supply-chain Levels for Software Artifacts)** — A security framework that defines levels of build integrity, from basic provenance to fully hermetic builds. Developed by Google and adopted by the OpenSSF. Level 2 is the practical target for most open-source projects: hosted build platform, version-controlled build scripts, signed provenance attached to artifacts.

**Provenance** — A cryptographically signed record of where a build artifact came from: which source commit, which workflow, which platform. Allows users and package managers to verify that a binary was produced from the claimed source by the claimed process.

**`cargo deny`** — A Cargo plugin that enforces dependency policy across three dimensions: security advisories (from the RustSec database), software licenses (against a defined allowlist), and source registries (ensuring deps come only from approved locations). More configurable than `cargo audit` and better suited to policy management at scale.

**`release-plz`** — A Rust-ecosystem release automation tool that creates "Release PRs" on push to the default branch, bumping versions and generating changelogs from conventional commit history. Workspace-aware; understands which crates changed and which need new versions.

**Reusable workflow** — A GitHub Actions workflow that can be called as a job from another workflow, with parameters. Allows build, test, and security logic to be defined once and called from both the PR pipeline and the release pipeline.

**Conventional commits** — A commit message convention (`feat:`, `fix:`, `chore:`, etc.) that enables automated changelog generation and version determination. The input that tools like `release-plz` use to decide whether a release is a patch, minor, or major bump.

**Strangler Fig (in pipeline context)** — The same migration strategy applied to workflows: build the new pipeline structure alongside the existing one, migrate jobs one at a time, retire the old files only when the new structure is complete and verified.

---

## Appendix B: Further Reading

- **SLSA Framework** — https://slsa.dev — The full specification and implementation guides for supply chain security levels.

- **`cargo deny` documentation** — https://embarkstudios.github.io/cargo-deny/ — Configuration reference for the `deny.toml` policy file, including all advisory, license, and source options.

- **`release-plz` documentation** — https://release-plz.eplant.org — Workspace configuration, changelog format customisation, and GitHub Actions integration guide.

- **GitHub Actions security hardening** — https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions — Official guidance on SHA pinning, token permissions, and supply chain risk in Actions workflows.

- **Conventional Commits specification** — https://www.conventionalcommits.org — The full specification for commit message format and its relationship to semantic versioning.

- **OpenSSF Scorecard** — https://securityscorecards.dev — An automated tool that scores open-source projects on security practices including dependency pinning, branch protection, code review requirements, and more. Useful as a baseline assessment and ongoing health metric.
