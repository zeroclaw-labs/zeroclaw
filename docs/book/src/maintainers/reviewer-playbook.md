# Reviewer Playbook

The operating model for reviewing PRs and triaging issues. Sized to keep review quality high under heavy volume; routes by risk so high-stakes paths get the attention they need without dragging every small change through the same gate.

For the actual fetch sequence and review verdict mechanics, see [PR Review Protocol](../contributing/pr-review-protocol.md). This page is the *operating model*; the protocol is the *procedure*.

## Fast paths

Use this section to route a review before reading deeper. Each row links to the section that elaborates.

Use [PR lanes](./pr-workflow.md#pr-lanes) for routing expectations; use this playbook's risk matrix for review depth.

| Situation | Action | Section |
|---|---|---|
| Intake fails in the first 5 minutes | Leave one actionable checklist comment, stop deep review | [Five-minute intake](#five-minute-intake) |
| Risk is high or unclear | Treat as `risk:high` until proven otherwise | [Review depth matrix](#review-depth-matrix) |
| Diff adds a parallel interpretive surface | Verify it is derived from, or explicitly points to, the canonical source | [Drift-surface review](#drift-surface-review) |
| Automation output is wrong or noisy | Apply the override protocol | [Automation override](#automation-override) |
| Need to hand off to another maintainer | Use the handoff template | [Handoff](#handoff) |

## Review depth matrix

| Risk label | Typical paths | Minimum depth | Required evidence |
|---|---|---|---|
| `risk:low` | Docs, tests, chore, isolated non-runtime | 1 reviewer + CI gate | Coherent validation evidence, no behavior ambiguity |
| `risk:medium` | `crates/zeroclaw-providers/`, `crates/zeroclaw-channels/`, `crates/zeroclaw-memory/`, `crates/zeroclaw-config/` | 1 subsystem-aware reviewer + behavior verification | Focused scenario proof, explicit side effects |
| `risk:high` | The [canonical high-risk path set](./labels.md#risk-labels) (runtime, gateway, tools, security, `.github/workflows/`) | Fast triage + deep review + rollback readiness | Security and failure-mode checks, rollback clarity |

When uncertain, treat as higher risk.

Risk labels are currently manual. If future risk automation is restored, follow the [labels automation contract](./labels.md#automation-contract): apply `risk:manual` when a maintainer correction should not be overwritten on the next pushed update.

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
- Behavior changes are checked against the controlling contract: architecture docs, source-of-truth modules, trait boundaries, existing tests, public API shape, source comments, or explicit maintainer decisions.
- PR-body provenance is true. Cited RFCs, audits, issues, PRs, paths, generated artifacts, or follow-up findings exist and support the claim.
- Validation evidence names the checks being relied on and why they cover the changed behavior.
- Directly user-observable claims identify the user boundary and provide the smallest credible evidence that reaches it; use [User-boundary proof](../contributing/user-boundary-proof.md) when unit, mocked, compile, or generic CI evidence stops short.
- Duplicate local Cargo is not required when fresh required CI covers the same head, target, and feature set. Ask for extra validation only when it maps to a named gap in the required gate, such as macOS/Windows tests, cross-platform Clippy, desktop coverage, release target builds, stale CI, or unavailable CI.
- User-facing behavior changes are documented.
- Author demonstrates understanding of behavior and blast radius (especially for AI-assisted PRs).
- Rollback path is concrete; "revert" is not concrete.
- Compatibility and migration impact is clear.
- MSRV, pinned toolchain, or other version-floor changes are called out as compatibility-impacting: the PR explains who must upgrade, CI and installer baselines agree, and release notes name the new floor when the change can affect source-build users.
- No personal or sensitive data leaked into diff artifacts; tests use neutral, project-scoped placeholders.
- Naming and architecture boundaries follow project contracts (`AGENTS.md`, [Architecture overview](../architecture/overview.md#core-traits)).

### Drift-surface review

Treat new duplicate interpretive surfaces as review risk. A PR should not add
comments, examples, generated snapshots, mapping tables, configuration mirrors,
or parallel registries that restate behavior already owned by code, schemas,
tests, WIT, config, or runtime dispatch unless the new surface is mechanically
derived from that owner or clearly points back to it.

Block or request changes when the new surface can drift and future readers,
reviewers, or automation might treat it as more authoritative than the source.
Common examples include comments that describe behavior not enforced by code,
docs that duplicate an enum or schema list by hand, tests that snapshot an
implementation detail rather than user-observable behavior, and registries that
copy a key space already owned by another module.

Prefer one of these resolutions:

- Remove the duplicate surface and make the canonical owner easier to read.
- Generate the secondary surface from the canonical owner.
- Replace the restatement with a source pointer plus the reason that the
  pointer belongs there.

`why` comments are still welcome when they capture non-obvious invariants,
hazards, or tradeoffs that the type system and tests cannot express. They
should explain intent, not restate the nearby control flow or become a second
contract.

#### Typed dispatch for shared key spaces

For shared key spaces such as wire method names, compiled channel type keys,
provider slots, or frontend/backend registry keys, apply this rule by resolving
raw strings at the API or config boundary. Downstream code should dispatch
through an enum, macro-generated table, trait/factory registry, or another
canonical owner. Do not add parallel string `match` arms, hand-typed dispatch
tables, or duplicate lists that must be kept in sync by reviewer memory.

This does not ban string constants at API boundaries. It prevents a second
dispatch surface where adding a new variant can compile while silently skipping
one consumer. Good examples are the RPC `Method` registry for wire method names
and `CHANNEL_COMPILE_SPECS` for channel compile keys, where one canonical owner
drives downstream coverage.

### Deep-review checklist (high-risk only)

For `risk:high` PRs, verify a concrete example in each category. One concrete instance beats five generic claims.

- **Security boundaries**: deny-by-default behavior preserved, no accidental scope broadening.
- **Failure modes**: error handling explicit, degrades safely.
- **Contract stability**: CLI, config, or API compatibility preserved or migration documented.
- **Diff shape**: large or new-integration PRs are coherent, merge-justified now, not easily split, and not mostly duplicated machinery.
- **Generated artifacts**: generated files that affect policy, schema, routes, migrations, lockfiles, release artifacts, capabilities, packages, runtime behavior, or reviewer evidence are reviewed like source.
- **Toolchain compatibility**: MSRV or pinned-toolchain changes are intentional, aligned across CI/Docker/install surfaces, and documented for downstream/source-build users.
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
| `status:no-stale` | Accepted or otherwise long-lived work should stay open and is not already protected by another stale exclusion. Record the reason and routing evidence using the contributor-visible sources in the [Project board contract](./pr-workflow.md#issue-routing-evidence). Active release trackers and active RFC or design trackers may use the tracker itself as the visible reason and routing surface while they remain active. |
| `type:tracker` | Active parent coordination issue for a release, roadmap, RFC/design thread, implementation batch, cleanup, or audit. Use only when the live label exists; do not substitute `roadmap` or `type:roadmap`. This is a finder/routing marker, not stale protection by itself. |
| `good first issue` | XS/S, self-contained, documented work with clear acceptance criteria, relevant code or docs links, a named mentor or contact, and low onboarding risk. |
| `help wanted` | Actionable, unblocked work maintainers want external help on and can review. Do not use it as a generic valid/unowned marker. |

Assignee means active work. Routing evidence records why an issue needs special stale protection, tracker treatment, or a deferred maintainer decision. `status:blocked` only needs the recorded unresolved blocker unless it also needs separate `status:no-stale` protection. The [Project board contract](./pr-workflow.md#issue-routing-evidence) defines the accepted evidence sources and routing outcomes. Labels can identify the likely area, but labels alone are not ownership or stale protection.

### Resolution labels

Use resolution labels only when closing or removing an item from the active queue. They explain the terminal outcome; they do not replace `status:*` lifecycle labels on work that should stay open. The [labels guide](./labels.md#resolution-labels) is the source of truth for current resolution-label definitions and migration holdbacks.

For duplicates, link the canonical target before closing or redirecting discussion. For invalid reports, explain what makes the report unactionable or where it should go instead. For work we are explicitly choosing not to pursue, use the board-level `Won't Do` / live `wontfix` path and leave a brief rationale.

For replaced PRs or issue paths, use [Superseding PRs](./superseding.md) and preserve contributor attribution when relevant.

If logs or payloads in the report contain personal identifiers or sensitive data, request redaction before deeper triage. The triage process must not propagate the exposure.

## Discussions stewardship

Discussions are a maintained community surface only when a steward or review cadence exists. The default cadence is a weekly maintainer pass over new and recently active threads. A named steward may own that surface pass, but the steward maintains the surface; they do not become the owner of every question, idea, or implementation that appears there.

During each Discussions pass:

1. Check new and recently active threads for category fit, unanswered Q&A, spam, sensitive data, and threads that have produced a concrete project outcome.
2. Keep lightweight community conversation in Discussions when it is still exploratory, answerable there, or useful as a showcase, demo, poll, announcement, or broad-feedback thread.
3. Promote concrete outcomes to the owning tracked surface: bugs and accepted feature scopes to issues, architecture proposals to RFC issues, PR-specific details to PR comments, durable operating rules to maintainer or contributor docs.
4. Close the loop in the originating Discussion with a short summary and link to the issue, RFC, PR, or doc that now owns the outcome. Mark an answer only when the category and result make that accurate.
5. Redirect security-sensitive threads to the private vulnerability path in [Security issues](../contributing/communication.md#security-issues), handle sensitive data under [Privacy](../contributing/privacy.md), and close pure advertising or not-project-relevant threads. Preserve useful project-related demos or integrations as community showcase material when they are not asking maintainers to track work.

If Discussions are not being reviewed on the documented cadence, do not present them as a required intake path. Treat them as a passive archive until a steward or cadence is restored.

### PR backlog pruning

When review demand exceeds capacity:

1. Keep active bug and security PRs (`size:XS` or `size:S`) at the top of the queue.
2. Ask overlapping PRs to consolidate; close older ones with a superseded or replaced rationale after the author acknowledges. See [Superseding PRs](./superseding.md) for the attribution rules.
3. Use the PR stale ramp below. PR backlog pruning uses `needs-author-action` and `stale-candidate`; issue stale sweeps use `status:stale` under the canonical [issue stale policy](./labels.md#issue-stale-policy).

When a maintainer submits a request-changes review and the next step is on the PR author, apply `needs-author-action` in the same review/label packet. Do not add it when the requested change is maintainer-fixable and a maintainer intends to push the cleanup, when another maintainer or owner is taking over the branch, or when the block is waiting on a maintainer decision rather than author work.

| State | When to use | Required public note | Follow-up |
|---|---|---|---|
| `needs-author-action` | The next PR step is on the author: rebase, conflict fix, scope split, review answer, requested code change, or refreshed validation. | A review or comment names the concrete action. Request-changes reviews should apply this label when they leave the next action with the author. | Remove the label when the author pushes a substantive update or provides requested information, then continue normal review. This is not a closure warning by itself. |
| `stale-candidate` | A prior author-action request has sat unanswered and the PR now blocks useful review, or the branch is clearly stale, dirty, or obsolete against current `master`. Do not stale-escalate work that is parked under a visible maintainer plan, explicit dependency, active owner, or recorded revisit date. | A comment names the requested action and a follow-up date, normally 7-10 days out unless a maintainer chooses a longer window. It should distinguish a stale branch from a still-valid bug or feature request. | At the follow-up date, re-check live state. If the author responded or the branch became reviewable, remove or keep off `stale-candidate`. If there is still no response and no maintainer takeover or replacement path, close with a backlog-hygiene rationale and a clear reopening or replacement path. |

If the underlying bug or feature is still valid, preserve it in an issue, tracker row, replacement PR, or takeover plan instead of implying that the idea was rejected. Require rebase + fresh validation evidence before reopening anything that's been stale-closed.

## Automation override

Use this when automation output creates review side effects:

1. **Incorrect risk label**: set the intended `risk:*` label. If future risk automation is active, also follow the [labels automation contract](./labels.md#automation-contract) for `risk:manual`.
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

- Walk the stale queue. Apply `status:no-stale` only under the rules in the [Project board contract](./pr-workflow.md#issue-routing-evidence): when accepted or otherwise long-lived work has a recorded reason to stay open, contributor-visible routing evidence, and no other stale exclusion already applies. Active release trackers and active RFC or design trackers may keep stale protection by default when the issue itself clearly identifies the active coordination or decision surface; revisit them when the milestone closes, the tracker drifts from live state, the RFC reaches a decision, is superseded, or closes, or the issue stops representing an active project decision surface. Until the stale-exemption audit lands, treat existing `status:no-stale` issues missing those facts as audit findings rather than automatic stale candidates.
- Prioritize `size:XS` or `size:S` bug and security PRs first.
- Convert recurring support questions into docs improvements and auto-response guidance.

The goal is a queue where every open PR is either being actively reviewed, blocked on the author, or blocked on something external, never just sitting because nobody got to it.
