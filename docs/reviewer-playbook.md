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
2. Confirm labels (`size:*`, `risk:*`, `area:*`) are present and plausible.
3. Confirm CI signal status (`CI Required Gate`).
4. Confirm scope is one concern (reject mixed mega-PRs unless justified).

If any intake requirement fails, leave one actionable checklist comment instead of deep review.

## 3) Risk-to-Depth Matrix

| Risk label | Typical touched paths | Minimum review depth |
|---|---|---|
| `risk: low` | docs/tests/chore, isolated non-runtime changes | 1 reviewer + CI gate |
| `risk: medium` | `src/providers/**`, `src/channels/**`, `src/memory/**`, `src/config/**` | 1 subsystem-aware reviewer + behavior verification |
| `risk: high` | `src/security/**`, `src/runtime/**`, `src/tools/**`, `.github/workflows/**` | fast triage + deep review, strong rollback and failure-mode checks |

When uncertain, treat as `risk: high`.

## 4) Fast-Lane Checklist (All PRs)

- Scope boundary is explicit and believable.
- Validation commands are present and results are coherent.
- User-facing behavior changes are documented.
- Rollback path is concrete (not just “revert”).
- Compatibility/migration impacts are clear.

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

## 7) Review Comment Style

Prefer checklist-style comments with one of these outcomes:

- **Ready to merge** (explicitly say why).
- **Needs author action** (ordered list of blockers).
- **Needs deeper security/runtime review** (state exact risk and requested evidence).

Avoid vague comments that create back-and-forth latency.

## 8) Handoff Protocol

If handing off review to another maintainer/agent, include:

1. Scope summary
2. Current risk class and why
3. What has been validated already
4. Open blockers
5. Suggested next action

## 9) Weekly Queue Hygiene

- Review stale queue and apply `no-stale` only to accepted-but-blocked work.
- Prioritize `size: XS/S` bug/security PRs first.
- Convert recurring support issues into docs updates and auto-response guidance.
