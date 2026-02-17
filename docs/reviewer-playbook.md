# Reviewer Playbook

This playbook is the operational companion to [`docs/pr-workflow.md`](pr-workflow.md).
Use it to reduce review latency without reducing quality.

## 1) Review Objectives

- Keep queue throughput predictable.
- Keep risk review proportionate to change risk.
- Keep merge decisions reproducible and auditable.

## 2) 5-Minute Intake Triage

For every new PR, do a fast intake pass:

1. Confirm template completeness (`summary`, `validation`, `security`, `rollback`).
2. Confirm labels (`size:*`, `risk:*`, scope labels such as `provider`/`channel`/`security`, module-scoped labels such as `channel: *`/`provider: *`/`tool: *`, and contributor tier labels when applicable) are present and plausible.
3. Confirm CI signal status (`CI Required Gate`).
4. Confirm scope is one concern (reject mixed mega-PRs unless justified).
5. Confirm privacy/data-hygiene and neutral test wording requirements are satisfied.

If any intake requirement fails, leave one actionable checklist comment instead of deep review.

## 3) Risk-to-Depth Matrix

| Risk label | Typical touched paths | Minimum review depth |
|---|---|---|
| `risk: low` | docs/tests/chore, isolated non-runtime changes | 1 reviewer + CI gate |
| `risk: medium` | `src/providers/**`, `src/channels/**`, `src/memory/**`, `src/config/**` | 1 subsystem-aware reviewer + behavior verification |
| `risk: high` | `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**` | fast triage + deep review, strong rollback and failure-mode checks |

When uncertain, treat as `risk: high`.

If automated risk labeling is contextually wrong, maintainers can apply `risk: manual` and set the final risk label explicitly.

## 4) Fast-Lane Checklist (All PRs)

- Scope boundary is explicit and believable.
- Validation commands are present and results are coherent.
- User-facing behavior changes are documented.
- Author demonstrates understanding of behavior and blast radius (especially for agent-assisted PRs).
- Rollback path is concrete (not just “revert”).
- Compatibility/migration impacts are clear.
- No personal/sensitive data leakage in diff artifacts; examples/tests remain neutral and project-scoped.
- If identity-like wording exists, it uses ZeroClaw/project-native roles (not personal or real-world identities).
- Naming and architecture boundaries follow project contracts (`AGENTS.md`, `CONTRIBUTING.md`).

## 5) Deep Review Checklist (High Risk)

For high-risk PRs, verify at least one example in each category:

- **Security boundaries**: deny-by-default behavior preserved, no accidental scope broadening.
- **Failure modes**: error handling is explicit and degrades safely.
- **Contract stability**: CLI/config/API compatibility preserved or migration documented.
- **Observability**: failures are diagnosable without leaking secrets.
- **Rollback safety**: revert path and blast radius are clear.

## 6) Issue Triage Playbook

Use labels to keep backlog actionable:

- `r:needs-repro` for incomplete bug reports.
- `r:support` for usage/support questions better routed outside bug backlog.
- `duplicate` / `invalid` for non-actionable duplicates/noise.
- `no-stale` for accepted work waiting on external blockers.
- Request redaction if logs/payloads include personal identifiers or sensitive data.

## 7) Review Comment Style

Prefer checklist-style comments with one of these outcomes:

- **Ready to merge** (explicitly say why).
- **Needs author action** (ordered list of blockers).
- **Needs deeper security/runtime review** (state exact risk and requested evidence).

Avoid vague comments that create back-and-forth latency.

## 8) Automation Override Protocol

Use this when automation output creates review side effects:

1. **Incorrect risk label**: add `risk: manual`, then set the intended `risk:*` label.
2. **Incorrect auto-close on issue triage**: reopen issue, remove route label, and leave one clarifying comment.
3. **Label spam/noise**: keep one canonical maintainer comment and remove redundant route labels.
4. **Ambiguous PR scope**: request split before deep review.

### PR Backlog Pruning Protocol

When review demand exceeds capacity, apply this order:

1. Keep active bug/security PRs (`size: XS/S`) at the top of queue.
2. Ask overlapping PRs to consolidate; close older ones as `superseded` after acknowledgement.
3. Mark dormant PRs as `stale-candidate` before stale closure window starts.
4. Require rebase + fresh validation before reopening stale/superseded technical work.

## 9) Handoff Protocol

If handing off review to another maintainer/agent, include:

1. Scope summary
2. Current risk class and why
3. What has been validated already
4. Open blockers
5. Suggested next action

## 10) Weekly Queue Hygiene

- Review stale queue and apply `no-stale` only to accepted-but-blocked work.
- Prioritize `size: XS/S` bug/security PRs first.
- Convert recurring support issues into docs updates and auto-response guidance.
