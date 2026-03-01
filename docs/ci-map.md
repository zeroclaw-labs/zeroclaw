# CI Workflow Map

This document explains what each GitHub workflow does, when it runs, and whether it should block merges.

For event-by-event delivery behavior across PR, merge, push, and release, see [`.github/workflows/main-branch-flow.md`](../.github/workflows/main-branch-flow.md).

## Merge-Blocking vs Optional

Merge-blocking checks should stay small and deterministic. Optional checks are useful for automation and maintenance, but should not block normal development.

### Merge-Blocking

- `.github/workflows/ci-run.yml` (`CI`)
    - Purpose: Rust validation (`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D clippy::correctness`, strict delta lint gate on changed Rust lines, `test`, release build smoke) + docs quality checks when docs change (`markdownlint` blocks only issues on changed lines; link check scans only links added on changed lines)
    - Additional behavior: for Rust-impacting PRs and pushes, `CI Required Gate` requires `lint` + `test` + `build` (no PR build-only bypass)
    - Additional behavior: `lint`, `test`, and `build` run in parallel (all depend only on `changes` job) to minimize critical path duration
    - Additional behavior: rust-cache is shared between `lint` and `test` via unified `prefix-key` (`ci-run-check`) to reduce redundant compilation; `build` uses a separate key for release-fast profile
    - Additional behavior: flake detection is integrated into the `test` job via single-retry probe; emits `test-flake-probe` artifact when flake is suspected; optional blocking can be enabled with repository variable `CI_BLOCK_ON_FLAKE_SUSPECTED=true`
    - Additional behavior: PRs that change CI/CD-governed paths require an explicit approving review from `@chumyin` (`.github/workflows/**`, `.github/codeql/**`, `.github/connectivity/**`, `.github/release/**`, `.github/security/**`, `.github/actionlint.yaml`, `.github/dependabot.yml`, `scripts/ci/**`, and CI governance docs)
    - Additional behavior: PRs that change root license files (`LICENSE-APACHE`, `LICENSE-MIT`) must be authored by `willsarg`
    - Additional behavior: when lint/docs gates fail on PRs, CI posts an actionable feedback comment with failing gate names and local fix commands
    - Merge gate: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
    - Purpose: lint GitHub workflow files (`actionlint`, tab checks)
    - Recommended for workflow-changing PRs
- `.github/workflows/pr-intake-checks.yml` (`PR Intake Checks`)
    - Purpose: safe pre-CI PR checks (template completeness, added-line tabs/trailing-whitespace/conflict markers) with immediate sticky feedback comment

### Non-Blocking but Important

- `.github/workflows/pub-docker-img.yml` (`Docker`)
    - Purpose: PR Docker smoke check on `dev`/`main` PRs and publish images on tag pushes (`v*`) only
    - Additional behavior: `ghcr_publish_contract_guard.py` enforces GHCR publish contract from `.github/release/ghcr-tag-policy.json` (`vX.Y.Z`, `sha-<12>`, `latest` digest parity + rollback mapping evidence)
    - Additional behavior: `ghcr_vulnerability_gate.py` enforces policy-driven Trivy gate + parity checks from `.github/release/ghcr-vulnerability-policy.json` and emits `ghcr-vulnerability-gate` audit evidence
- `.github/workflows/feature-matrix.yml` (`Feature Matrix`)
    - Purpose: compile-time matrix validation for `default`, `whatsapp-web`, `browser-native`, and `nightly-all-features` lanes
    - Additional behavior: push-triggered matrix runs are limited to `dev` branch Rust/workflow-path changes to avoid duplicate post-merge fan-out on `main`
    - Additional behavior: on PRs, lanes only run when `ci:full` or `ci:feature-matrix` label is applied (push-to-dev and schedules run unconditionally)
    - Additional behavior: each lane emits machine-readable result artifacts; summary lane aggregates owner routing from `.github/release/nightly-owner-routing.json`
    - Additional behavior: supports `compile` (merge-gate) and `nightly` (integration-oriented) profiles with bounded retry policy and trend snapshot artifact (`nightly-history.json`)
    - Additional behavior: required-check mapping is anchored to stable job name `Feature Matrix Summary`; lane jobs stay informational
- `.github/workflows/nightly-all-features.yml` (`Nightly All-Features`)
    - Purpose: legacy/dev-only nightly template; primary nightly signal is emitted by `feature-matrix.yml` nightly profile
    - Additional behavior: owner routing + escalation policy is documented in `docs/operations/nightly-all-features-runbook.md`
- `.github/workflows/sec-audit.yml` (`Security Audit`)
    - Purpose: dependency advisories (`rustsec/audit-check`, pinned SHA), policy/license checks (`cargo deny`), gitleaks-based secrets governance (allowlist policy metadata + expiry guard), and SBOM snapshot artifacts (`CycloneDX` + `SPDX`)
- `.github/workflows/sec-codeql.yml` (`CodeQL Analysis`)
    - Purpose: static analysis for security findings on PR/push (Rust/codeql paths) plus scheduled/manual runs
- `.github/workflows/ci-change-audit.yml` (`CI/CD Change Audit`)
    - Purpose: machine-auditable diff report for CI/security workflow changes (line churn, new `uses:` references, unpinned action-policy violations, pipe-to-shell policy violations, broad `permissions: write-all` grants, new `pull_request_target` trigger introductions, new secret references)
- `.github/workflows/ci-provider-connectivity.yml` (`CI Provider Connectivity`)
    - Purpose: scheduled/manual/provider-list probe matrix with downloadable JSON/Markdown artifacts for provider endpoint reachability
- `.github/workflows/ci-reproducible-build.yml` (`CI Reproducible Build`)
    - Purpose: deterministic build drift probe (double clean-build hash comparison) with structured artifacts
- `.github/workflows/ci-supply-chain-provenance.yml` (`CI Supply Chain Provenance`)
    - Purpose: release-fast artifact provenance statement generation + keyless signature bundle for supply-chain traceability
- `.github/workflows/ci-rollback.yml` (`CI Rollback Guard`)
    - Purpose: deterministic rollback plan generation with guarded execute mode, marker-tag option, rollback audit artifacts, and dispatch contract for canary-abort auto-triggering
- `.github/workflows/sec-vorpal-reviewdog.yml` (`Sec Vorpal Reviewdog`)
    - Purpose: manual secure-coding feedback scan for supported non-Rust files (`.py`, `.js`, `.jsx`, `.ts`, `.tsx`) using reviewdog annotations
    - Noise control: excludes common test/fixture paths and test file patterns by default (`include_tests=false`)
- `.github/workflows/pub-release.yml` (`Release`)
    - Purpose: build release artifacts in verification mode (manual/scheduled) and publish GitHub releases on tag push or manual publish mode
- `.github/workflows/pr-label-policy-check.yml` (`Label Policy Sanity`)
    - Purpose: validate shared contributor-tier policy in `.github/label-policy.json` and ensure label workflows consume that policy

### Optional Repository Automation

- `.github/workflows/pr-labeler.yml` (`PR Labeler`)
    - Purpose: scope/path labels + size/risk labels + fine-grained module labels (`<module>: <component>`)
    - Additional behavior: label descriptions are auto-managed as hover tooltips to explain each auto-judgment rule
    - Additional behavior: provider-related keywords in provider/config/onboard/integration changes are promoted to `provider:*` labels (for example `provider:kimi`, `provider:deepseek`)
    - Additional behavior: hierarchical de-duplication keeps only the most specific scope labels (for example `tool:composio` suppresses `tool:core` and `tool`)
    - Additional behavior: module namespaces are compacted — one specific module keeps `prefix:component`; multiple specifics collapse to just `prefix`
    - Additional behavior: applies contributor tiers on PRs by merged PR count (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50)
    - Additional behavior: final label set is priority-sorted (`risk:*` first, then `size:*`, then contributor tier, then module/path labels)
    - Additional behavior: managed label colors follow display order to produce a smooth left-to-right gradient when many labels are present
    - Manual governance: supports `workflow_dispatch` with `mode=audit|repair` to inspect/fix managed label metadata drift across the whole repository
    - Additional behavior: risk + size labels are recomputed on PR lifecycle events (`opened`/`reopened`/`synchronize`/`ready_for_review`); maintainers can use manual `workflow_dispatch` (`mode=repair`) to re-sync managed label metadata after exceptional manual edits
    - High-risk heuristic paths: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`
    - Guardrail: maintainers can apply `risk: manual` to freeze automated risk recalculation
- `.github/workflows/pr-auto-response.yml` (`PR Auto Responder`)
    - Purpose: first-time contributor onboarding + label-driven response routing (`r:support`, `r:needs-repro`, etc.)
    - Additional behavior: applies contributor tiers on issues by merged PR count (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), matching PR tier thresholds exactly
    - Additional behavior: contributor-tier labels are treated as automation-managed (manual add/remove on PR/issue is auto-corrected)
    - Guardrail: label-based close routes are issue-only; PRs are never auto-closed by route labels
- `.github/workflows/pr-check-stale.yml` (`Stale`)
    - Purpose: stale issue/PR lifecycle automation
- `.github/dependabot.yml` (`Dependabot`)
    - Purpose: grouped, rate-limited dependency update PRs (Cargo + GitHub Actions)
- `.github/workflows/pr-check-status.yml` (`PR Hygiene`)
    - Purpose: nudge stale-but-active PRs to rebase/re-run required checks before queue starvation

## Trigger Map

- `CI`: push to `dev` and `main`, PRs to `dev` and `main`, merge queue `merge_group` for `dev`/`main`
- `Docker`: tag push (`v*`) for publish, matching PRs to `dev`/`main` for smoke build, manual dispatch for smoke only
- `Feature Matrix`: push on Rust + workflow paths to `dev`, merge queue, weekly schedule, manual dispatch; PRs only when `ci:full` or `ci:feature-matrix` label is applied
- `Nightly All-Features`: daily schedule and manual dispatch
- `Release`: tag push (`v*`), weekly schedule (verification-only), manual dispatch (verification or publish)
- `Security Audit`: push to `dev` and `main`, PRs to `dev` and `main`, weekly schedule
- `Sec Vorpal Reviewdog`: manual dispatch only
- `Workflow Sanity`: PR/push when `.github/workflows/**`, `.github/*.yml`, or `.github/*.yaml` change
- `Dependabot`: all update PRs target `main` (not `dev`)
- `PR Intake Checks`: `pull_request_target` on opened/reopened/synchronize/ready_for_review
- `Label Policy Sanity`: PR/push when `.github/label-policy.json`, `.github/workflows/pr-labeler.yml`, or `.github/workflows/pr-auto-response.yml` changes
- `PR Labeler`: `pull_request_target` on opened/reopened/synchronize/ready_for_review
- `PR Auto Responder`: issue opened/labeled, `pull_request_target` opened/labeled
- `Test E2E`: push to `dev`/`main` for Rust-impacting paths (`Cargo*`, `src/**`, `crates/**`, `tests/**`, `scripts/**`) and manual dispatch
- `Stale PR Check`: daily schedule, manual dispatch
- `PR Hygiene`: every 12 hours schedule, manual dispatch

## Fast Triage Guide

1. `CI Required Gate` failing: start with `.github/workflows/ci-run.yml`.
2. Docker failures on PRs: inspect `.github/workflows/pub-docker-img.yml` `pr-smoke` job.
   - For tag-publish failures, inspect `ghcr-publish-contract.json` / `audit-event-ghcr-publish-contract.json`, `ghcr-vulnerability-gate.json` / `audit-event-ghcr-vulnerability-gate.json`, and Trivy artifacts from `pub-docker-img.yml`.
3. Release failures (tag/manual/scheduled): inspect `.github/workflows/pub-release.yml` and the `prepare` job outputs.
4. Security failures: inspect `.github/workflows/sec-audit.yml` and `deny.toml`.
5. Workflow syntax/lint failures: inspect `.github/workflows/workflow-sanity.yml`.
6. PR intake failures: inspect `.github/workflows/pr-intake-checks.yml` sticky comment and run logs.
7. Label policy parity failures: inspect `.github/workflows/pr-label-policy-check.yml`.
8. Docs failures in CI: inspect `docs-quality` job logs in `.github/workflows/ci-run.yml`.
9. Strict delta lint failures in CI: inspect `lint-strict-delta` job logs and compare with `BASE_SHA` diff scope.

## Maintenance Rules

- Keep merge-blocking checks deterministic and reproducible (`--locked` where applicable).
- Keep merge-queue compatibility explicit by supporting `merge_group` on required workflows (`ci-run`, `sec-audit`, and `sec-codeql`).
- Keep PRs mapped to Linear issue keys (`RMN-*`/`CDV-*`/`COM-*`) via PR intake checks.
- Keep `deny.toml` advisory ignore entries in object form with explicit reasons (enforced by `deny_policy_guard.py`).
- Keep deny ignore governance metadata current in `.github/security/deny-ignore-governance.json` (owner/reason/expiry/ticket enforced by `deny_policy_guard.py`).
- Keep gitleaks allowlist governance metadata current in `.github/security/gitleaks-allowlist-governance.json` (owner/reason/expiry/ticket enforced by `secrets_governance_guard.py`).
- Keep audit event schema + retention metadata aligned with `docs/audit-event-schema.md` (`emit_audit_event.py` envelope + workflow artifact policy).
- Keep rollback operations guarded and reversible (`ci-rollback.yml` defaults to `dry-run`; `execute` is manual and policy-gated).
- Keep canary policy thresholds and sample-size rules current in `.github/release/canary-policy.json`.
- Keep GHCR tag taxonomy and immutability policy current in `.github/release/ghcr-tag-policy.json` and `docs/operations/ghcr-tag-policy.md`.
- Keep GHCR vulnerability gate policy current in `.github/release/ghcr-vulnerability-policy.json` and `docs/operations/ghcr-vulnerability-policy.md`.
- Keep pre-release stage transition policy + matrix coverage + transition audit semantics current in `.github/release/prerelease-stage-gates.json`.
- Keep required check naming stable and documented in `docs/operations/required-check-mapping.md` before changing branch protection settings.
- Follow `docs/release-process.md` for verify-before-publish release cadence and tag discipline.
- Keep merge-blocking rust quality policy aligned across `.github/workflows/ci-run.yml`, `dev/ci.sh`, and `.githooks/pre-push` (`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`).
- Use `./scripts/ci/rust_strict_delta_gate.sh` (or `./dev/ci.sh lint-delta`) as the incremental strict merge gate for changed Rust lines.
- Run full strict lint audits regularly via `./scripts/ci/rust_quality_gate.sh --strict` (for example through `./dev/ci.sh lint-strict`) and track cleanup in focused PRs.
- Keep docs markdown gating incremental via `./scripts/ci/docs_quality_gate.sh` (block changed-line issues, report baseline issues separately).
- Keep docs link gating incremental via `./scripts/ci/collect_changed_links.py` + lychee (check only links added on changed lines).
- Keep docs deploy policy current in `.github/release/docs-deploy-policy.json`, `docs/operations/docs-deploy-policy.md`, and `docs/operations/docs-deploy-runbook.md`.
- Prefer explicit workflow permissions (least privilege).
- Keep Actions source policy restricted to approved allowlist patterns (see `docs/actions-source-policy.md`).
- Use path filters for expensive workflows when practical.
- Keep docs quality checks low-noise (incremental markdown + incremental added-link checks).
- Use `scripts/ci/queue_hygiene.py` for controlled cleanup of obsolete or superseded queued runs during runner-pressure incidents.
- Keep dependency update volume controlled (grouping + PR limits).
- Install third-party CI tooling through repository-managed pinned installers with checksum verification (for example `scripts/ci/install_gitleaks.sh`, `scripts/ci/install_syft.sh`); avoid remote `curl | sh` patterns.
- Avoid mixing onboarding/community automation with merge-gating logic.

## Automation Side-Effect Controls

- Prefer deterministic automation that can be manually overridden (`risk: manual`) when context is nuanced.
- Keep auto-response comments deduplicated to prevent triage noise.
- Keep auto-close behavior scoped to issues; maintainers own PR close/merge decisions.
- If automation is wrong, correct labels first, then continue review with explicit rationale.
- Use `superseded` / `stale-candidate` labels to prune duplicate or dormant PRs before deep review.
