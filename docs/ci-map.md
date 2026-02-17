# CI Workflow Map

This document explains what each GitHub workflow does, when it runs, and whether it should block merges.

## Merge-Blocking vs Optional

Merge-blocking checks should stay small and deterministic. Optional checks are useful for automation and maintenance, but should not block normal development.

### Merge-Blocking

- `.github/workflows/ci.yml` (`CI`)
    - Purpose: Rust validation (`cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D clippy::correctness`, strict delta lint gate on changed Rust lines, `test`, release build smoke) + docs quality checks when docs change (`markdownlint` blocks only issues on changed lines; link check scans only links added on changed lines)
    - Additional behavior: PRs that change `.github/workflows/**` require at least one approving review from a login in `WORKFLOW_OWNER_LOGINS` (repository variable fallback: `theonlyhennygod,willsarg`)
    - Additional behavior: lint gates run before `test`/`build`; when lint/docs gates fail on PRs, CI posts an actionable feedback comment with failing gate names and local fix commands
    - Merge gate: `CI Required Gate`
- `.github/workflows/workflow-sanity.yml` (`Workflow Sanity`)
    - Purpose: lint GitHub workflow files (`actionlint`, tab checks)
    - Recommended for workflow-changing PRs

### Non-Blocking but Important

- `.github/workflows/docker.yml` (`Docker`)
    - Purpose: PR docker smoke check and publish images on `main`/tag pushes
- `.github/workflows/security.yml` (`Security Audit`)
    - Purpose: dependency advisories (`cargo audit`) and policy/license checks (`cargo deny`)
- `.github/workflows/release.yml` (`Release`)
    - Purpose: build tagged release artifacts and publish GitHub releases

### Optional Repository Automation

- `.github/workflows/labeler.yml` (`PR Labeler`)
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
- `.github/workflows/auto-response.yml` (`PR Auto Responder`)
    - Purpose: first-time contributor onboarding + label-driven response routing (`r:support`, `r:needs-repro`, etc.)
    - Additional behavior: applies contributor tiers on issues by merged PR count (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), matching PR tier thresholds exactly
    - Additional behavior: contributor-tier labels are treated as automation-managed (manual add/remove on PR/issue is auto-corrected)
    - Guardrail: label-based close routes are issue-only; PRs are never auto-closed by route labels
- `.github/workflows/stale.yml` (`Stale`)
    - Purpose: stale issue/PR lifecycle automation
- `.github/dependabot.yml` (`Dependabot`)
    - Purpose: grouped, rate-limited dependency update PRs (Cargo + GitHub Actions)
- `.github/workflows/pr-hygiene.yml` (`PR Hygiene`)
    - Purpose: nudge stale-but-active PRs to rebase/re-run required checks before queue starvation

## Trigger Map

- `CI`: push to `main`, PRs to `main`
- `Docker`: push to `main`, tag push (`v*`), PRs touching docker/workflow files, manual dispatch
- `Release`: tag push (`v*`)
- `Security Audit`: push to `main`, PRs to `main`, weekly schedule
- `Workflow Sanity`: PR/push when `.github/workflows/**`, `.github/*.yml`, or `.github/*.yaml` change
- `PR Labeler`: `pull_request_target` lifecycle events
- `PR Auto Responder`: issue opened/labeled, `pull_request_target` opened/labeled
- `Stale`: daily schedule, manual dispatch
- `Dependabot`: weekly dependency maintenance windows
- `PR Hygiene`: every 12 hours schedule, manual dispatch

## Fast Triage Guide

1. `CI Required Gate` failing: start with `.github/workflows/ci.yml`.
2. Docker failures on PRs: inspect `.github/workflows/docker.yml` `pr-smoke` job.
3. Release failures on tags: inspect `.github/workflows/release.yml`.
4. Security failures: inspect `.github/workflows/security.yml` and `deny.toml`.
5. Workflow syntax/lint failures: inspect `.github/workflows/workflow-sanity.yml`.
6. Docs failures in CI: inspect `docs-quality` job logs in `.github/workflows/ci.yml`.
7. Strict delta lint failures in CI: inspect `lint-strict-delta` job logs and compare with `BASE_SHA` diff scope.

## Maintenance Rules

- Keep merge-blocking checks deterministic and reproducible (`--locked` where applicable).
- Keep merge-blocking rust quality policy aligned across `.github/workflows/ci.yml`, `dev/ci.sh`, and `.githooks/pre-push` (`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`).
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
