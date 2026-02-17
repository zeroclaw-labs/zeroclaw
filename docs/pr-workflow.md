# ZeroClaw PR Workflow (High-Volume Collaboration)

This document defines how ZeroClaw handles high PR volume while maintaining:

- High performance
- High efficiency
- High stability
- High extensibility
- High sustainability
- High security

Related references:

- [`docs/ci-map.md`](ci-map.md) for per-workflow ownership, triggers, and triage flow.
- [`docs/reviewer-playbook.md`](reviewer-playbook.md) for day-to-day reviewer execution.

## 1) Governance Goals

1. Keep merge throughput predictable under heavy PR load.
2. Keep CI signal quality high (fast feedback, low false positives).
3. Keep security review explicit for risky surfaces.
4. Keep changes easy to reason about and easy to revert.
5. Keep repository artifacts free of personal/sensitive data leakage.

### Governance Design Logic (Control Loop)

This workflow is intentionally layered to reduce reviewer load while keeping accountability clear:

1. **Intake classification**: path/size/risk/module labels route the PR to the right review depth.
2. **Deterministic validation**: merge gate depends on reproducible checks, not subjective comments.
3. **Risk-based review depth**: high-risk paths trigger deep review; low-risk paths stay fast.
4. **Rollback-first merge contract**: every merge path includes concrete recovery steps.

Automation assists with triage and guardrails, but final merge accountability remains with human maintainers and PR authors.

## 2) Required Repository Settings

Maintain these branch protection rules on `main`:

- Require status checks before merge.
- Require check `CI Required Gate`.
- Require pull request reviews before merge.
- Require CODEOWNERS review for protected paths.
- Dismiss stale approvals when new commits are pushed.
- Restrict force-push on protected branches.

## 3) PR Lifecycle

### Step A: Intake

- Contributor opens PR with full `.github/pull_request_template.md`.
- `PR Labeler` applies scope/path labels + size labels + risk labels + module labels (for example `channel:telegram`, `provider:kimi`, `tool:shell`) and contributor tiers by merged PR count (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50), while de-duplicating less-specific scope labels when a more specific module label is present.
- For all module prefixes, module labels are compacted to reduce noise: one specific module keeps `prefix:component`, but multiple specifics collapse to the base scope label `prefix`.
- Label ordering is priority-first: `risk:*` -> `size:*` -> contributor tier -> module/path labels.
- Maintainers can run `PR Labeler` manually (`workflow_dispatch`) in `audit` mode for drift visibility or `repair` mode to normalize managed label metadata repository-wide.
- Hovering a label in GitHub shows its auto-managed description (rule/threshold summary).
- Managed label colors are arranged by display order to create a smooth gradient across long label rows.
- `Auto Response` posts first-time guidance, handles label-driven routing for low-signal items, and auto-applies issue contributor tiers using the same thresholds as `PR Labeler` (`trusted` >=5, `experienced` >=10, `principal` >=20, `distinguished` >=50).

### Step B: Validation

- `CI Required Gate` is the merge gate.
- Docs-only PRs use fast-path and skip heavy Rust jobs.
- Non-doc PRs must pass lint, tests, and release build smoke check.

### Step C: Review

- Reviewers prioritize by risk and size labels.
- Security-sensitive paths (`src/security`, `src/runtime`, `src/gateway`, and CI workflows) require maintainer attention.
- Large PRs (`size: L`/`size: XL`) should be split unless strongly justified.

### Step D: Merge

- Prefer **squash merge** to keep history compact.
- PR title should follow Conventional Commit style.
- Merge only when rollback path is documented.

## 4) PR Readiness Contracts (DoR / DoD)

### Definition of Ready (before requesting review)

- PR template fully completed.
- Scope boundary is explicit (what changed / what did not).
- Validation evidence attached (not just "CI will check").
- Security and rollback fields completed for risky paths.
- Privacy/data-hygiene checks are completed and test language is neutral/project-scoped.
- If identity-like wording appears in tests/examples, it is normalized to ZeroClaw/project-native labels.

### Definition of Done (merge-ready)

- `CI Required Gate` is green.
- Required reviewers approved (including CODEOWNERS paths).
- Risk class labels match touched paths.
- Migration/compatibility impact is documented.
- Rollback path is concrete and fast.

## 5) PR Size Policy

- `size: XS` <= 80 changed lines
- `size: S` <= 250 changed lines
- `size: M` <= 500 changed lines
- `size: L` <= 1000 changed lines
- `size: XL` > 1000 changed lines

Policy:

- Target `XS/S/M` by default.
- `L/XL` PRs need explicit justification and tighter test evidence.
- If a large feature is unavoidable, split into stacked PRs.

Automation behavior:

- `PR Labeler` applies `size:*` labels from effective changed lines.
- Docs-only/lockfile-heavy PRs are normalized to avoid size inflation.

## 6) AI/Agent Contribution Policy

AI-assisted PRs are welcome, and review can also be agent-assisted.

Required:

1. Clear PR summary with scope boundary.
2. Explicit test/validation evidence.
3. Security impact and rollback notes for risky changes.

Recommended:

1. Brief tool/workflow notes when automation materially influenced the change.
2. Optional prompt/plan snippets for reproducibility.

We do **not** require contributors to quantify AI-vs-human line ownership.

Review emphasis for AI-heavy PRs:

- Contract compatibility
- Security boundaries
- Error handling and fallback behavior
- Performance and memory regressions

## 7) Review SLA and Queue Discipline

- First maintainer triage target: within 48 hours.
- If PR is blocked, maintainer leaves one actionable checklist.
- `stale` automation is used to keep queue healthy; maintainers can apply `no-stale` when needed.
- `pr-hygiene` automation checks open PRs every 12 hours and posts a nudge when a PR has no new commits for 48+ hours and is either behind `main` or missing/failing `CI Required Gate` on the head commit.

Backlog pressure controls:

- Use a review queue budget: limit concurrent deep-review PRs per maintainer and keep the rest in triage state.
- For stacked work, require explicit `Depends on #...` so review order is deterministic.
- If a new PR replaces an older open PR, require `Supersedes #...` and close the older one after maintainer confirmation.
- Mark dormant/redundant PRs with `stale-candidate` or `superseded` to reduce duplicate review effort.

Issue triage discipline:

- `r:needs-repro` for incomplete bug reports (request deterministic repro before deep triage).
- `r:support` for usage/help items better handled outside bug backlog.
- `invalid` / `duplicate` labels trigger **issue-only** closing automation with guidance.

Automation side-effect guards:

- `Auto Response` deduplicates label-based comments to avoid spam.
- Automated close routes are limited to issues, not PRs.
- Maintainers can freeze automated risk recalculation with `risk: manual` when context demands human override.

## 8) Security and Stability Rules

Changes in these areas require stricter review and stronger test evidence:

- `src/security/**`
- runtime process management
- gateway ingress/authentication behavior (`src/gateway/**`)
- filesystem access boundaries
- network/authentication behavior
- GitHub workflows and release pipeline
- tools with execution capability (`src/tools/**`)

Minimum for risky PRs:

- threat/risk statement
- mitigation notes
- rollback steps

Recommended for high-risk PRs:

- include a focused test proving boundary behavior
- include one explicit failure-mode scenario and expected degradation

For agent-assisted contributions, reviewers should also verify the author demonstrates understanding of runtime behavior and blast radius.

## 9) Failure Recovery

If a merged PR causes regressions:

1. Revert PR immediately on `main`.
2. Open a follow-up issue with root-cause analysis.
3. Re-introduce fix only with regression tests.

Prefer fast restore of service quality over delayed perfect fixes.

## 10) Maintainer Checklist (Merge-Ready)

- Scope is focused and understandable.
- CI gate is green.
- Docs-quality checks are green when docs changed.
- Security impact fields are complete.
- Privacy/data-hygiene fields are complete and evidence is redacted/anonymized.
- Agent workflow notes are sufficient for reproducibility (if automation was used).
- Rollback plan is explicit.
- Commit title follows Conventional Commits.

## 11) Agent Review Operating Model

To keep review quality stable under high PR volume, we use a two-lane review model:

### Lane A: Fast triage (agent-friendly)

- Confirm PR template completeness.
- Confirm CI gate signal (`CI Required Gate`).
- Confirm risk class via labels and touched paths.
- Confirm rollback statement exists.
- Confirm privacy/data-hygiene section and neutral wording requirements are satisfied.
- Confirm any required identity-like wording uses ZeroClaw/project-native terminology.

### Lane B: Deep review (risk-based)

Required for high-risk changes (security/runtime/gateway/CI):

- Validate threat model assumptions.
- Validate failure mode and degradation behavior.
- Validate backward compatibility and migration impact.
- Validate observability/logging impact.

## 12) Queue Priority and Label Discipline

Triage order recommendation:

1. `size: XS`/`size: S` + bug/security fixes
2. `size: M` focused changes
3. `size: L`/`size: XL` split requests or staged review

Label discipline:

- Path labels identify subsystem ownership quickly.
- Size labels drive batching strategy.
- Risk labels drive review depth (`risk: low/medium/high`).
- Module labels (`<module>: <component>`) improve reviewer routing for integration-specific changes and future newly-added modules.
- `risk: manual` allows maintainers to preserve a human risk judgment when automation lacks context.
- `no-stale` is reserved for accepted-but-blocked work.

## 13) Agent Handoff Contract

When one agent hands off to another (or to a maintainer), include:

1. Scope boundary (what changed / what did not).
2. Validation evidence.
3. Open risks and unknowns.
4. Suggested next action.

This keeps context loss low and avoids repeated deep dives.
