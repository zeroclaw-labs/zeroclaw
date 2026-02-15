# ZeroClaw PR Workflow (High-Volume Collaboration)

This document defines how ZeroClaw handles high PR volume while maintaining:

- High performance
- High efficiency
- High stability
- High extensibility
- High sustainability
- High security

## 1) Governance Goals

1. Keep merge throughput predictable under heavy PR load.
2. Keep CI signal quality high (fast feedback, low false positives).
3. Keep security review explicit for risky surfaces.
4. Keep changes easy to reason about and easy to revert.

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
- `PR Labeler` applies path labels + size labels.
- `Auto Response` posts first-time contributor guidance.

### Step B: Validation

- `CI Required Gate` is the merge gate.
- Docs-only PRs use fast-path and skip heavy Rust jobs.
- Non-doc PRs must pass lint, tests, and release build smoke check.

### Step C: Review

- Reviewers prioritize by risk and size labels.
- Security-sensitive paths (`src/security`, runtime, CI) require maintainer attention.
- Large PRs (`size: L`/`size: XL`) should be split unless strongly justified.

### Step D: Merge

- Prefer **squash merge** to keep history compact.
- PR title should follow Conventional Commit style.
- Merge only when rollback path is documented.

## 4) PR Size Policy

- `size: XS` <= 80 changed lines
- `size: S` <= 250 changed lines
- `size: M` <= 500 changed lines
- `size: L` <= 1000 changed lines
- `size: XL` > 1000 changed lines

Policy:

- Target `XS/S/M` by default.
- `L/XL` PRs need explicit justification and tighter test evidence.
- If a large feature is unavoidable, split into stacked PRs.

## 5) AI/Agent Contribution Policy

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

## 6) Review SLA and Queue Discipline

- First maintainer triage target: within 48 hours.
- If PR is blocked, maintainer leaves one actionable checklist.
- `stale` automation is used to keep queue healthy; maintainers can apply `no-stale` when needed.

## 7) Security and Stability Rules

Changes in these areas require stricter review and stronger test evidence:

- `src/security/**`
- runtime process management
- filesystem access boundaries
- network/authentication behavior
- GitHub workflows and release pipeline

Minimum for risky PRs:

- threat/risk statement
- mitigation notes
- rollback steps

## 8) Failure Recovery

If a merged PR causes regressions:

1. Revert PR immediately on `main`.
2. Open a follow-up issue with root-cause analysis.
3. Re-introduce fix only with regression tests.

Prefer fast restore of service quality over delayed perfect fixes.

## 9) Maintainer Checklist (Merge-Ready)

- Scope is focused and understandable.
- CI gate is green.
- Security impact fields are complete.
- Agent workflow notes are sufficient for reproducibility (if automation was used).
- Rollback plan is explicit.
- Commit title follows Conventional Commits.

## 10) Agent Review Operating Model

To keep review quality stable under high PR volume, we use a two-lane review model:

### Lane A: Fast triage (agent-friendly)

- Confirm PR template completeness.
- Confirm CI gate signal (`CI Required Gate`).
- Confirm risk class via labels and touched paths.
- Confirm rollback statement exists.

### Lane B: Deep review (risk-based)

Required for high-risk changes (security/runtime/gateway/CI):

- Validate threat model assumptions.
- Validate failure mode and degradation behavior.
- Validate backward compatibility and migration impact.
- Validate observability/logging impact.

## 11) Queue Priority and Label Discipline

Triage order recommendation:

1. `size: XS`/`size: S` + bug/security fixes
2. `size: M` focused changes
3. `size: L`/`size: XL` split requests or staged review

Label discipline:

- Path labels identify subsystem ownership quickly.
- Size labels drive batching strategy.
- `no-stale` is reserved for accepted-but-blocked work.

## 12) Agent Handoff Contract

When one agent hands off to another (or to a maintainer), include:

1. Scope boundary (what changed / what did not).
2. Validation evidence.
3. Open risks and unknowns.
4. Suggested next action.

This keeps context loss low and avoids repeated deep dives.
