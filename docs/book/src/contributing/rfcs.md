# RFC Process

Substantial changes to ZeroClaw's architecture, user-facing surface, or core policies go through an RFC before implementation. The process exists to surface design trade-offs, give maintainers and contributors a chance to push back early, and leave a searchable record of *why* a decision was made.

Governance, RFC ratification rules, and voting thresholds are defined in RFC #5577.

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

Filed RFCs go through a discussion window (default 7 days, longer for larger proposals). Anyone can comment. Maintainers weigh in. The RFC author iterates on the body in response.

## Ratification

Per RFC #5577, RFCs are ratified by a two-thirds maintainer majority. The outcomes:

- **Accepted**: issue carries `status:accepted`, and a maintainer comment records the final shape and links its durable disposition. Implementation PRs can proceed once that governance handoff is visible.
- **Rejected**: issue closed with a maintainer comment giving the rationale. The record lives; re-proposing requires a materially different take.
- **Deferred**: issue stays open with a maintainer comment noting it's parked; revisit later. Add `status:blocked` when it's waiting on a specific prerequisite.
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
- The human takes the ratification vote, not the AI

This has worked well so far. Treat AI drafts as first-class but remember the sponsor is accountable.

## See also

- [How to contribute](./how-to.md)
- [Communication](./communication.md)
- [Philosophy](../philosophy/index.md)
