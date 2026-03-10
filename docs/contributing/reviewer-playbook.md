# Reviewer Playbook

This playbook is the operational companion to [`docs/pr-workflow.md`](./pr-workflow.md).
For broader documentation navigation, use [`docs/README.md`](./README.md).

## 0. Summary

- **Purpose:** define a deterministic reviewer operating model that keeps review quality high under heavy PR volume.
- **Audience:** maintainers, reviewers, and agent-assisted reviewers.
- **Scope:** intake triage, risk-to-depth routing, deep-review checks, automation overrides, and handoff protocol.
- **Non-goals:** replacing PR policy authority in `CONTRIBUTING.md` or workflow authority in CI files.

---

## 1. Fast Path by Review Situation

Use this section to route quickly before reading full detail.

### 1.1 Intake fails in first 5 minutes

1. Leave one actionable checklist comment.
2. Stop deep review until intake blockers are fixed.

Go to:

- [Section 3.1](#31-five-minute-intake-triage)

### 1.2 Risk is high or unclear

1. Treat as `risk: high` by default.
2. Require deep review and explicit rollback evidence.

Go to:

- [Section 2](#2-review-depth-decision-matrix)
- [Section 3.3](#33-deep-review-checklist-high-risk)

### 1.3 Automation output is wrong/noisy

1. Apply override protocol (`risk: manual`, dedupe comments/labels).
2. Continue review with explicit rationale.

Go to:

- [Section 5](#5-automation-override-protocol)

### 1.4 Need review handoff

1. Handoff with scope/risk/validation/blockers.
2. Assign concrete next action.

Go to:

- [Section 6](#6-handoff-protocol)

---

## 2. Review Depth Decision Matrix

| Risk label | Typical touched paths | Minimum review depth | Required evidence |
|---|---|---|---|
| `risk: low` | docs/tests/chore, isolated non-runtime changes | 1 reviewer + CI gate | coherent local validation + no behavior ambiguity |
| `risk: medium` | `src/providers/**`, `src/channels/**`, `src/memory/**`, `src/config/**` | 1 subsystem-aware reviewer + behavior verification | focused scenario proof + explicit side effects |
| `risk: high` | `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**` | fast triage + deep review + rollback readiness | security/failure-mode checks + rollback clarity |

When uncertain, treat as `risk: high`.

If automated risk labeling is contextually wrong, maintainers can apply `risk: manual` and set the final `risk:*` label explicitly.

---

## 3. Standard Review Workflow

### 3.1 Five-minute intake triage

For every new PR:

1. Confirm template completeness (`summary`, `validation`, `security`, `rollback`).
2. Confirm labels are present and plausible:
   - `size:*`, `risk:*`
   - scope labels (for example `provider`, `channel`, `security`)
   - module-scoped labels (`channel:*`, `provider:*`, `tool:*`)
   - contributor tier labels when applicable
3. Confirm CI signal status (`CI Required Gate`).
4. Confirm scope is one concern (reject mixed mega-PRs unless justified).
5. Confirm privacy/data-hygiene and neutral test wording requirements are satisfied.

If any intake requirement fails, leave one actionable checklist comment instead of deep review.

### 3.2 Fast-lane checklist (all PRs)

- Scope boundary is explicit and believable.
- Validation commands are present and results are coherent.
- User-facing behavior changes are documented.
- Author demonstrates understanding of behavior and blast radius (especially for agent-assisted PRs).
- Rollback path is concrete (not just “revert”).
- Compatibility/migration impacts are clear.
- No personal/sensitive data leakage in diff artifacts; examples/tests remain neutral and project-scoped.
- If identity-like wording exists, it uses ZeroClaw/project-native roles (not personal or real-world identities).
- Naming and architecture boundaries follow project contracts (`AGENTS.md`, `CONTRIBUTING.md`).

### 3.3 Deep review checklist (high risk)

For high-risk PRs, verify at least one concrete example in each category:

- **Security boundaries:** deny-by-default behavior preserved, no accidental scope broadening.
- **Failure modes:** error handling is explicit and degrades safely.
- **Contract stability:** CLI/config/API compatibility preserved or migration documented.
- **Observability:** failures are diagnosable without leaking secrets.
- **Rollback safety:** revert path and blast radius are clear.

### 3.4 Review comment outcome style

Prefer checklist-style comments with one explicit outcome:

- **Ready to merge** (say why).
- **Needs author action** (ordered blocker list).
- **Needs deeper security/runtime review** (state exact risk and requested evidence).

Avoid vague comments that create avoidable back-and-forth latency.

---

## 4. Issue Triage and Backlog Governance

### 4.1 Issue triage label playbook

Use labels to keep backlog actionable:

- `r:needs-repro` for incomplete bug reports.
- `r:support` for usage/support questions better routed outside bug backlog.
- `duplicate` / `invalid` for non-actionable duplicates/noise.
- `no-stale` for accepted work waiting on external blockers.
- Request redaction when logs/payloads include personal identifiers or sensitive data.

### 4.2 PR backlog pruning protocol

When review demand exceeds capacity, apply this order:

1. Keep active bug/security PRs (`size: XS/S`) at the top of queue.
2. Ask overlapping PRs to consolidate; close older ones as `superseded` after acknowledgement.
3. Mark dormant PRs as `stale-candidate` before stale closure window starts.
4. Require rebase + fresh validation before reopening stale/superseded technical work.

---

## 5. Automation Override Protocol

Use this when automation output creates review side effects:

1. **Incorrect risk label:** add `risk: manual`, then set intended `risk:*` label.
2. **Incorrect auto-close on issue triage:** reopen issue, remove route label, leave one clarifying comment.
3. **Label spam/noise:** keep one canonical maintainer comment and remove redundant route labels.
4. **Ambiguous PR scope:** request split before deep review.

---

## 6. Handoff Protocol

If handing off review to another maintainer/agent, include:

1. Scope summary.
2. Current risk class and rationale.
3. What has been validated already.
4. Open blockers.
5. Suggested next action.

---

## 7. Weekly Queue Hygiene

- Review stale queue and apply `no-stale` only to accepted-but-blocked work.
- Prioritize `size: XS/S` bug/security PRs first.
- Convert recurring support issues into docs updates and auto-response guidance.

---

## 8. Related Docs

- [README.md](./README.md) — documentation taxonomy and navigation.
- [pr-workflow.md](./pr-workflow.md) — governance workflow and merge contract.
- [ci-map.md](./ci-map.md) — CI ownership and triage map.
- [actions-source-policy.md](./actions-source-policy.md) — action source allowlist policy.

---

## 9. Maintenance Notes

- **Owner:** maintainers responsible for review quality and queue throughput.
- **Update trigger:** PR policy changes, risk-routing model changes, or automation override behavior changes.
- **Last reviewed:** 2026-02-18.
