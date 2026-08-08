# RFC Process

Substantial changes to ZeroClaw's architecture, user-facing surface, or core policies go through an RFC before implementation. The process exists to surface design trade-offs, give maintainers and contributors a chance to push back early, and leave a searchable record of *why* a decision was made.

[FND-003](../foundations/fnd-003-governance.md#8-the-rfc-governance-loop) is the authoritative source for RFC governance, ratification, and voting thresholds. This page summarizes the contributor-facing process without replacing those rules.

## When to file an RFC vs. just a PR

| Change | RFC first? |
|---|---|
| New channel implementation | No: open a PR |
| New provider implementation | No: open a PR |
| New tool | No: open a PR |
| Bug fix | No: open a PR |
| New config key | Depends: if it fits within existing schema shape, PR. If it introduces a new subsystem or paradigm, RFC |
| Changing an established default | Yes: RFC |
| Schema migration that breaks existing configs | Yes: RFC |
| Cross-cutting refactor affecting multiple crates | Yes: RFC |
| New subsystem (e.g. a new security layer, a new protocol) | Yes: RFC |
| Changes to governance, release process, or contribution model | Yes: RFC |

Rule of thumb: if you'd want a second opinion before writing the code, it's an RFC. If it's obvious what to build, it's a PR.

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

Filed RFCs go through a discussion period of at least seven days, and larger proposals may remain in discussion longer. Anyone can comment, maintainers weigh in, and the RFC author iterates on the body in response. Ordinary author revisions are expected during this window and do not automatically extend or restart it. A material revision establishes a new stable snapshot and restarts the minimum seven-day discussion period before voting.

## Ratification

Voting starts after the minimum discussion period when the proposal is stable. The required threshold depends on the change type: a simple majority for low-stakes documentation, tooling, and non-breaking features; a two-thirds majority for moderate-stakes API, subsystem, behavioral, release-process, and contribution-process changes; and unanimous agreement for architecture, security-model, breaking, governance-authority, team-organization, and ratification-rule changes. See [FND-003 §8.2](../foundations/fnd-003-governance.md#82-vote-thresholds) for the authoritative classifications, overlap rule, thresholds, electorate, and outcome precedence.

A formal **REVISE** ballot closes the current vote without acceptance and returns the RFC to discussion under FND-003. It does not automatically require another seven-day period; only a resulting material revision restarts that minimum discussion period.

The outcomes:

- **Accepted**: issue carries `status:accepted`, and a maintainer comment records the final shape and links its durable disposition. Implementation PRs can proceed once that governance handoff is visible.
- **Rejected**: issue closed with a maintainer comment giving the rationale. This rejects the current proposal, not necessarily its underlying problem; the closing summary links any issue or tracker where that problem continues. The record lives; re-proposing requires a materially different take.
- **Deferred**: issue stays open with a maintainer comment noting why it's parked and the condition for another vote. An unchanged proposal may return to a new vote without repeating the minimum discussion period; a material change restarts the seven-day minimum. Add `status:blocked` when it's waiting on a specific prerequisite.
- **Withdrawn**: the author pulls it. Closed without prejudice.

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

The list above is the canonical source. A snapshot of notable open RFCs at time of writing (browse the live list for the current set):

- **#6808**: Work Lanes, Board Automation, and Label Cleanup (governance, in progress)
- **#6971**: Security UX, runtime credential boundaries, and isolation defaults
- **#6996**: Granular sandbox policy: filesystem and network restrictions
- **#7218**: A2A agent discovery (`.well-known/agent-card.json`) for multi-agent installs
- **#7184**: Move translated `.ftl` and `.po` files into a git submodule

## Ratified foundational RFCs

These shape everything else. Read them before proposing cross-cutting changes:

- **#5574**: Microkernel transition: crate split, feature-flag taxonomy, v1.0 path
- **#5576**: Documentation standards and knowledge architecture
- **#5577**: Project governance: core team, voting thresholds, this document's authority
- **#5579**: Engineering infrastructure: CI pipelines, release automation
- **#5615**: Contribution culture: human/AI co-authorship norms
- **#5653**: Zero Compromise: error handling, dead-code policy, release-readiness bar

## AI-authored RFCs

RFC authorship by AI assistants (with a human sponsor) is explicitly permitted per RFC #5615. If an RFC was drafted with AI help:

- Mark it clearly in the body ("drafted with Claude, reviewed by @maintainer")
- The sponsoring human is responsible for accuracy and for responding to review
- Only current Core Team members cast binding ratification ballots; activity and the vote denominator are determined dynamically under FND-003, and human sponsorship does not create voting authority

This has worked well so far. Treat AI drafts as first-class but remember the sponsor is accountable.

## See also

- [How to contribute](./how-to.md)
- [Communication](./communication.md)
- [Philosophy](../philosophy/index.md)
