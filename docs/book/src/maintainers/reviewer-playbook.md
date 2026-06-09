# Reviewer Playbook

The operating model for reviewing PRs and triaging issues. Sized to keep review quality high under heavy volume; routes by risk so high-stakes paths get the attention they need without dragging every small change through the same gate.

For the actual fetch sequence and review verdict mechanics, see [PR Review Protocol](../contributing/pr-review-protocol.md). This page is the *operating model*; the protocol is the *procedure*.

## Fast paths

Use this section to route a review before reading deeper. Each row links to the section that elaborates.

Use [PR lanes](./pr-workflow.md#pr-lanes) for routing expectations; use this playbook's risk matrix for review depth.

| Situation | Action | Section |
|---|---|---|
| Intake fails in the first 5 minutes | Leave one actionable checklist comment, stop deep review | [Five-minute intake](#five-minute-intake) |
| Risk is high or unclear | Treat as `risk: high` until proven otherwise | [Review depth matrix](#review-depth-matrix) |
| Automation output is wrong or noisy | Apply the override protocol | [Automation override](#automation-override) |
| Need to hand off to another maintainer | Use the handoff template | [Handoff](#handoff) |

## Review depth matrix

| Risk label | Typical paths | Minimum depth | Required evidence |
|---|---|---|---|
| `risk: low` | Docs, tests, chore, isolated non-runtime | 1 reviewer + CI gate | Coherent local validation, no behavior ambiguity |
| `risk: medium` | `crates/zeroclaw-providers/`, `crates/zeroclaw-channels/`, `crates/zeroclaw-memory/`, `crates/zeroclaw-config/` | 1 subsystem-aware reviewer + behavior verification | Focused scenario proof, explicit side effects |
| `risk: high` | The [canonical high-risk path set](./labels.md#risk-labels) (runtime, gateway, tools, security, `.github/workflows/`) | Fast triage + deep review + rollback readiness | Security and failure-mode checks, rollback clarity |

When uncertain, treat as higher risk.

Risk labels are currently manual. If future risk automation is restored, follow the [labels automation contract](./labels.md#automation-contract): apply `risk: manual` when a maintainer correction should not be overwritten on the next pushed update.

Labels are maintainer metadata. If the correct label is obvious and you have permission, fix it yourself before finalizing the review. Ask the author only when the right label choice is ambiguous or nobody with label permissions is available.

## Standard workflow

### Five-minute intake

For every new PR, before reading any code:

1. Confirm the PR template is complete: summary, validation evidence, security & privacy, compatibility, rollback (for medium/high).
2. Confirm labels are present and plausible: `size:*`, `risk:*`, scope labels, contributor tier where applicable.
3. Confirm `CI Required Gate` signal status.
4. Confirm scope is one concern. Mixed-feature mega-PRs go back for a split unless the mix is explicitly justified.
5. Confirm privacy / data-hygiene rules. See [Privacy](../contributing/privacy.md) for the full rulebook.

If any intake check fails, leave one actionable checklist comment and stop. Don't deep-review a PR that hasn't passed intake: the back-and-forth is cheaper at this layer than after the diff has been reasoned about.

### Fast-lane checklist (every PR)

- Scope boundary is explicit and believable.
- Validation commands are present and the results are coherent.
- User-facing behavior changes are documented.
- Author demonstrates understanding of behavior and blast radius (especially for AI-assisted PRs).
- Rollback path is concrete; "revert" is not concrete.
- Compatibility and migration impact is clear.
- No personal or sensitive data leaked into diff artifacts; tests use neutral, project-scoped placeholders.
- Naming and architecture boundaries follow project contracts (`AGENTS.md`, [Extension examples](../developing/extension-examples.md)).

### Deep-review checklist (high-risk only)

For `risk: high` PRs, verify a concrete example in each category. One concrete instance beats five generic claims.

- **Security boundaries**: deny-by-default behavior preserved, no accidental scope broadening.
- **Failure modes**: error handling explicit, degrades safely.
- **Contract stability**: CLI, config, or API compatibility preserved or migration documented.
- **Observability**: failures diagnosable without leaking secrets.
- **Rollback safety**: revert path and blast radius clear.

### Comment shape

Prefer checklist-style comments with one explicit outcome:

- **Ready to merge** (say why).
- **Needs author action** (ordered blocker list).
- **Needs deeper security or runtime review** (state the exact risk and the requested evidence).

Vague comments create avoidable round trips. If you find yourself writing "this might be a problem", invest 30 more seconds and turn it into a specific scenario or pull the comment.

## Issue triage

The same risk-routing principle applies to issues, but the labels and signals are different.

Issue `risk:*` labels describe likely fix blast radius from the report. PR `risk:*` labels describe the actual diff under review. Reassess risk when an issue becomes a PR instead of carrying the issue label forward automatically.

### Triage labels

| Label | When to use |
|---|---|
| `r:needs-repro` | Bug report missing a deterministic repro. Block deeper triage on this. |
| `r:support` | Usage or help question better routed outside the bug backlog. |
| `status:accepted` | The team has accepted the RFC or work item. Add `status:no-stale` only when the issue also needs stale protection. |
| `status:blocked` | Valid work is waiting on an external dependency, maintainer decision, or linked prerequisite. Record the blocker; this is stale protection only while that blocker remains unresolved. |
| `status:in-progress` | An open PR is actively targeting the issue. Re-check live PR state before relying on it during stale passes. |
| `status:no-stale` | Accepted or otherwise long-lived work should stay open and is not already protected by another stale exclusion. Record the reason and active owner or steward path using the contributor-visible owner sources in the [Project board contract](./pr-workflow.md#issue-ownership-path). |
| `good first issue` | XS/S, self-contained, documented work with clear acceptance criteria, relevant code or docs links, a named mentor or contact, and low onboarding risk. |
| `help wanted` | Actionable, unblocked work maintainers want external help on and can review. Do not use it as a generic valid/unowned marker. |

Assignee means active work. Area steward means responsibility for the next issue-routing decision when no implementer is active. The [Project board contract](./pr-workflow.md#issue-ownership-path) defines the accepted owner sources and routing outcomes. Labels can identify the likely area, but labels alone are not an owner source.

### Resolution labels

Use resolution labels only when closing or removing an item from the active queue. They explain the terminal outcome; they do not replace `status:*` lifecycle labels on work that should stay open. The [labels guide](./labels.md#resolution-labels) is the source of truth for current resolution-label definitions and migration holdbacks.

For duplicates, link the canonical target before closing or redirecting discussion. For invalid reports, explain what makes the report unactionable or where it should go instead. For work we are explicitly choosing not to pursue, use the board-level `Won't Do` / live `wontfix` path and leave a brief rationale.

For replaced PRs or issue paths, use [Superseding PRs](./superseding.md) and preserve contributor attribution when relevant.

If logs or payloads in the report contain personal identifiers or sensitive data, request redaction before deeper triage. The triage process must not propagate the exposure.

### PR backlog pruning

When review demand exceeds capacity:

1. Keep active bug and security PRs (`size: XS/S`) at the top of the queue.
2. Ask overlapping PRs to consolidate; close older ones with a superseded or replaced rationale after the author acknowledges. See [Superseding PRs](./superseding.md) for the attribution rules.
3. Mark dormant PRs as `stale-candidate` before stale closure window starts.
4. Require rebase + fresh validation evidence before reopening anything that's been stale-closed.

## Automation override

Use this when automation output creates review side effects:

1. **Incorrect risk label**: set the intended `risk:*` label. If future risk automation is active, also follow the [labels automation contract](./labels.md#automation-contract) for `risk: manual`.
2. **Incorrect auto-close on issue triage**: reopen, remove the route label, leave one clarifying comment.
3. **Label spam or noise**: keep one canonical maintainer comment, remove redundant route labels.
4. **Ambiguous PR scope**: request a split before deep review; don't try to review across two concerns at once.

## Handoff

When passing review to another maintainer or agent mid-flight, include:

1. **Scope summary.**
2. **Current risk class and rationale.**
3. **What you've validated.**
4. **Open blockers.**
5. **Suggested next action.**

This keeps context loss low and avoids the next reviewer redoing the same fetches you already did.

## Weekly queue hygiene

- Walk the stale queue. Apply `status:no-stale` only under the rules in the [Project board contract](./pr-workflow.md#issue-ownership-path); treat existing `status:no-stale` issues missing a recorded reason or owner path as audit findings rather than automatic stale candidates.
- Prioritize `size: XS/S` bug and security PRs first.
- Convert recurring support questions into docs improvements and auto-response guidance.

The goal is a queue where every open PR is either being actively reviewed, blocked on the author, or blocked on something external, never just sitting because nobody got to it.
