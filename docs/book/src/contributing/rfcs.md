# RFC Process

An RFC records a durable project-level decision before implementation. The process exists to surface design trade-offs, give maintainers and contributors a chance to push back early, and leave a searchable record of *why* a decision was made.

Most work does not need one. The RFC trigger is deliberately narrow so that proposals which genuinely need a project-level decision are not queued behind ordinary features.

RFC scope and ratification rules are defined by [FND-003](../foundations/fnd-003-governance.md). The current protocol was narrowed by [#9496](https://github.com/zeroclaw-labs/zeroclaw/issues/9496), clarified for deferred votes by [#10288](https://github.com/zeroclaw-labs/zeroclaw/pull/10288), and simplified by [#10549](https://github.com/zeroclaw-labs/zeroclaw/issues/10549).

## When to file an RFC vs. just a PR

File an RFC when the proposal requires a durable project-level decision before implementation, meaning it is at least one of:

- a new security layer, or a material change to the project's security model;
- a governance, contribution-process, or project-authority change;
- a cross-cutting architectural refactor that changes ownership or contracts across established boundaries; or
- a new subsystem, or another project-wide capability boundary.

Do **not** file an RFC merely because the work includes:

- an ordinary feature addition;
- a schema or data migration;
- a configuration field or default change; or
- a bounded implementation refactor.

Those go through an issue and a PR. A new channel, a new provider, a new tool, and a bug fix are all ordinary work, however large the diff. They need an RFC only when their substantive effect also crosses one of the four triggers above.

The test follows substantive project effect, not the issue title, the author, whether the draft was AI-assisted, or the mere presence of a migration, feature, or default change. When you are unsure, open an ordinary issue and say why you think it might cross a trigger. A maintainer can promote it; that costs far less than a stalled RFC.

Security vulnerabilities are reported privately per [SECURITY.md](https://github.com/zeroclaw-labs/zeroclaw/blob/master/SECURITY.md), never as a public RFC.

Maintainers may relabel or close a filed RFC as an ordinary issue, feature request, or implementation follow-up when it does not meet the trigger. That disposition says whether the underlying work remains valid and where it continues; it is a routing decision, not a rejection on substance.

## Filing an RFC

RFCs are GitHub Issues tagged `type:rfc`. Title format:

```
RFC: <short description of the proposal>
```

Body structure: adapt to the size of the proposal:

1. **Problem**: what user pain or system deficiency motivates this?
2. **Proposal**: what are you proposing to do?
3. **Design**: the details; code sketches, schema shapes, migration plans
4. **Alternatives considered**: what else did you evaluate, and why not?
5. **Non-goals**: what this proposal explicitly isn't trying to solve
6. **Risks and mitigations**: what could go wrong, and what's the rollback story
7. **Rollout**: feature-flagged? schema-versioned? breaking change window?

Filed RFCs go through snapshot preparation and discussion, but there is no mandatory minimum discussion timer. Anyone can comment. Maintainers weigh in. The author iterates on the body in response.

A Core vote may open once the proposal has a visible stable snapshot, the vote opener identifies the RFC trigger, and a Core contributor is willing to put that snapshot to a decision. Body edits before a vote do not restart an automatic waiting period; body edits after a vote opens either stop the current vote as a material replacement or stay outside the voted snapshot until the next revision.

## Ratification

A vote cycle normally stays open for **72 hours** against an immutable proposal snapshot, identified by an immutable artifact or commit, or by a recorded issue-body digest plus a concise decision summary. The vote-opening comment records the snapshot, the assigned electorate, the threshold and why it applies, that quorum requires two explicit ballots, and the exact UTC deadline. It may not add ad hoc closing conditions outside this protocol.

**Electorate.** An active Core contributor is a current Core Team member who cast an explicit ballot in a formally opened RFC vote within the preceding 30 days. Any current Core Team member outside that set may still ballot; doing so joins them to that vote's electorate and reactivates them for later votes.

**Ballots** are `APPROVE`, `REVISE`, or `REJECT`. `APPROVE` accepts the snapshot as written, optionally with implementation boundaries. `REVISE` stops the current snapshot when it names the affected contract or body term, the concrete body change or decision needed, and why the ambiguity prevents approval. `REJECT` is a blocking objection and needs a specific reason. While a vote remains open, your latest `APPROVE` or `REJECT` before the recorded deadline supersedes your earlier ballot; a valid `REVISE` is terminal for that snapshot.

**Threshold.** Two-thirds of the final active electorate by default, rounded up to a whole voter. Quorum requires at least two explicit ballots; silence never counts toward quorum. Once quorum is met, silence from the electorate counts as `APPROVE` for ordinary votes. Unanimity is reserved for decisions whose cost or irreversibility makes a supermajority inadequate, such as license or legal-ownership changes; it requires explicit `APPROVE` from every assigned voter, and silence cannot establish it.

Outcomes, applied in this order:

- **Returned to revision**: a valid eligible `REVISE` before the deadline stops the current snapshot. The return record names the blocking body change or decision needed.
- **Deferred**: the vote cannot close because it has fewer than two explicit ballots, or because quorum is met with no valid `REVISE` or `REJECT` but the applicable threshold or required explicit approvals are not yet satisfied. The proposal may enter another 72-hour cycle against the same immutable snapshot. The continuation or reopening record names the unchanged snapshot, carried ballots, electorate so far, threshold, missing condition, and exact UTC deadline. Existing explicit ballots count toward quorum and outcome in the renewed cycle, and carry forward until withdrawn or replaced by a later ballot. A material proposal change creates a new snapshot.
- **Rejected**: quorum met and any final ballot is `REJECT`. Issue closed with the blocking objection recorded, linking any issue where the underlying problem continues. This rejects the proposal, not necessarily the problem.
- **Accepted**: quorum met, no valid `REVISE` or `REJECT`, and the applicable threshold is satisfied. By default, that means at least two-thirds approve explicitly or by silence; exceptional-unanimous votes require explicit `APPROVE` from every assigned voter. Issue carries `status:accepted`, and the closing record or implementation handoff records non-blocking implementation boundaries when they were raised. Implementation PRs can proceed once that handoff is visible.
- **Returned to discussion**: the proposal body materially changes, or the author asks to revise before a decision. The return record names the next stable proposal target or the revision needed.
- **Withdrawn**: the author pulls it. Closed without prejudice.

A vague `REVISE` that does not name a concrete body blocker does not stop the vote or count toward quorum until it is cured. The vote opener may ask for clarification, but cannot set aside a facially valid `REVISE` alone; if validity is disputed, one other Core contributor must agree that the ballot is invalid or the ballot stands.

Silence-as-approval is evaluated only at the deadline for a vote that remains open and has no valid `REVISE` or `REJECT`. Silence never overrides a stopped snapshot.

A vote may close early only when every member of the final active electorate has explicitly approved and no inactive Core contributor has asked for the full window. The closing record must say why it closed early.

The current protocol applies to RFC votes opened after the documentation PR that ratifies that revision lands, unless Core explicitly records earlier use on a specific issue. It does not automatically invalidate earlier accepted RFCs; historical-process audit and correction work are tracked separately.

### Vote records

Use the prose requirements in this guide as the canonical format for vote-opening, continuation, and closing comments. There are no separate required vote-record templates; the RFC issue form collects the initial proposal, not the vote record. [FND-003](../foundations/fnd-003-governance.md#81-the-full-rfc-lifecycle) governs ballot validity, deadlines, and outcome precedence.

An opening record includes the RFC trigger and why the snapshot is ready, alongside the ratification fields above. An unchanged-snapshot continuation lists carried ballots that have not been withdrawn or replaced and sets a new exact UTC deadline; it does not leave the vote pending indefinitely. A closing or return record identifies the snapshot, eligible ballots, outcome and reason, and the required revision, implementation boundaries, or continuation. A valid eligible `REVISE` stops that snapshot rather than becoming a concern to tally past at closure.

## Implementing an accepted RFC

Implementation PRs should:

- Confirm the RFC issue records its final accepted shape and durable disposition before implementation begins
- Reference the RFC issue number (`Implements #5574 phase 1`)
- Fit within the accepted design, if a detail changes during implementation, update the RFC body or file a follow-up clarification issue
- Ship behind a feature flag if the RFC calls for gradual rollout
- Include migration paths for users affected by breaking changes

Large RFCs often ship across multiple PRs over several releases. The RFC's tracking comment gets updated as phases land.

## Current open RFCs

Open RFCs are the best primary source for "what's coming next" in ZeroClaw. Browse:

<div class="os-tabs-src">

#### sh

```sh
gh issue list --repo zeroclaw-labs/zeroclaw --label type:rfc --state open
```

</div>

That query is the canonical source. This page deliberately does not mirror a snapshot of it, because a hand-maintained list drifts out of date faster than anyone notices.

## Ratified foundational RFCs

These shape everything else. Read them before proposing cross-cutting changes:

- **#5574**: Microkernel transition: crate split, feature-flag taxonomy, v1.0 path
- **#5576**: Documentation standards and knowledge architecture
- **#5577**: Project governance: core team, this document's authority. Its RFC scope and voting thresholds are superseded by [#9496](https://github.com/zeroclaw-labs/zeroclaw/issues/9496) (FND-003 Rev. 15)
- **#5579**: Engineering infrastructure: CI pipelines, release automation
- **#5615**: Contribution culture: human/AI co-authorship norms
- **#5653**: Zero Compromise: error handling, dead-code policy, release-readiness bar

## AI-authored RFCs

RFC authorship by AI assistants (with a human sponsor) is explicitly permitted per RFC #5615. If an RFC was drafted with AI help:

- Mark it clearly in the body ("drafted with Claude, reviewed by @maintainer")
- The sponsoring human is responsible for accuracy and for responding to review
- Only current Core Team members cast binding ballots. Sponsoring an AI-drafted RFC does not create voting authority, and an AI-assisted origin does not by itself change whether a proposal meets the RFC trigger

This has worked well so far. Treat AI drafts as first-class but remember the sponsor is accountable.

## See also

- [How to contribute](./how-to.md)
- [Communication](./communication.md)
- [Philosophy](../philosophy/index.md)
