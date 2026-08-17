# FND-003: Team Organization, Project Governance, and Contribution Pipeline

> Starting v0.7.0 · Type: Governance · Rev. 15
>
> **Canonical reference** · Ratified by the team · Rev. 15
> Original governance discussion: [#5577](https://github.com/zeroclaw-labs/zeroclaw/issues/5577)
> Follow-up work-lane and label-governance policy: [#6808](https://github.com/zeroclaw-labs/zeroclaw/issues/6808)

---

> **A note to the team before you read this.**
>
> Software projects do not fail because the code is bad. They fail because the people writing the code cannot coordinate. Features get built twice. Bugs get lost. Good ideas evaporate because nobody wrote them down. New contributors show up wanting to help and cannot find where to start. This RFC is about building the lightweight scaffolding that prevents those failures, not so the project feels organized, but so the team can move faster, with more confidence, and with less friction. Every recommendation here is chosen specifically for a small, growing, student-led open source team. Nothing here requires a project manager, a Scrum Master, or a formal committee.

---

## Revision History

| Rev | Date | Summary |
|---|---|---|
| 1 | 2026-04-09 | Initial draft |
| 2 | 2026-04-09 | Added §6.4 Architectural Compliance: Human Review, AI Support; added Discussion Question on AI automation of architecture reviews |
| 3 | 2026-05-24 | Added #6808 operational-label-policy pointers; current label behavior lives in maintainer docs ([#6899](https://github.com/zeroclaw-labs/zeroclaw/pull/6899)) |
| 4 | 2026-05-24 | Added #6808 community-pickup and issue-risk/PR-risk operational pointers ([#6903](https://github.com/zeroclaw-labs/zeroclaw/pull/6903)) |
| 5 | 2026-05-25 | Promoted #6808 feature-facing work-lane and label-governance policy into FND-003; clarified durable source boundaries, Discussions stewardship, Discord-to-GitHub handoff, and where operational gate questions live ([#6919](https://github.com/zeroclaw-labs/zeroclaw/pull/6919)) |
| 6 | 2026-05-27 | Made board-level `Won't Do` a durable closure decision and delegated current terminal-label and replacement-process rules to maintainer sources ([#6929](https://github.com/zeroclaw-labs/zeroclaw/pull/6929)) |
| 7 | 2026-06-07 | Expanded project-board planning ownership to an active owner or steward path and required stale-exemption reason plus active movement ownership ([#7011](https://github.com/zeroclaw-labs/zeroclaw/pull/7011)) |
| 8 | 2026-06-14 | Replaced owner-or-steward requirements with contributor-visible routing evidence for project-board and stale-exemption policy ([#7571](https://github.com/zeroclaw-labs/zeroclaw/pull/7571)) |
| 9 | 2026-06-16 | Made `.github/ISSUE_TEMPLATE/` the operational intake source, defined the current intake lanes, and kept judgment-only labels maintainer-applied ([#7652](https://github.com/zeroclaw-labs/zeroclaw/pull/7652)) |
| 10 | 2026-06-23 | Standardized size-label spelling and changed PR-size labeling from required automation to a future optional mechanism aligned with maintainer policy ([#8111](https://github.com/zeroclaw-labs/zeroclaw/pull/8111)) |
| 11 | 2026-07-05 | Changed the RFC lifecycle to issue-first governance and linked the foundational RFCs to their canonical FNDs ([#8694](https://github.com/zeroclaw-labs/zeroclaw/pull/8694)) |
| 12 | 2026-07-12 | Revised issue stale timing and qualifying-activity policy; made the maintainer label guide the sole operational source ([#8989](https://github.com/zeroclaw-labs/zeroclaw/pull/8989)) |
| 13 | 2026-07-18 | Replaced the universal ADR requirement with an explicit durable-disposition rule for accepted RFCs; reserved ADRs for significant architecture decisions ([#9136](https://github.com/zeroclaw-labs/zeroclaw/pull/9136)) |
| 14 | 2026-07-25 | Retired the `CONTRIBUTORS.md` membership record and the `zeroclaw-core`/`zeroclaw-contributors` team names, none of which were ever created; §5.3 now names the `core-contributors` GitHub team, CODEOWNERS, and the Communication maintainer table as the real records ([#9388](https://github.com/zeroclaw-labs/zeroclaw/pull/9388)) |
| 15 | 2026-08-10 | Narrowed the RFC trigger to four project-level categories and named the ordinary work that does not require an RFC; replaced the seven-day discussion period with 48h ordinary / 72h exceptional; defined the 72-hour vote against an immutable snapshot, the 30-day active electorate, two-ballot quorum, silence-as-approval after quorum, non-vetoing `REVISE`, and outcome precedence; made two-thirds the default threshold and reserved unanimity for expensive or irreversible decisions; retired the nonexistent parallel `rfc:*` label family; added the GitHub bridge record for Core meeting decisions ([#9499](https://github.com/zeroclaw-labs/zeroclaw/pull/9499)) |

---

## Table of Contents

1. [The Coordination Problem](#1-the-coordination-problem)
2. [The Three-Part System](#2-the-three-part-system)
3. [GitHub Projects: The Work Pipeline](#3-github-projects-the-work-pipeline)
   - [3.6 Work Lanes and State Ownership](#36-work-lanes-and-state-ownership)
4. [GitHub Discussions: Community Discussion and Handoff](#4-github-discussions-community-discussion-and-handoff)
   - [4.5 Discussions Stewardship And Discord-to-GitHub Handoff](#45-discussions-stewardship-and-discord-to-github-handoff)
5. [Team Tiers and Contribution Authority](#5-team-tiers-and-contribution-authority)
6. [CODEOWNERS and Branch Protection](#6-codeowners-and-branch-protection)
   - [6.4 Architectural Compliance: Human Review, AI Support](#64-architectural-compliance-human-review-ai-support)
7. [Issue Templates](#7-issue-templates)
8. [The RFC Governance Loop](#8-the-rfc-governance-loop)
9. [Label Taxonomy](#9-label-taxonomy)
10. [Definition of Done](#10-definition-of-done)
11. [Automation](#11-automation)
12. [Phased Rollout](#12-phased-rollout)

---

## 1. The Coordination Problem

Every project without an intentional coordination system develops an accidental one. The accidental system for most open source projects looks like this:

- Ideas live in someone's head, or in a chat message that scrolls off the screen
- Issues pile up in the tracker with no priority, no owner, and no clear definition of done
- Contributors open PRs for things nobody asked for, or ask to help and get no response
- The team works reactively: whoever shouts loudest gets attention, whatever breaks gets fixed, nothing gets planned more than a week out
- Architectural decisions get made in PR comments and are never recorded anywhere

This is not a criticism of anyone's effort. It is a description of what happens by default. The solution is not more process. It is the right process, applied at the right level for the size and maturity of the team.

ZeroClaw needs three things:

1. **A pipeline** for turning ideas into shipped code, with visible stages and clear gates at each transition
2. **A maintained discussion lane** for community questions, ideas, showcases, and early exploration that are not ready for the pipeline yet, without losing them or cluttering the active work
3. **A governance model** that defines who can decide what, how architectural decisions get made, and how the team grows

These are three distinct concerns. Conflating them, putting everything in one board, or relying on informal chat for decisions, is what creates the chaos the team is trying to escape.

---

## 2. The Three-Part System

| Concern | Tool | Why This Tool |
|---|---|---|
| Work pipeline (backlog → release) | **GitHub Projects v2** | Custom fields, multiple views, Kanban + roadmap, built-in automation, milestone tracking |
| Community discussion and idea incubation | **GitHub Discussions** | Community-visible, no PR required, separates early conversation from committed work, promotes concrete outcomes into the owning tracked surface |
| Governance and decision authority | **RFC process + Team Tiers + CODEOWNERS** | Established through RFC issues, foundation docs, and CODEOWNERS; needs formalization and close loop |

The key principle: **the Project board contains only work the team has committed to thinking about.** Early community discussion, ideas, Q&A, and showcases can live in Discussions when the lane is maintained. Work that has been evaluated, accepted, and scoped lives in the Project. This distinction is what keeps the board useful.

FND-003 is the durable governance source for work-lane and contribution-pipeline policy. RFC #6808 was the staging discussion for feature-facing work lanes, label governance, issue triage, and maintainer routing; after its policy slices are promoted, their durable rules live in this foundation document plus the maintainer operational pages linked below. Do not treat the RFC issue as a competing governance document after its policy has been promoted here.

Operational details intentionally live close to the workflow that uses them:

| Durable decision | Operational home |
|---|---|
| Project board purpose and stage gates | This document |
| PR lanes and merge/review queue discipline | [Maintainer PR workflow](../maintainers/pr-workflow.md) |
| Label definitions, ownership boundaries, and cleanup protocol | [Maintainer labels guide](../maintainers/labels.md) |
| Reviewer intake, risk depth, issue triage, and queue hygiene | [Reviewer playbook](../maintainers/reviewer-playbook.md) |
| Mechanical issue-triage procedure and stale pass details | [Maintainer skills guide](../maintainers/skills.md#issue-triage-workflow) and [Reviewer playbook](../maintainers/reviewer-playbook.md#issue-triage) |
| Contributor-facing filing and PR mechanics | Issue templates, PR template, and [How to contribute](../contributing/how-to.md) |
| Contributor communication, Discussions stewardship, and Discord-to-GitHub handoff | [Communication](../contributing/communication.md) and §4.5 below |
| RFC-shaped contribution routing before implementation | [Architecture and contribution map](../contributing/architecture-map.md) and [RFC process](../contributing/rfcs.md) |

---

## 3. GitHub Projects: The Work Pipeline

### 3.1 The Pipeline Stages

The Project board has a single **Status** field with seven values. Each value is a stage in the pipeline. The sequence is linear but items can be moved back:

```
💡 Idea
    ↓  Gate: Vision alignment check
📋 Backlog
    ↓  Gate: Architecture fit + acceptance criteria
🎯 Defined
    ↓  Gate: Assignee, size, risk tier confirmed
🚧 In Progress
    ↓  Gate: Tests written, CI passing
👀 In Review
    ↓  Gate: Correct reviewer tier approved, docs updated
✅ Done
```

Plus one terminal state that can be reached from anywhere:

```
🚫 Won't Do  ← explicit decision not to pursue; never silently closed
```

The board-level `Won't Do` state is a durable closure decision. Current closure-label spelling and replacement-process rules live in the [maintainer label guide](../maintainers/labels.md#resolution-labels) and [superseding guide](../maintainers/superseding.md).

### 3.2 The Gate Questions

Every transition has a gate question. The question must be answered "yes" before the item moves forward. This is the project board made operational: the Vision → Architecture → Design → Implementation → Testing → Documentation hierarchy becomes a checklist at each stage.

| Transition | Gate Question | Who Checks |
|---|---|---|
| Idea → Backlog | Does this align with the Vision statement? Does it fit the target architecture? | Core Team triage |
| Backlog → Defined | Is there a clear acceptance criteria? Does it need an ADR or design note? Is the risk tier assigned? | Assignee + reviewer |
| Defined → In Progress | Is there an assignee? Is it sized? Are the related ADRs or docs identified? | Assignee |
| In Progress → In Review | Do tests exist for the new behavior? Is CI passing? Is the PR description complete? | Author (self-check) |
| In Review → Done | Has the correct reviewer tier approved? Is documentation updated? Is the CHANGELOG entry written? | Reviewer |
| Any → Won't Do | Has the decision not to pursue been explained in the item's comments? | Core Team |

**Why explicit gates matter for a student team:** Without gates, cards move because someone feels done, not because done has a definition. This is the single most common source of "done" work that is not actually done. The gates make the definition visible and shared.

These gate questions are governance prompts, not another checklist to duplicate in every PR body or issue comment. The operational forms live in the artifacts that maintainers already touch:

- issue templates collect the report, user value, reproduction, architecture impact, and risk hints needed for first triage;
- the PR template collects scope boundary, validation evidence, security/privacy impact, compatibility, rollback, labels, and linked issues;
- the maintainer PR workflow defines Definition of Ready, Definition of Done, PR lanes, and merge checks;
- the labels guide defines durable classification, stale-policy labels, and cleanup sequence;
- the reviewer playbook defines intake, review depth, issue triage, automation override, and queue hygiene.

If an old FND-003 gate question seems missing, first check those operational homes before adding another copy here.

### 3.3 Custom Fields

Create these fields in the GitHub Project settings:

| Field | Type | Values |
|---|---|---|
| **Status** | Single select | 💡 Idea · 📋 Backlog · 🎯 Defined · 🚧 In Progress · 👀 In Review · ✅ Done · 🚫 Won't Do |
| **Type** | Single select | Feature · Bug · Refactor · ADR · Docs · Security · Infrastructure · RFC |
| **Priority** | Single select | 🔴 Critical · 🟠 High · 🟡 Medium · 🟢 Low |
| **Size** | Single select | XS · S · M · L · XL |
| **Risk Tier** | Single select | Low · Medium · High (mirrors `AGENTS.md` risk tiers) |
| **Component** | Single select | Kernel · Gateway · Channels · Tools · Memory · Security · Hardware · Docs · Infrastructure |
| **Milestone** | Milestone | v0.7.0 · v0.8.0 · v0.9.0 · v1.0.0 · Icebox |

**On sizing (T-shirt sizes):** Story points require calibration and historical data the team does not have yet. T-shirt sizes are immediately intuitive and good enough for a team at this stage:

| Size | What It Means | Approximate Scope |
|---|---|---|
| XS | Under 2 hours | A typo fix, a config tweak, a one-line change |
| S | Half a day | A small bug fix, a minor feature addition, a docs update |
| M | 1–3 days | A meaningful feature, a refactor of one module, a new test suite |
| L | 1–2 weeks | A significant feature, a new crate extraction, a cross-cutting change |
| XL | More than 2 weeks | An architectural change; should be broken into smaller items |

XL items should almost always be broken down before they enter In Progress. If you cannot break it down, the design is not complete enough.

### 3.4 Views

Create four named views in the Project:

#### View 1: Roadmap

- Type: Roadmap (timeline)
- Grouped by: Milestone
- Visible fields: Title, Type, Size, Component, Assignee
- Purpose: Public-facing. "Here is what is coming and when." Share this link in the README and with the community. Keep it updated.

#### View 2: Board

- Type: Board (Kanban)
- Columns: Status field values
- Filtered to: Current milestone only
- Visible fields: Title, Assignee, Size, Risk Tier
- Purpose: Day-to-day work visibility. What is everyone working on right now? What is blocked?

#### View 3: Backlog

- Type: Table
- Sorted by: Priority (descending), then Size (ascending)
- Filtered to: Status = Backlog OR Defined
- Visible fields: Title, Type, Priority, Size, Component, Milestone, Risk Tier
- Purpose: Used during grooming sessions. What needs to be worked on next? What is sized and ready to pick up?

#### View 4: My Work

- Type: Board
- Filtered to: Assignee = @me
- Purpose: Personal dashboard. Each contributor can see their own items without noise.

### 3.5 Pinned Items

GitHub allows up to six pinned issues per repository. Use them for high-signal, always-visible communication:

1. The current active RFC under discussion
2. The most wanted community feature (highest-voted Discussion)
3. The next release milestone tracking issue
4. The good first issue index (an issue that links to all current `good first issue` items)

Pinned issues are a promise to the community: these are the things that matter most right now. Update them when priorities shift.

### 3.6 Work Lanes and State Ownership

Work-lane policy keeps the board, labels, PRs, and issues from trying to answer the same question in different places.

Use this split:

| Surface | Owns | Does not own |
|---|---|---|
| Labels | durable classification: type, scope, risk, size, contributor tier, stale/triage policy | per-push review state, active CI status, personal task lists |
| Project board | planning state: readiness, routing evidence, roadmap grouping, dependency/blocker state, stale-exemption reason when a field exists | authoritative PR review queue, mergeability, required checks |
| Native PR state | review decision, required checks, branch freshness, conflicts, mergeability, draft/ready state | long-term roadmap ownership |
| Issues/RFCs | durable discussion record, acceptance state, user need, linked implementation trail | live replacement for maintainer docs after policy promotion |

PR lanes, contributor-pickup labels, stale-exemption labels, and label migration are durable governance concepts, but their exact operational criteria live in maintainer docs. FND-003 owns the split: labels classify durable work, project boards plan work, native PR state owns live review and merge state, and issues/RFCs preserve decisions. The [Maintainer PR workflow](../maintainers/pr-workflow.md#pr-lanes) owns PR lane definitions, the [Labels guide](../maintainers/labels.md) owns exact label meanings and cleanup rules, and the [Reviewer playbook](../maintainers/reviewer-playbook.md#issue-triage) owns how reviewers apply those signals during triage and review. Treat live label migration as a separate maintainer-approved cleanup, not ordinary PR review.

Stale exemptions are governance exceptions, not permanent label shields. The target policy is that `status:no-stale` is valid only when the lane's operational source records why the issue is exempt and what visible routing evidence carries the next decision. The maintainer docs define where those facts live and how stale automation or stale sweeps enforce the rule.

---

## 4. GitHub Discussions: Community Discussion and Handoff

### 4.1 Maintained Discussions Lane

Treat GitHub Discussions as a maintained community surface. Discussions are useful for questions, ideas, polls, announcements, showcases, project or integration demos, and exploratory threads that need more permanence than Discord but are not yet tracked work.

Exact categories, category descriptions, and review cadence are operational details. They belong in the contributor communication guide and maintainer workflow docs, and they may evolve without revising this foundation document.

### 4.2 Promotion From Discussion To Tracked Work

Discussions do not become backlog work just because a thread exists. Promote a Discussion when it produces a concrete tracked outcome. Contributor-facing trigger examples live in [Communication](../contributing/communication.md).

The target depends on the result. Confirmed bugs and accepted feature scopes move to issues. Architecture decisions move through the RFC process. PR-specific details move to PR comments. Durable operating rules move to maintainer or contributor docs.

Close the loop in the originating Discussion. If the category supports answers, mark the summary or tracked-work link as the answer when that is appropriate. If it does not, add a final summary comment with the issue, RFC, PR, or docs link.

### 4.3 Ideas That Should Not Wait for Votes

Some items bypass Discussions and enter the tracked surface directly:

- Security vulnerabilities (via private security report, never public)
- Confirmed bugs with reproduction steps (go directly to Bug Report issue template)
- RFC-accepted architecture items (spawned directly from the RFC close loop)
- Items from the project roadmap (placed directly by Core Team)

### 4.4 Architecture Exploration

Architecture exploration can start in Discussions when the question is community-facing and not yet ready for a formal RFC. This lowers the barrier to raising design concerns without turning every early thought into tracked policy.

When the thread reaches a concrete architecture proposal, open the RFC issue and move the durable proposal into the RFC surface. The Discussion can then link to the RFC and stop being the source of truth.

### 4.5 Discussions Stewardship And Discord-to-GitHub Handoff

Discord is for fast conversation. GitHub is the durable record. Discussions are one maintained GitHub surface for community-facing conversation that needs more permanence than Discord but is not yet tracked work.

Discussions are active only when someone owns the lane. That ownership can be a named steward or a documented review cadence. Without ownership, Discussions are a passive archive, not a required intake path.

Use Discussions for exploratory, community-facing, or broad-feedback threads. Use an issue, RFC issue, PR comment, or maintainer doc when the outcome is already concrete or authoritative. The contributor-facing trigger list and category examples live in [Communication](../contributing/communication.md).

The handoff does not need to copy the whole chat. Capture the outcome and enough context for another maintainer to continue. If a Discussion later produces tracked work or durable policy, promote that result into the surface that owns it.

---

## 5. Team Tiers and Contribution Authority

### 5.1 The Three Tiers

Open source projects run on **meritocracy**: influence and authority come from demonstrated contribution, not from seniority, title, or who you know. This is one of the things that makes open source different from corporate software, and it is worth teaching explicitly.

The three tiers reflect increasing demonstrated commitment to the project:

---

#### Tier 1: Community

Anyone. No approval required.

*What they can do:*
- Open issues using the issue templates
- Comment on any issue or PR
- React to Discussions and vote on ideas
- Submit pull requests (which will be reviewed before merging)
- Edit the GitHub Wiki

*What they cannot do:*
- Be assigned issues (can request to be assigned)
- Approve PRs
- Merge PRs
- Vote on RFCs with binding authority

---

#### Tier 2: Contributor

Community members who have had at least two PRs merged into the `master` branch.

*How to become one:* Have two PRs merged, recognized by a Core Team member. Tier 2 has no durable membership record today; see §5.3.

*What they gain beyond Community:*
- Can be assigned issues
- Can be requested as a reviewer on PRs (non-required review)
- Vote on Ideas in Discussions counts toward the promotion threshold
- Can request RFC discussions without going through Discussions first

*What they still cannot do:*
- Approve PRs for High Risk paths
- Merge PRs
- Cast binding RFC votes

*Why this tier exists:* It creates a visible, achievable first milestone for new contributors. "How do I get more involved?" has a clear answer: get two PRs merged. This motivates good early contributions and gives the team a way to recognize contributors publicly.

---

#### Tier 3: Core Team

Contributors who have demonstrated consistent, high-quality contributions over time and have been invited by existing Core Team members.

*How to become one:* Invitation from existing Core Team members, announced publicly in Discussions. There is no formal threshold; it is a judgment call based on the quality, consistency, and alignment of past contributions.

*What they gain beyond Contributor:*
- Write access to the repository
- Can merge PRs that have met review requirements
- Can approve PRs for High Risk paths (subject to CODEOWNERS requirements)
- Cast binding votes on RFCs
- Can move items through the Project pipeline
- Can cut releases
- Participate in governance decisions (Core Team discussions)

*Responsibilities:*
- Triage new issues within 3 business days
- Review PRs in their area of expertise within 5 business days
- Participate in RFC votes
- Uphold the project's Code of Conduct

---

### 5.2 The Lazy Consensus Rule

For routine decisions, adding a label, closing a stale issue, updating documentation, Core Team members operate under **lazy consensus**: if you announce your intention in the relevant issue and no Core Team member objects within 48 hours, you proceed. This prevents the paralysis of requiring explicit approval for everything while maintaining visibility.

Lazy consensus does not apply to:
- RFC acceptance or rejection
- Releases
- Changes to CODEOWNERS or branch protection rules
- Changes to this governance document
- Additions to the Core Team

These always require explicit Core Team votes.

### 5.3 Recording Team Membership

Membership itself is established by decision, not by any file or GitHub setting. Per §5.1, someone becomes Core Team by invitation from existing Core Team members, announced publicly in Discussions. That decision, and its public announcement, is the source of truth. Everything below is a record of something downstream of it, and none of them is a membership roster:

**The `core-contributors` GitHub team** and the repository collaborator list, in the organization settings: **access controls**, not membership records. They answer who can write to the repository, which is a consequence of membership rather than a definition of it. Expect them to differ from the member list in both directions. They include automation accounts that are not people, and access can be granted directly, held from before a membership decision, or still pending acceptance of an invitation. When you need to know who can push, read these. When you need to know who is Core Team, read the announcement that admitted them.

**`.github/CODEOWNERS`** at the repository root: **review routing**, not membership. It records who is requested on which paths. Being listed does not confer membership and being a member does not imply being listed. Changes to it require an explicit Core Team vote, per §5.2.

**The maintainer table in [Communication](../contributing/communication.md#maintainer-contacts)**: the human-readable summary of current members and what each works on. It is the closest thing to a published roster, and it is maintained by hand, so treat it as a summary of admission decisions rather than as an authority. For focus areas it is a convenience view over CODEOWNERS, and where those two disagree, CODEOWNERS wins.

Removals work the same way as admissions: they are decisions, recorded where they are made. Revoking access or removing someone from CODEOWNERS implements a departure; it does not by itself constitute one.

Revisions 1 through 7 of this document specified a `CONTRIBUTORS.md` file at the repository root as a tier-organized membership record, and named `zeroclaw-core` and `zeroclaw-contributors` GitHub teams. None of the three was ever created; the organization uses a single `core-contributors` team instead. RFC #6808 reached the same finding independently, recording that the FND-003 team-tier structure is not the visible current routing model and that new lane rules should not be built on it. Those references are retired here rather than left standing as a description of machinery that does not exist.

Tier 2 has no durable membership record at present. Establishing one, or retiring the tier, is an open question for the team.

---

## 6. CODEOWNERS and Branch Protection

### 6.1 CODEOWNERS

The `CODEOWNERS` file makes governance automatic. It defines which paths require review from which team before a PR can merge. GitHub enforces this as a required review: the PR cannot be merged until the requirement is satisfied.

The block below is the original illustrative proposal, kept for the reasoning it shows about routing by risk tier. It is not the current file and should not be copied. `.github/CODEOWNERS` already exists and is actively maintained; it routes to individual handles rather than team handles, and its paths follow the post-microkernel crate layout established in #6537. The `@zeroclaw-labs/zeroclaw-core` and `@zeroclaw-labs/zeroclaw-contributors` handles used here were never created; see §5.3. Read the live file for current routing.

```
# CODEOWNERS — Automatic review routing by risk tier
# See AGENTS.md for risk tier definitions.
# See the governance foundation doc and RFC issue template for team tier definitions.

# ── High Risk: requires Core Team approval ──────────────────────────────────

src/security/**                 @zeroclaw-labs/zeroclaw-core
src/gateway/**                  @zeroclaw-labs/zeroclaw-core
src/runtime/**                  @zeroclaw-labs/zeroclaw-core
src/tools/shell.rs              @zeroclaw-labs/zeroclaw-core
src/tools/file_write.rs         @zeroclaw-labs/zeroclaw-core
src/tools/security_ops.rs       @zeroclaw-labs/zeroclaw-core

# ── Governance and configuration: requires Core Team approval ───────────────

.github/**                      @zeroclaw-labs/zeroclaw-core
CODEOWNERS                      @zeroclaw-labs/zeroclaw-core
Cargo.toml                      @zeroclaw-labs/zeroclaw-core
deny.toml                       @zeroclaw-labs/zeroclaw-core

# ── Architecture documents: requires Core Team review ───────────────────────

docs/book/src/foundations/**    @zeroclaw-labs/zeroclaw-core
docs/book/src/architecture/decisions/**  @zeroclaw-labs/zeroclaw-core
AGENTS.md                       @zeroclaw-labs/zeroclaw-core

# ── Default: any Contributor or Core Team member can review ─────────────────

*                               @zeroclaw-labs/zeroclaw-contributors
```

As specific Core Team members take ownership of components, add their individual handles alongside the team handle. Specificity wins in CODEOWNERS: a more specific path rule overrides a more general one.

### 6.2 Branch Protection Rules

Configure the following branch protection rules for `master`:

| Rule | Setting | Reason |
|---|---|---|
| Require a pull request before merging | Enabled | No direct pushes to master, ever |
| Require approvals | 1 for Low/Medium risk; 2 for High risk | CODEOWNERS enforcement handles the "who" |
| Require status checks to pass | `cargo fmt`, `cargo clippy`, `cargo test` | CI must be green before merge |
| Require branches to be up to date | Enabled | Prevents merging stale code |
| Require conversation resolution | Enabled | All review comments must be resolved |
| Do not allow bypassing the above settings | Enabled | Applies to everyone, including admins |
| Allow force pushes | Disabled | Preserve commit history |
| Allow deletions | Disabled | Protect the branch |

**Why admins cannot bypass:** One of the most common mistakes in small team projects is treating branch protection as "for other people." When an admin can bypass, they will, under time pressure, in an emergency, "just this once." Then it becomes the norm. The rule must apply to everyone for it to mean anything. If there is a genuine emergency, the right response is to follow the process faster, not to skip it.

### 6.3 Required Status Checks

The CI checks that must pass before any PR can merge:

```
build (stable)          ← cargo build --release
test                    ← cargo test
fmt                     ← cargo fmt --all -- --check
clippy                  ← cargo clippy --all-targets -- -D warnings
```

As the workspace decomposes into crates (per the architecture RFC), add per-crate checks. A change to `crates/zeroclaw-api` should run that crate's test suite independently.

### 6.4 Architectural Compliance: Human Review, AI Support

This section exists because the question will come up (it already has) and it deserves a clear, documented answer rather than a debate on every PR.

**The question:** Should we add an automated gate that checks whether a PR conforms to the architecture and design patterns defined in the RFCs?

**The answer:** No. And understanding why is important.

---

**There are two fundamentally different kinds of quality enforcement, and they require different mechanisms.**

The first kind is *structural compliance*: does this code violate a mechanical rule? Does `zeroclaw-kernel` import `TelegramChannel`? Do the dependency graph edges point the wrong way? Are there clippy warnings? These are binary questions. Either the code violates the rule or it does not. The compiler, `cargo deny`, and `cargo clippy --workspace` already enforce this. No human is needed. No AI is needed. The machine is authoritative, fast, and never wrong about a factual violation.

The second kind is *architectural intent*: does this decision belong here? Is this abstraction at the right layer? Does this trade-off align with the vision? Is this coupling going to be painful in Phase 3? Will this PR create a maintenance burden that isn't visible in the diff today? These questions require judgment, context, and an understanding of *why* the architecture exists, not just what the rules are. No automated tool can answer them reliably, because the answer depends on information that is not in the diff: the roadmap, the team's current priorities, the contributor's intent, and the long-term cost of the decision.

**The failure modes of automating architectural judgment are both bad.**

A gate that passes subtle architectural violations creates false confidence. The developer sees ✅ and assumes their decision was validated. The most damaging architectural drift, the kind that takes years to untangle, looks structurally correct. It compiles. It passes lint. The dependency graph is fine. The problem is that it violated the spirit of the design in a way that only becomes apparent later, when the cost of unwinding it is high.

A gate that flags valid architectural decisions because the tool misread the context teaches developers to dismiss the gate entirely. Once a team learns to click past a noisy automated check, the check is gone in practice even if it is still running in CI. The project has spent CI minutes to achieve negative value.

**CODEOWNERS is the architectural compliance gate. The reviewer is the tool.**

The `CODEOWNERS` configuration in §6.1 already enforces that PRs touching high-risk paths, crate boundaries, trait definitions, the dependency graph, `src/security/`, `.github/`, require review from a Core Team member. That Core Team member, equipped with the RFCs as their reference framework, is the architectural compliance check. They bring the contextual judgment that no automation can replicate.

This is why the RFCs, the AGENTS.md files, and the documentation standards exist: not so a machine can parse them and produce a score, but so a human reviewer has a consistent, documented framework to apply. The RFC answers "why does this architecture exist." The reviewer answers "does this PR serve or undermine that why."

**AI belongs in the development loop, not the merge gate.**

AI tools, Claude, Copilot, Cursor, and whatever comes next, are genuinely useful for architectural work when they are used in the right place. The right place is *during development*, not *during the merge gate*.

During development, an AI assistant equipped with the RFC and the crate's AGENTS.md can help a contributor understand which crate a new piece of functionality belongs in before they write it, flag a potential dependency inversion while the code is still being shaped, explain why a design pattern exists, and suggest whether a new abstraction is at the right layer. This is additive. It makes contributors more capable.

During a review, an AI assistant can help a human reviewer draft structured feedback, cross-reference a change against the RFC, and identify which discussion questions in the RFC are relevant to the PR. This is also additive. The reviewer brings the judgment; the AI brings speed and recall.

What AI cannot do is replace the judgment. "AI helps me assess this PR" and "AI automatically gates this PR" are categorically different, and only the first one works for architectural decisions. The day the project routes architectural compliance through an automated gate, however sophisticated, is the day the architecture starts drifting in ways nobody notices until it is too late.

**The practical policy, stated plainly:**

- Structural compliance (import direction, dependency graph, lint, format) is enforced by CI. This is non-negotiable and automated.
- Architectural intent compliance is enforced by CODEOWNERS routing to a Core Team reviewer. This is non-negotiable and human.
- AI tools support contributors during development and support reviewers during review. They do not gate merges on their own authority.
- If the team wants to evaluate AI-assisted review tooling in the future, that evaluation goes through the RFC process first. It does not get added to `.github/workflows/` without a documented decision.

This policy is not a limitation on AI or on automation. It is a recognition that different problems require different tools, and using the right tool in the right place is exactly what the architecture RFC is asking of the codebase.

---

## 7. Issue Templates

Issue templates route incoming reports to the right process before they reach a human. A well-written template gathers the information needed for triage automatically. A missing or ignored template results in issues that take three comment exchanges to understand.

The operational source of truth is `.github/ISSUE_TEMPLATE/`. Do not duplicate full template YAML here. When template wording changes, update the issue form itself and keep this section at the level of durable intent.

Current intake lanes:

| Template | Purpose | Intake signals collected |
|---|---|---|
| `bug_report.yml` | Reproducible defects | Component, severity, reproduction, expected behavior, environment, privacy check |
| `support_config.yml` | Setup, configuration, and usage help | Goal, observed behavior, redacted config or commands when relevant |
| `feature_request.yml` | Ordinary feature ideas | User problem, proposed solution, non-goals, architecture/risk hints, expected routing |
| `rfc_design.yml` | Proposals crossing an RFC trigger in §8: security model, governance or contribution process, cross-cutting ownership refactor, or a new subsystem or capability boundary | Trigger crossed, problem, proposal, risks, breaking-change assessment, decision/revisit surface |
| `roadmap_tracker.yml` | Active release, roadmap, RFC, implementation, cleanup, or audit trackers | Purpose, scope, linked work, routing evidence, close criteria, stale-exemption request |
| `docs_issue.yml` | Missing, wrong, confusing, or outdated docs | Location, problem, expected documentation, related source of truth |
| `contributor_task.yml` | Maintainer-scoped work intended for external contributors | Context, acceptance criteria, likely files, pickup fit, mentor or review contact |

Security vulnerabilities do not get a public issue template. `config.yml` links to the private security policy, Discord, GitHub Discussions, the contribution guide, the RFC process, and the maintainer PR workflow so contributors can choose the right surface before creating a tracked issue.

Issue templates collect evidence; they do not decide final labels by themselves. Maintainers still apply judgment-only labels such as `status:accepted`, `status:no-stale`, `help wanted`, and `good first issue` after checking the body, discussion, and linked work. In particular, `status:no-stale` should not be applied automatically from a template. A tracker, RFC, or long-lived accepted issue must record both the stale-exemption reason and the visible next decision or revisit surface before stale protection is added or kept.

---

## 8. The RFC Governance Loop

The RFC process was established in the documentation RFC and the architecture RFC. This section defines the close loop: how an RFC moves from proposal to decision to action.

**When an RFC is required.** An RFC records a durable project-level decision before implementation. Require one when the proposal is at least one of:

- a new security layer, or a material change to the project's security model;
- a governance, contribution-process, or project-authority change;
- a cross-cutting architectural refactor that changes ownership or contracts across established boundaries; or
- a new subsystem, or another project-wide capability boundary.

Do not require an RFC merely because the work includes an ordinary feature addition, a schema or data migration, a configuration field or default change, or a bounded implementation refactor. Those proceed through an issue and a PR. They require an RFC only when their substantive effect also meets one of the triggers above.

The trigger follows substantive project effect, not the issue title, the author, an AI-assisted origin, or the mere presence of a migration, feature, or default change. Security vulnerabilities use private reporting, never a public RFC.

Maintainers may relabel or close a filed RFC as an ordinary issue, feature request, or implementation follow-up when it does not meet the trigger. The disposition states whether the underlying work remains valid and where it continues. This routes work; it is not a rejection on substance.

### 8.1 The Full RFC Lifecycle

Ordinary author revisions and clarifications during discussion do not restart the clock. A revision that materially changes the proposed decision establishes a new stable snapshot, identified publicly, and restarts the applicable minimum discussion period.

```
1. AUTHOR opens an RFC issue using the RFC issue template,
   naming the trigger the proposal crosses
           |
2. DISCUSSION PERIOD, against a visible proposal
     minimum 48 hours for an ordinary RFC
     minimum 72 hours when the exceptional unanimous path is requested
   Anyone can comment. Core Team members engage substantively.
           |
3. VOTE OPENS once the period has elapsed and the proposal is stable.
   The vote-opening comment records:
     - the immutable proposal snapshot (artifact, commit, or issue-body digest)
     - the assigned active electorate, and inactive Core notified for re-entry
     - the threshold, and why it applies
     - that quorum requires two explicit ballots
     - the exact UTC deadline, 72 hours after opening
           |
4. CORE TEAM BALLOTS, one of:
     APPROVE  accept the snapshot as written
     REVISE   request changes, withhold approval, do not veto
     REJECT   blocking objection, with a specific reason
   A member's latest ballot before the deadline supersedes their earlier one.
           |
5. OUTCOME, applied in this precedence order:
     a. Fewer than two explicit ballots        -> DEFERRED
     b. Quorum met and any final ballot REJECT -> REJECTED
     c. Quorum met, no REJECT, two-thirds
        approving explicitly or by silence     -> ACCEPTED
     d. Otherwise                              -> RETURNED TO DISCUSSION
```

Accepted RFCs carry `status:accepted`, and the closing record addresses every `REVISE` concern rather than discarding it. Rejected RFCs are closed with the blocking objection recorded and a link to any issue where the underlying problem continues; rejection ends the current proposal, not necessarily the problem. Deferred proposals stay open with the condition for another vote recorded, and an unchanged deferred proposal may return to a new 72-hour vote without repeating discussion.

Use the live `type:rfc` and `status:accepted` labels. There is no parallel `rfc:*` status label family.

Rev. 15 applies to RFC votes opened after ratification. It does not automatically invalidate earlier accepted RFCs; historical-process audit and correction work remain tracked separately.

A vote may close early only when every member of the final active electorate has explicitly approved and no otherwise inactive Core contributor has asked for the full window. The closing record must say why it closed before the deadline. An exceptional unanimous vote may close early only on explicit approval from every assigned voter.

### 8.2 Vote Thresholds

**Two-thirds of the final active electorate is the default threshold**, rounded up to a whole voter. The final active electorate is the electorate assigned at opening plus any other current Core Team member who ballots in that same vote.

- **Quorum** requires at least two current Core contributors to cast an explicit ballot. Silence never counts toward quorum.
- **Silence counts as `APPROVE`** from the final active electorate once quorum is met, for ordinary votes only.
- **`REVISE`** counts as non-approval and does not veto.
- **`REJECT`** vetoes acceptance once quorum is met.

For example, with four members in the final active electorate, one explicit `APPROVE`, one explicit `REVISE`, and two silent members produce three approvals out of four, which meets the threshold.

**Unanimity is reserved** for decisions whose cost or irreversibility makes supermajority approval inadequate, such as license or legal-ownership changes. The vote opening must explain why unanimity applies. A unanimous vote requires an explicit `APPROVE` from every assigned eligible Core contributor; silence cannot establish unanimity.

**Active electorate.** An active Core contributor is a current Core Team member who cast an explicit `APPROVE`, `REVISE`, or `REJECT` ballot in a formally opened RFC vote during the preceding 30 days, and who has not publicly stepped away or recorded unavailability for the voting period. Inactive current Core members are notified and may join a vote's final electorate by balloting in it, which also reactivates them for later votes.

Quorum and the denominator are determined separately for every vote. Activity is checked when the vote opens; later activity in a different concurrent vote does not change an already-open vote's electorate.

### 8.2a Core Meeting Decisions and the GitHub Bridge

GitHub is the source of truth for proposal text, discussion, vote openings, ballots, deadlines, and outcomes. Discord may announce or discuss an RFC but does not establish governance state.

Core contributor meeting decisions recorded in the project's approved internal decision record may guide immediate maintainer action, and may supersede prior internal direction. Any such action that changes public project state must leave a GitHub bridge record on the affected issue, PR, tracker, or RFC. The bridge record names the meeting date or decision record, summarizes the decision applied, states the public action taken, and says whether it is a one-off exception or a durable rule change.

Meeting decisions do not silently rewrite this document, contributor docs, labels, issue templates, or RFC outcomes. Durable governance changes become policy only when reflected in the relevant GitHub and documentation surfaces. For exceptional unanimous decisions, an internal meeting record cannot replace the required explicit GitHub approvals unless that record documents the approving members and the public issue records that basis.

### 8.3 Durable Follow-Through and the ADR Connection

For newly accepted RFCs, the final shape and durable follow-through must be visible from the RFC issue before implementation proceeds. Acceptance alone does not complete the governance handoff. For accepted RFCs audited after implementation, record the disposition retrospectively without reopening completed work.

Each disposition record identifies the authoritative final shape, the selected disposition and rationale, the durable artifact or delivery tracker, and the owner or next action when follow-through remains.

Use one of four dispositions:

- **ADR:** required when the decision materially constrains future architecture. Indicators include a surprising system boundary, a non-obvious tradeoff, or a choice that materially limits future architecture alternatives.
- **Standing-document update:** required when the durable result is an operational, reference, workflow, security, or user contract rather than a new architecture decision.
- **Implementation or tracker follow-up:** required when an existing ADR, FND, or standing document already carries the decision and delivery work remains. Link the delivery tracker and its next action.
- **No separate artifact:** permitted when an identified existing FND, ADR, standing document, completed implementation, or superseding decision already preserves the result and no additional delivery tracking remains. The issue must record that rationale and link the durable surface.

An RFC is the discussion and acceptance surface. An ADR is the permanent record of a significant architecture decision, not a mandatory summary of every accepted RFC. Standing documents and implementation trackers do not replace an ADR when the accepted decision meets the architecture threshold above.

### 8.4 Foundational RFCs

The early proposal documents have since been represented as RFC issues
and foundation documents:

| RFC issue | Current durable surface | Priority |
|---|---|---|
| [#5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574) | [FND-001: Intentional architecture](./fnd-001-intentional-architecture.md) | High |
| [#5576](https://github.com/zeroclaw-labs/zeroclaw/issues/5576) | [FND-002: Documentation standards](./fnd-002-documentation-standards.md) | High |
| [#5577](https://github.com/zeroclaw-labs/zeroclaw/issues/5577) | [FND-003: Governance](./fnd-003-governance.md) | Medium |

---

## 9. Label Taxonomy

Labels are the metadata layer on issues and PRs. A consistent, well-designed label system makes filtering, reporting, and automation possible. An inconsistent label system (the common case, labels added ad hoc by whoever creates an issue) creates noise.

Use a **namespaced** label system. Each label has a prefix that identifies its category:

### `type:` What kind of work is this?

| Label | Color | Use |
|---|---|---|
| `type:feature` | `#0075ca` Blue | New capability or enhancement |
| `type:bug` | `#d73a4a` Red | Something is not working correctly |
| `type:refactor` | `#e4e669` Yellow | Code restructuring without behavior change |
| `type:docs` | `#0075ca` Blue | Documentation changes only |
| `type:security` | `#e11d48` Dark red | Security-related changes |
| `type:infrastructure` | `#6366f1` Purple | CI, tooling, build system |
| `type:adr` | `#a855f7` Light purple | Architecture Decision Record |
| `type:rfc` | `#f59e0b` Amber | Request for Comments / proposal |

### `priority:` How urgent is this?

| Label | Color | Use |
|---|---|---|
| `priority:critical` | `#b91c1c` Dark red | Blocking release or causing data loss |
| `priority:high` | `#f97316` Orange | Important, should be in next milestone |
| `priority:medium` | `#eab308` Yellow | Normal priority |
| `priority:low` | `#22c55e` Green | Nice to have, low urgency |

### `size:` How large is this work item?

| Label | Color | Use |
|---|---|---|
| `size:XS` | `#dcfce7` Light green | Under 2 hours |
| `size:S` | `#bbf7d0` Green | Half a day |
| `size:M` | `#86efac` Medium green | 1–3 days |
| `size:L` | `#4ade80` Dark green | 1–2 weeks |
| `size:XL` | `#16a34a` Deep green | More than 2 weeks; should be broken down |

### `component:` Which part of the system?

`component:kernel` · `component:gateway` · `component:channels` · `component:tools` · `component:memory` · `component:security` · `component:hardware` · `component:docs` · `component:infra`

Use `#f1f5f9` (light gray) for all component labels to distinguish them visually from other categories.

### `risk:` What is the risk tier? (mirrors `AGENTS.md`)

| Label | Color | Use |
|---|---|---|
| `risk:low` | `#dcfce7` | Docs, tests, minor changes |
| `risk:medium` | `#fef9c3` | Most `src/**` changes |
| `risk:high` | `#fee2e2` | Security, gateway, runtime, CI |

### `status:` Where is this in the process?

This table records governance intent and historical taxonomy shape. For current live label semantics and automation behavior, use the maintainer label guide as the operational reference; maintainer docs carry later label-policy corrections from #6808.

| Label | Color | Use |
|---|---|---|
| `status:needs-triage` | `#f8fafc` White | Newly opened, not yet reviewed |
| `status:accepted` | `#0e8a16` Green | RFC or work item ratified; not stale-exempt by itself |
| `status:blocked` | `#b60205` Red | Waiting on a recorded unresolved external dependency, maintainer decision, or linked prerequisite |
| `status:in-progress` | `#0075ca` Blue | Open PR is actively targeting the issue; verify live PR state during stale passes |
| `status:stale` | `#e4e669` Yellow | Issue is in the response window defined by the [maintainer label guide](../maintainers/labels.md#issue-stale-policy) |
| `status:no-stale` | `#0e8a16` Green | Explicit stale exemption for accepted or otherwise long-lived work; target policy requires a recorded reason and visible routing evidence in the operational source |
| `status:help-wanted` | `#059669` Green | Looking for a contributor |
| `status:good-first-issue` | `#059669` Green | Suitable for new contributors |
| `status:discussion` | `#a78bfa` Purple | Needs team discussion before work begins |

The live community-pickup labels are the unprefixed `good first issue` and `help wanted`; the `status:*` pickup rows above are historical taxonomy. Current operational risk labels also distinguish issue risk (likely fix blast radius from the report) from PR risk (the actual diff under review). See the [maintainer label guide](../maintainers/labels.md) for the live policy.

Terminal closure labels are operational policy, not part of the historical `status:*` taxonomy in this foundation document. Use the [maintainer label guide](../maintainers/labels.md#resolution-labels) for current resolution labels and the [superseding guide](../maintainers/superseding.md) for replacement-process rules.

### `rfc:` RFC-specific status

Retired in Rev. 15 and never created as live labels. RFC state uses the live `type:rfc` and `status:accepted` labels; see §8.1.

---

## 10. Definition of Done

**"Done" means something specific. If you do not define it, everyone will have a different definition, and the disagreements will surface at the worst possible time: during review, during release, or after a user files a bug.**

An item is **Done** when all of the following are true:

### For code changes

- [ ] The PR has been reviewed and approved by the required reviewer tier (per CODEOWNERS and risk level)
- [ ] All CI checks pass: `cargo fmt`, `cargo clippy`, `cargo test`
- [ ] Tests exist for the new or changed behavior (unit tests at minimum; integration tests for user-facing features)
- [ ] No test coverage that was passing before the PR was lost
- [ ] The PR description explains *what* changed and *why* (not just "fixed bug": what bug, what was wrong, what was changed)
- [ ] If the change affects user-facing behavior: the relevant reference documentation is updated in the same PR
- [ ] If the change is significant: a CHANGELOG.md entry is added under the correct milestone section
- [ ] If the change requires an ADR: the ADR is written, linked, and merged before or with the implementation PR

### For documentation changes

- [ ] YAML frontmatter is present and valid
- [ ] All internal links resolve correctly
- [ ] If the document describes a current behavior: it is accurate against the current `master` branch
- [ ] If the document is an ADR: it follows the Nygard format and has a `status` field

### For releases

- [ ] All items in the milestone are in `Done` status or explicitly moved to the next milestone with a comment explaining why
- [ ] The CHANGELOG.md entry for the release is complete
- [ ] Every accepted RFC in this milestone has a recorded durable disposition; required ADRs and standing-document updates are merged, and remaining delivery trackers are linked
- [ ] The release has been tested on at least one platform (Linux x86_64 at minimum)
- [ ] The release tag follows Semantic Versioning

### The "Done Done" rule

There is a concept in software teams of work that is "done" but not "done done." Done means the code is written. Done done means it is tested, documented, reviewed, merged, and released. The Definition of Done above describes done done. Nothing should be called done until it meets the full definition.

---

## 11. Automation

GitHub Projects v2 and GitHub Actions together enable significant automation that reduces manual coordination overhead. Here is what to implement, ordered by value-to-effort ratio.

### 11.1 Project Board Automation (Built-in, No Actions Required)

Configure these in the Project's built-in automation settings:

| Trigger | Action |
|---|---|
| Issue opened | Add to Project; set Status = 💡 Idea |
| Issue labeled `type:bug` | Set Priority = 🟠 High (if no priority set) |
| PR opened that references an issue | Set linked issue Status = 👀 In Review |
| PR merged | Set linked issue Status = ✅ Done; close linked issue |
| Issue closed as not planned | Set Status = 🚫 Won't Do |

### 11.2 GitHub Actions Workflows

**Auto-label by changed files:**

The active path labeler applies scope labels to PRs based on changed files. Risk and size labels are currently maintainer-applied; the maintainer label guide is the live source for label names, automation status, and risk semantics.

**Auto-request CODEOWNERS review (built into CODEOWNERS: no Action needed):**

GitHub enforces CODEOWNERS automatically when the file exists and branch protection requires it. No Action required.

**Stale issue management (maintainer-run):**

No GitHub Actions stale workflow is currently configured in the repository. Maintainers run stale passes to prevent inactive issues from accumulating while preserving a defined response window for the affected community. The [issue stale policy](../maintainers/labels.md#issue-stale-policy) is the sole operational source for timing, qualifying activity, exclusions, and re-engagement; the issue-triage protocol carries only the execution mechanics.

**PR size labeling (future/optional):**

If size automation is added later, it should follow the maintainer label guide's live names (`size:XS` through `size:XL`) and recalculate on pushed updates so the label describes the diff under review. Until then, size labels are maintainer-applied.

**Milestone check on PR merge (`.github/workflows/milestone-check.yml`):**

Warn (not block) if a PR is merged without a linked issue that has a milestone assigned. This is a gentle nudge, not a hard gate: the goal is to prevent work from happening without being tracked to a release.

### 11.3 What NOT to Automate Yet

- **Automated release drafts:** GitHub's release-drafter is useful but adds configuration overhead. Add it after the team has established a stable release rhythm.
- **Automated dependency updates (Dependabot PRs):** Enable Dependabot security updates (free, low noise), but defer automated version bumps until the team has CI stability. Bumping versions creates noise before the CI foundation is solid.
- **Sprint planning automation:** Do not automate sprint planning. It requires human judgment about capacity, priority, and team context that no automation can replace at this team size.

---

## 12. Phased Rollout

Governance and tooling must be introduced incrementally. Introducing everything at once creates overhead before the team understands why each piece exists.

---

### Phase 1 · This Week: "Foundations"

The minimum viable governance setup. Gets the team coordinating immediately.

- [ ] Create the GitHub Project with Status, Type, Priority, and Milestone fields
- [ ] Create the four Project views (Roadmap, Board, Backlog, My Work)
- [ ] Enable GitHub Discussions with maintained categories documented in the contributor communication and maintainer workflow docs
- [ ] Create the three RFC issues for the existing proposals (Section 8.4)
- [ ] Add the issue templates listed in Section 7
- [ ] Create the `CODEOWNERS` file (Section 6.1)
- [ ] Enable branch protection rules on `master` (Section 6.2)
- [ ] Add the remaining label taxonomy (Section 9) to the repository
- [ ] Pin the three RFC issues and the next release milestone issue

**Success signal:** New issues automatically appear in the Project. The team knows where to look for active work and where to post ideas.

---

### Phase 2 · v0.7.0 Milestone: "The Pipeline"

Establish the full workflow and populate the backlog from the accepted RFCs.

- [ ] Add Size, Risk Tier, and Component fields to the Project
- [ ] Populate the Backlog with deliverables from the microkernel architecture RFC
- [ ] Populate the Backlog with deliverables from the documentation standards RFC
- [ ] Conduct the first formal RFC votes on the three existing proposals
- [ ] Complete the selected foundational ADR set (ADR-001 through ADR-007 per the docs RFC)
- [ ] Implement the auto-label by path Actions workflow
- [ ] Implement the stale issue management workflow
- [x] Create the Core Team GitHub team, shipped as a single `core-contributors` team rather than the two originally planned. The `CONTRIBUTORS.md` roster item that sat alongside it is retired; see §5.3.

**Success signal:** The team is using the board daily. Items move through stages with visible gate checks. The RFC for the microkernel architecture has a recorded vote outcome.

---

### Phase 3 · v0.8.0 Milestone: "Growing the Community"

As the plugin system becomes usable, external contributors will start arriving. The contribution infrastructure must be ready.

- [ ] Implement the PR size labeling workflow
- [ ] Create the first batch of `good first issue` items (minimum 5) for the plugin SDK work
- [ ] Add the `Good First Issue Index` as a pinned issue with links to current good first issues
- [ ] Establish the idea promotion threshold and promote the first Discussion idea to an issue
- [ ] Document the Core Team expansion process: criteria for inviting new Core Team members

**Success signal:** At least one external contributor (not on the current team) submits a PR via a good first issue. The Discussions Ideas category has active community participation.

---

### Phase 4 · v1.0.0: "Sustainable Governance"

By v1.0.0, the governance model should be self-sustaining: the team should not need to think about it, it should just work.

- [ ] Review and update the governance document based on what has worked and what has not
- [ ] Establish the release cadence (how often are releases cut, who cuts them)
- [ ] Publish the plugin registry governance document (per the architecture RFC)
- [ ] Consider introducing time-boxed cycles (two or four weeks) if milestone-only planning feels too loose
- [ ] Document the process for a Core Team member to step down or become inactive

**Success signal:** The last six months of development history shows consistent use of the pipeline. Issues are triaged within 3 days. PRs are reviewed within 5 days. The CHANGELOG is updated on every merge.

---

## Appendix A: Glossary

**Backlog grooming**: A regular team activity (typically weekly or bi-weekly) in which the team reviews the backlog, reprioritizes items, closes stale ones, and ensures that the top items are "Defined" and ready to be picked up.

**Branch protection**: A GitHub feature that prevents direct pushes to protected branches and enforces requirements (reviews, CI checks) before merging.

**CODEOWNERS**: A GitHub file that automatically requests reviews from specified individuals or teams when files they own are changed in a PR.

**Definition of Done**: A shared checklist that specifies exactly what "done" means for a work item. Without a shared definition, "done" means something different to everyone.

**Lazy consensus**: A decision-making approach in which a proposed action proceeds unless someone objects within a defined time period. Reduces the overhead of requiring explicit approval for routine decisions.

**Meritocracy**: A governance model in which authority and influence are earned through demonstrated contribution, not through seniority or title. Standard in open source projects.

**Milestone**: A GitHub feature that groups issues and PRs by release target. A milestone represents a version of the software.

**T-shirt sizing**: An estimation technique that uses abstract sizes (XS, S, M, L, XL) rather than numeric story points. Easier to use without historical calibration data and sufficient for teams at an early stage.

**Triage**: The process of reviewing new issues to confirm they are valid, assign labels and priority, link them to milestones, and determine whether they belong in the backlog or should be closed.

---

## Appendix B: Further Reading

- [GitHub Projects documentation](https://docs.github.com/en/issues/planning-and-tracking-with-projects): Complete reference for GitHub Projects v2 features.
- [GitHub Discussions documentation](https://docs.github.com/en/discussions): Setup guide and governance options for GitHub Discussions.
- [CODEOWNERS syntax reference](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners): The full syntax for CODEOWNERS files.
- **"Producing Open Source Software"**: Karl Fogel: The definitive book on running an open source project. Free online at [producingoss.com](https://producingoss.com). Chapters on governance, contributor management, and communication are directly applicable.
- **"An Introduction to Open Source Governance Models"**: The Apache Software Foundation's governance documentation is a good model for how a mature open source project formalizes authority and decision-making: <https://www.apache.org/foundation/governance/>
- **Vale prose linter**: [Vale](https://vale.sh): Referenced in the documentation RFC; integrates with the `good first issue` documentation improvement workflow.

---

*This proposal was developed in the context of ZeroClaw v0.6.8 and the two preceding architecture and documentation RFCs. The governance model proposed here is intentionally lightweight for a student-led project at an early stage of community growth. It is designed to scale: adding process as the team grows, not all at once.*

*The best governance model is the simplest one the team will actually follow. Start here. Adjust based on what you learn.*
