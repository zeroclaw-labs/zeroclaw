# Issue Stewardship

Issue stewardship is the process for deciding who owns the next movement on an issue when nobody is actively implementing it.

This page is the source of truth for issue stewardship assignment. The [Project board contract](./pr-workflow.md#issue-ownership-path), [Labels](./labels.md), and [Reviewer playbook](./reviewer-playbook.md) link here rather than maintaining separate steward maps.

## Why this exists

PRs have CODEOWNERS, requested reviewers, checks, and merge state. Issues need a parallel routing path so accepted work does not sit indefinitely without a visible next decision.

Stewardship answers one question: who is responsible for deciding the next routing step?

It does not mean the steward must implement the issue, guarantee staffing, or personally review every related PR.

## Owner sources

Use the narrowest visible owner source that honestly describes the current state:

| Source | Use when | Expires when |
|---|---|---|
| Assignee | Someone is actively implementing, investigating, or shepherding the immediate issue. | They stop doing that work, the linked PR closes, or the issue moves back to triage. |
| Issue-visible maintainer comment or body section | A maintainer records the steward path, decision needed, or next triage date on the issue. | The decision is made, the date passes, or the issue state changes. |
| Public Project field | A public issue field or Project field names the active owner, steward path, or routing lane. | The field no longer matches live issue state. |
| Linked public tracker | A release tracker, RFC, roadmap issue, or implementation tracker owns the current decision surface. | The tracker closes, drifts from live state, or stops representing an active decision. |
| Standing steward map | The team has confirmed a current area steward for the relevant surface. | The steward steps back, the area map changes, or the entry's recorded review date passes. |

Labels alone are not owner sources. They identify likely areas, but they do not prove that someone is currently responsible for routing the issue.

## Standing steward map

No standing area steward map is active yet.

Until current maintainers confirm a map, do not treat labels such as `channel:*`, `provider:*`, `tool:*`, `gateway`, `runtime`, `security:*`, or `docs` as owner evidence by themselves. Route those issues through explicit owner sources instead: assignee, issue-visible comment, public Project field, linked tracker, or a dated maintainer-triage plan.

When the team is ready to add a standing map, record it here with:

- area or label family;
- steward handle or team;
- fallback when that steward is unavailable;
- what decisions the steward owns;
- what decisions still require broader maintainer review;
- last confirmed date.
- next review date or event.

Do not add someone as a standing steward without their agreement. When a steward steps away, remove or replace that entry before using it for `status:no-stale`, maintainer triage, or board-routing decisions.

## Assignment process

When triaging an issue:

1. Decide whether the issue is actionable, blocked, accepted, support-only, duplicate, invalid, or not planned.
2. If someone is actively working it, assign them and use `status:in-progress` only while that active path exists.
3. If no implementer is active, record the next routing owner source before adding durable protection such as `status:no-stale`.
4. If the issue belongs to an active release tracker, RFC, design tracker, or roadmap tracker, link that tracker and use it as the steward surface while it remains active.
5. If the issue needs maintainer triage but has no steward yet, record what decision is needed, where it will be decided, and when it should be revisited.
6. If no owner source exists, keep the issue open if it is still useful, but do not use `status:no-stale` as a permanent shield.

## Stale exemption rule

`status:no-stale` requires both:

- a reason the issue should stay open despite age; and
- a contributor-visible owner source from this page.

Active release trackers and active RFC or design trackers may supply both facts through the tracker itself. Ordinary accepted issues need a more specific owner source unless another stale exclusion already applies.

Revisit a stale exemption when:

- the assignee stops active work;
- a linked PR merges, closes, or becomes inactive;
- a milestone or release tracker closes;
- an RFC is decided, superseded, or closed;
- the issue no longer matches its tracker;
- the recorded revisit date passes;
- the documented steward steps away.

## Maintainer-triage fallback

Maintainer triage is a temporary routing state, not ownership by another name.

Use it only when the issue records:

- the decision needed;
- the maintainer or meeting surface expected to make the decision;
- the revisit date or event;
- the possible outcomes.

After that triage pass, replace the fallback with an active assignee, explicit steward path, linked tracker, blocked/deferred state, or closure rationale.
