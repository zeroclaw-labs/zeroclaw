# CI Workflow Map

This document explains what each GitHub workflow does, when it runs, and whether it should block merges.

For event-by-event delivery behavior across PR, merge, push, and release, see [`.github/workflows/main-branch-flow.md`](../.github/workflows/main-branch-flow.md).

## Merge-Blocking vs Optional

Merge-blocking checks should stay small and deterministic. Optional checks are useful for automation and maintenance, but should not block normal development.

### Merge-Blocking

- `.github/workflows/ci-run.yml` (`CI`)
    - Purpose: Rust validation (`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D clippy::correctness`, strict delta lint gate on changed Rust lines, `test`, release build smoke) + docs quality checks when docs change (`markdownlint` blocks only issues on changed lines; link check scans only links added on changed lines)
    - Additional behavior: PRs that change `.github/workflows/**` require at least one approving review from a login in `WORKFLOW_OWNER_LOGINS` (repository variable fallback: `theonlyhennygod,willsarg`)
    - Additional behavior: PRs that change root license files (`LICENSE-APACHE`, `LICENSE-MIT`) must be authored by `willsarg`
    - Additional behavior: lint gates run before `test`/`build`; when lint/docs gates fail on PRs, CI posts an actionable feedback comment with failing gate names and local fix commands
    - Merge gate: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
    - Purpose: lint GitHub workflow files (`actionlint`, tab checks)
    - Recommended for workflow-changing PRs
- `.github/workflows/pr-intake-checks.yml` (`PR Intake Checks`)
    - Purpose: safe pre-CI PR checks (template completeness, added-line tabs/trailing-whitespace/conflict markers) with immediate sticky feedback comment
- `.github/workflows/main-promotion-gate.yml` (`Main Promotion Gate`)
    - Purpose: enforce stable-branch policy by allowing only `dev` -> `main` PR promotion authored by `willsarg` or `theonlyhennygod`

### Non-Blocking but Important

- `.github/workflows/pub-docker-img.yml` (`Docker`)
    - Purpose: PR Docker smoke check on `dev`/`main` PRs and publish images on tag pushes (`v*`) only
- `.github/workflows/sec-audit.yml` (`Security Audit`)
    - Purpose: dependency advisories (`rustsec/audit-check`, pinned SHA) and policy/license checks (`cargo deny`)
- `.github/workflows/sec-codeql.yml` (`CodeQL Analysis`)
    - Purpose: scheduled/manual static analysis for security findings
- `.github/workflows/sec-vorpal-reviewdog.yml` (`Sec Vorpal Reviewdog`)
    - Purpose: manual secure-coding feedback scan for supported non-Rust files (`.py`, `.js`, `.jsx`, `.ts`, `.tsx`) using reviewdog annotations
    - Noise control: excludes common test/fixture paths and test file patterns by default (`include_tests=false`)
- `.github/workflows/pub-release.yml` (`Release`)
    - Purpose: build release artifacts in verification mode (manual/scheduled) and publish GitHub releases on tag push or manual publish mode
- `.github/workflows/pub-homebrew-core.yml` (`Pub Homebrew Core`)
    - Purpose: manual, bot-owned Homebrew core formula bump PR flow for tagged releases
    - Guardrail: release tag must match `Cargo.toml` version
- `.github/workflows/pr-label-policy-check.yml` (`Label Policy Sanity`)
    - Purpose: validate shared contributor-tier policy in `.github/label-policy.json` and ensure label workflows consume that policy
- `.github/workflows/test-rust-build.yml` (`Rust Reusable Job`)
    - Purpose: reusable Rust setup/cache + command runner for workflow-call consumers

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
    - Additional behavior: risk + size labels are auto-corrected on manual PR label edits (`labeled`/`unlabeled` events); apply `risk: manual` when maintainers intentionally override automated risk selection
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

- `CI`: push to `dev` and `main`, PRs to `dev` and `main`
- `Docker`: tag push (`v*`) for publish, matching PRs to `dev`/`main` for smoke build, manual dispatch for smoke only
- `Release`: tag push (`v*`), weekly schedule (verification-only), manual dispatch (verification or publish)
- `Pub Homebrew Core`: manual dispatch only
- `Security Audit`: push to `dev` and `main`, PRs to `dev` and `main`, weekly schedule
- `Sec Vorpal Reviewdog`: manual dispatch only
- `Workflow Sanity`: PR/push when `.github/workflows/**`, `.github/*.yml`, or `.github/*.yaml` change
- `Main Promotion Gate`: PRs to `main` only; requires PR author `willsarg`/`theonlyhennygod` and head branch `dev` in the same repository
- `Dependabot`: all update PRs target `dev` (not `main`)
- `PR Intake Checks`: `pull_request_target` on opened/reopened/synchronize/edited/ready_for_review
- `Label Policy Sanity`: PR/push when `.github/label-policy.json`, `.github/workflows/pr-labeler.yml`, or `.github/workflows/pr-auto-response.yml` changes
- `PR Labeler`: `pull_request_target` lifecycle events
- `PR Auto Responder`: issue opened/labeled, `pull_request_target` opened/labeled
- `Stale PR Check`: daily schedule, manual dispatch
- `PR Hygiene`: every 12 hours schedule, manual dispatch

## Fast Triage Guide

1. `CI Required Gate` failing: start with `.github/workflows/ci-run.yml`.
2. Docker failures on PRs: inspect `.github/workflows/pub-docker-img.yml` `pr-smoke` job.
3. Release failures (tag/manual/scheduled): inspect `.github/workflows/pub-release.yml` and the `prepare` job outputs.
4. Homebrew formula publish failures: inspect `.github/workflows/pub-homebrew-core.yml` summary output and bot token/fork variables.
5. Security failures: inspect `.github/workflows/sec-audit.yml` and `deny.toml`.
6. Workflow syntax/lint failures: inspect `.github/workflows/workflow-sanity.yml`.
7. PR intake failures: inspect `.github/workflows/pr-intake-checks.yml` sticky comment and run logs.
8. Label policy parity failures: inspect `.github/workflows/pr-label-policy-check.yml`.
9. Docs failures in CI: inspect `docs-quality` job logs in `.github/workflows/ci-run.yml`.
10. Strict delta lint failures in CI: inspect `lint-strict-delta` job logs and compare with `BASE_SHA` diff scope.

## Maintenance Rules

- Keep merge-blocking checks deterministic and reproducible (`--locked` where applicable).
- Follow `docs/release-process.md` for verify-before-publish release cadence and tag discipline.
- Keep merge-blocking rust quality policy aligned across `.github/workflows/ci-run.yml`, `dev/ci.sh`, and `.githooks/pre-push` (`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`).
- Use `./scripts/ci/rust_strict_delta_gate.sh` (or `./dev/ci.sh lint-delta`) as the incremental strict merge gate for changed Rust lines.
- Run full strict lint audits regularly via `./scripts/ci/rust_quality_gate.sh --strict` (for example through `./dev/ci.sh lint-strict`) and track cleanup in focused PRs.
- Keep docs markdown gating incremental via `./scripts/ci/docs_quality_gate.sh` (block changed-line issues, report baseline issues separately).
- Keep docs link gating incremental via `./scripts/ci/collect_changed_links.py` + lychee (check only links added on changed lines).
- Prefer explicit workflow permissions (least privilege).
- Keep Actions source policy restricted to approved allowlist patterns (see `docs/actions-source-policy.md`).
- Use path filters for expensive workflows when practical.
- Keep docs quality checks low-noise (incremental markdown + incremental added-link checks).
- Keep dependency update volume controlled (grouping + PR limits).
- Avoid mixing onboarding/community automation with merge-gating logic.

## Automation Side-Effect Controls

- Prefer deterministic automation that can be manually overridden (`risk: manual`) when context is nuanced.
- Keep auto-response comments deduplicated to prevent triage noise.
- Keep auto-close behavior scoped to issues; maintainers own PR close/merge decisions.
- If automation is wrong, correct labels first, then continue review with explicit rationale.
- Use `superseded` / `stale-candidate` labels to prune duplicate or dormant PRs before deep review.
