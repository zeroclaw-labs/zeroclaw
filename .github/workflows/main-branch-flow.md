# Main Branch Delivery Flows

This document explains what runs when code is proposed to `dev`, promoted to `main`, and released.

Use this with:

- [`docs/ci-map.md`](../../docs/ci-map.md)
- [`docs/pr-workflow.md`](../../docs/pr-workflow.md)
- [`docs/release-process.md`](../../docs/release-process.md)

## Event Summary

| Event | Main workflows |
| --- | --- |
| PR activity (`pull_request_target`) | `pr-intake-checks.yml`, `pr-labeler.yml`, `pr-auto-response.yml` |
| PR activity (`pull_request`) | `ci-run.yml`, `sec-audit.yml`, `main-promotion-gate.yml` (for `main` PRs), plus path-scoped workflows |
| Push to `dev`/`main` | `ci-run.yml`, `sec-audit.yml`, plus path-scoped workflows |
| Tag push (`v*`) | `pub-release.yml` publish mode, `pub-docker-img.yml` publish job |
| Scheduled/manual | `pub-release.yml` verification mode, `sec-codeql.yml`, `feature-matrix.yml`, `test-fuzz.yml`, `pr-check-stale.yml`, `pr-check-status.yml`, `sync-contributors.yml`, `test-benchmarks.yml`, `test-e2e.yml` |

## Runtime and Docker Matrix

Observed averages below are from recent completed runs (sampled from GitHub Actions on February 17, 2026). Values are directional, not SLA.

| Workflow | Typical trigger in main flow | Avg runtime | Docker build? | Docker run? | Docker push? |
| --- | --- | ---:| --- | --- | --- |
| `pr-intake-checks.yml` | PR open/update (`pull_request_target`) | 14.5s | No | No | No |
| `pr-labeler.yml` | PR open/update (`pull_request_target`) | 53.7s | No | No | No |
| `pr-auto-response.yml` | PR/issue automation | 24.3s | No | No | No |
| `ci-run.yml` | PR + push to `dev`/`main` | 74.7s | No | No | No |
| `sec-audit.yml` | PR + push to `dev`/`main` | 127.2s | No | No | No |
| `workflow-sanity.yml` | Workflow-file changes | 34.2s | No | No | No |
| `pr-label-policy-check.yml` | Label policy/automation changes | 14.7s | No | No | No |
| `pub-docker-img.yml` (`pull_request`) | Docker build-input PR changes | 240.4s | Yes | Yes | No |
| `pub-docker-img.yml` (`push`) | tag push `v*` | 139.9s | Yes | No | Yes |
| `pub-release.yml` | Tag push `v*` (publish) + manual/scheduled verification (no publish) | N/A in recent sample | No | No | No |

Notes:

1. `pub-docker-img.yml` is the only workflow in the main PR/push path that builds Docker images.
2. Container runtime verification (`docker run`) occurs in PR smoke only.
3. Container registry push occurs on tag pushes (`v*`) only.
4. `ci-run.yml` "Build (Smoke)" builds Rust binaries, not Docker images.

## Step-By-Step

### 1) PR from branch in this repository -> `dev`

1. Contributor opens or updates PR against `dev`.
2. `pull_request_target` automation runs (typical runtime):
   - `pr-intake-checks.yml` posts intake warnings/errors.
   - `pr-labeler.yml` sets size/risk/scope labels.
   - `pr-auto-response.yml` runs first-interaction and label routes.
3. `pull_request` CI workflows start:
   - `ci-run.yml`
   - `feature-matrix.yml` (Rust/workflow path scope)
   - `sec-audit.yml`
   - `sec-codeql.yml` (if Rust/codeql paths changed)
     - path-scoped workflows if matching files changed:
       - `pub-docker-img.yml` (Docker build-input paths only)
       - `docs-deploy.yml` (docs + README markdown paths; deploy contract guard enforces promotion + rollback ref policy)
       - `workflow-sanity.yml` (workflow files only)
     - `pr-label-policy-check.yml` (label-policy files only)
     - `ci-change-audit.yml` (CI/security path changes)
     - `ci-provider-connectivity.yml` (probe config/script/workflow changes)
     - `ci-reproducible-build.yml` (Rust/build reproducibility paths)
4. In `ci-run.yml`, `changes` computes:
   - `docs_only`
   - `docs_changed`
   - `rust_changed`
   - `workflow_changed`
5. `build` runs for Rust-impacting changes.
6. On PRs, full lint/test/docs checks run when PR has label `ci:full`:
   - `lint`
   - `lint-strict-delta`
   - `test`
   - `flake-probe` (single-retry telemetry; optional block via `CI_BLOCK_ON_FLAKE_SUSPECTED`)
   - `docs-quality`
7. If `.github/workflows/**` changed, `workflow-owner-approval` must pass.
8. If root license files (`LICENSE-APACHE`, `LICENSE-MIT`) changed, `license-file-owner-guard` allows only PR author `willsarg`.
9. `lint-feedback` posts actionable comment if lint/docs gates fail.
10. `CI Required Gate` aggregates results to final pass/fail.
11. Maintainer merges PR once checks and review policy are satisfied.
12. Merge emits a `push` event on `dev` (see scenario 4).

### 2) PR from fork -> `dev`

1. External contributor opens PR from `fork/<branch>` into `zeroclaw:dev`.
2. Immediately on `opened`:
   - `pull_request_target` workflows start with base-repo context and base-repo token:
     - `pr-intake-checks.yml`
     - `pr-labeler.yml`
     - `pr-auto-response.yml`
   - `pull_request` workflows are queued for the fork head commit:
     - `ci-run.yml`
     - `sec-audit.yml`
     - path-scoped workflows (`pub-docker-img.yml`, `workflow-sanity.yml`, `pr-label-policy-check.yml`) if changed files match.
3. Fork-specific permission behavior in `pull_request` workflows:
   - token is restricted (read-focused), so jobs that try to write PR comments/status extras can be limited.
   - secrets from the base repo are not exposed to fork PR `pull_request` jobs.
4. Approval gate possibility:
   - if Actions settings require maintainer approval for fork workflows, the `pull_request` run stays in `action_required`/waiting state until approved.
5. Event fan-out after labeling:
   - manual label changes emit `labeled`/`unlabeled` events.
   - those events retrigger only label-driven `pull_request_target` automation (`pr-auto-response.yml`); `pr-labeler.yml` now runs only on PR lifecycle events (`opened`/`reopened`/`synchronize`/`ready_for_review`) to reduce churn.
6. When contributor pushes new commits to fork branch (`synchronize`):
   - reruns: `pr-intake-checks.yml`, `pr-labeler.yml`, `ci-run.yml`, `sec-audit.yml`, and matching path-scoped PR workflows.
   - does not rerun `pr-auto-response.yml` unless label/open events occur.
7. `ci-run.yml` execution details for fork PR:
   - `changes` computes `docs_only`, `docs_changed`, `rust_changed`, `workflow_changed`.
   - `build` runs for Rust-impacting changes.
   - `lint`/`lint-strict-delta`/`test`/`docs-quality` run on PR when `ci:full` label exists.
   - `workflow-owner-approval` runs when `.github/workflows/**` changed.
   - `CI Required Gate` emits final pass/fail for the PR head.
8. Fork PR merge blockers to check first when diagnosing stalls:
   - run approval pending for fork workflows.
   - `workflow-owner-approval` failing on workflow-file changes.
   - `license-file-owner-guard` failing when root license files are modified by non-owner PR author.
   - `CI Required Gate` failure caused by upstream jobs.
   - repeated `pull_request_target` reruns from label churn causing noisy signals.
9. After merge, normal `push` workflows on `dev` execute (scenario 4).

### 3) Promotion PR `dev` -> `main`

1. Maintainer opens PR with head `dev` and base `main`.
2. `main-promotion-gate.yml` runs and fails unless PR author is `willsarg` or `theonlyhennygod`.
3. `main-promotion-gate.yml` also fails if head repo/branch is not `<this-repo>:dev`.
4. `ci-run.yml` and `sec-audit.yml` run on the promotion PR.
5. Maintainer merges PR once checks and review policy pass.
6. Merge emits a `push` event on `main`.

### 4) Push/Merge Queue to `dev` or `main` (including after merge)

1. Commit reaches `dev` or `main` (usually from a merged PR), or merge queue creates a `merge_group` validation commit.
2. `ci-run.yml` runs on `push` and `merge_group`.
3. `feature-matrix.yml` runs on `push` to `dev` for Rust/workflow paths and on `merge_group`.
4. `sec-audit.yml` runs on `push` and `merge_group`.
5. `sec-codeql.yml` runs on `push`/`merge_group` when Rust/codeql paths change (path-scoped on push).
6. `ci-supply-chain-provenance.yml` runs on push when Rust/build provenance paths change.
7. Path-filtered workflows run only if touched files match their filters.
8. In `ci-run.yml`, push/merge-group behavior differs from PR behavior:
   - Rust path: `lint`, `lint-strict-delta`, `test`, `build` are expected.
   - Docs/non-rust paths: fast-path behavior applies.
9. `CI Required Gate` computes overall push/merge-group result.

## Docker Publish Logic

Workflow: `.github/workflows/pub-docker-img.yml`

### PR behavior

1. Triggered on `pull_request` to `dev` or `main` when Docker build-input paths change.
2. Runs `PR Docker Smoke` job:
   - Builds local smoke image with Buildx builder.
   - Verifies container with `docker run ... --version`.
3. Typical runtime in recent sample: ~240.4s.
4. No registry push happens on PR events.

### Push behavior

1. `publish` job runs on tag pushes `v*` only.
2. Workflow trigger includes semantic version tag pushes (`v*`) only.
3. Login to `ghcr.io` uses `${{ github.actor }}` and `${{ secrets.GITHUB_TOKEN }}`.
4. Tag computation includes semantic tag from pushed git tag (`vX.Y.Z`) + SHA tag (`sha-<12>`) + `latest`.
5. Multi-platform publish is used for tag pushes (`linux/amd64,linux/arm64`).
6. `scripts/ci/ghcr_publish_contract_guard.py` validates anonymous pullability and digest parity across `vX.Y.Z`, `sha-<12>`, and `latest`, then emits rollback candidate mapping evidence.
7. Trivy scans are emitted for version, SHA, and latest references.
8. `scripts/ci/ghcr_vulnerability_gate.py` validates Trivy JSON outputs against `.github/release/ghcr-vulnerability-policy.json` and emits audit-event evidence.
9. Typical runtime in recent sample: ~139.9s.
10. Result: pushed image tags under `ghcr.io/<owner>/<repo>` with publish-contract + vulnerability-gate + scan artifacts.

Important: Docker publish now requires a `v*` tag push; regular `dev`/`main` branch pushes do not publish images.

## Release Logic

Workflow: `.github/workflows/pub-release.yml`

1. Trigger modes:
   - Tag push `v*` -> publish mode.
   - Manual dispatch -> verification-only or publish mode (input-driven).
   - Weekly schedule -> verification-only mode.
2. `prepare` resolves release context (`release_ref`, `release_tag`, publish/draft mode) and runs `scripts/ci/release_trigger_guard.py`.
   - publish mode enforces actor authorization, stable annotated tag policy, `origin/main` ancestry, and `release_tag` == `Cargo.toml` version at the tag commit.
   - trigger provenance is emitted as `release-trigger-guard` artifacts.
3. `build-release` builds matrix artifacts across Linux/macOS/Windows targets.
4. `verify-artifacts` runs `scripts/ci/release_artifact_guard.py` against `.github/release/release-artifact-contract.json` in verify-stage mode (archive contract required; manifest/SBOM/notice checks intentionally skipped) and uploads `release-artifact-guard-verify` evidence.
5. In publish mode, workflow generates SBOM (`CycloneDX` + `SPDX`), `SHA256SUMS`, and a checksum provenance statement (`zeroclaw.sha256sums.intoto.json`) plus audit-event envelope.
6. In publish mode, after manifest generation, workflow reruns `release_artifact_guard.py` in full-contract mode and emits `release-artifact-guard.publish.json` plus `audit-event-release-artifact-guard-publish.json`.
7. In publish mode, workflow keyless-signs release artifacts and composes a supply-chain release-notes preface via `release_notes_with_supply_chain_refs.py`.
8. In publish mode, workflow verifies GHCR release-tag availability.
9. In publish mode, workflow creates/updates the GitHub Release for the resolved tag and commit-ish, combining generated supply-chain preface with GitHub auto-generated commit notes.

Pre-release path:

1. Pre-release tags (`vX.Y.Z-alpha.N`, `vX.Y.Z-beta.N`, `vX.Y.Z-rc.N`) trigger `.github/workflows/pub-prerelease.yml`.
2. `scripts/ci/prerelease_guard.py` enforces stage progression, `origin/main` ancestry, and Cargo version/tag alignment.
3. In publish mode, prerelease assets are attached to a GitHub prerelease for the stage tag.

Canary policy lane:

1. `.github/workflows/ci-canary-gate.yml` runs weekly or manually.
2. `scripts/ci/canary_guard.py` evaluates metrics against `.github/release/canary-policy.json`.
3. Decision output is explicit (`promote`, `hold`, `abort`) with auditable artifacts and optional dispatch signal.

## Merge/Policy Notes

1. Workflow-file changes (`.github/workflows/**`) activate owner-approval gate in `ci-run.yml`.
2. PR lint/test strictness is intentionally controlled by `ci:full` label.
3. `pr-intake-checks.yml` now blocks PRs missing a Linear issue key (`RMN-*`, `CDV-*`, `COM-*`) to keep execution mapped to Linear.
4. `sec-audit.yml` runs on PR/push/merge queue (`merge_group`), plus scheduled weekly.
5. `ci-change-audit.yml` enforces pinned `uses:` references for CI/security workflow changes.
6. `sec-audit.yml` includes deny policy hygiene checks (`deny_policy_guard.py`) before cargo-deny.
7. `sec-audit.yml` includes gitleaks allowlist governance checks (`secrets_governance_guard.py`) against `.github/security/gitleaks-allowlist-governance.json`.
8. `ci-reproducible-build.yml` and `ci-supply-chain-provenance.yml` provide scheduled supply-chain assurance signals outside release-only windows.
9. Some workflows are operational and non-merge-path (`pr-check-stale`, `pr-check-status`, `sync-contributors`, etc.).
10. Workflow-specific JavaScript helpers are organized under `.github/workflows/scripts/`.
11. `ci-run.yml` includes cache partitioning (`prefix-key`) across lint/test/build/flake-probe lanes to reduce cache contention.
12. `ci-rollback.yml` provides a guarded rollback planning lane (scheduled dry-run + manual execute controls) with audit artifacts.

## Mermaid Diagrams

### PR to Dev

```mermaid
flowchart TD
  A["PR opened or updated -> dev"] --> B["pull_request_target lane"]
  B --> B1["pr-intake-checks.yml"]
  B --> B2["pr-labeler.yml"]
  B --> B3["pr-auto-response.yml"]
  A --> C["pull_request CI lane"]
  C --> C1["ci-run.yml"]
  C --> C2["sec-audit.yml"]
  C --> C3["pub-docker-img.yml (if Docker paths changed)"]
  C --> C4["workflow-sanity.yml (if workflow files changed)"]
  C --> C5["pr-label-policy-check.yml (if policy files changed)"]
  C1 --> D["CI Required Gate"]
  D --> E{"Checks + review policy pass?"}
  E -->|No| F["PR stays open"]
  E -->|Yes| G["Merge PR"]
  G --> H["push event on dev"]
```

### Promotion and Release

```mermaid
flowchart TD
  D0["Commit reaches dev"] --> B0["ci-run.yml"]
  D0 --> C0["sec-audit.yml"]
  P["Promotion PR dev -> main"] --> PG["main-promotion-gate.yml"]
  PG --> M["Merge to main"]
  M --> A["Commit reaches main"]
  A --> B["ci-run.yml"]
  A --> C["sec-audit.yml"]
  A --> D["path-scoped workflows (if matched)"]
  T["Tag push v*"] --> R["pub-release.yml"]
  W["Manual/Scheduled release verify"] --> R
  T --> P["pub-docker-img.yml publish job"]
  R --> R1["Artifacts + SBOM + checksums + signatures + GitHub Release"]
  W --> R2["Verification build only (no GitHub Release publish)"]
  P --> P1["Push ghcr image tags (version + sha + latest)"]
```

## Quick Troubleshooting

1. Unexpected skipped jobs: inspect `scripts/ci/detect_change_scope.sh` outputs.
2. Workflow-change PR blocked: verify `WORKFLOW_OWNER_LOGINS` and approvals.
3. Fork PR appears stalled: check whether Actions run approval is pending.
4. Docker not published: confirm a `v*` tag was pushed to the intended commit.
