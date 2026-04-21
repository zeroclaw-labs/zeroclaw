# FND-003: Team Organization, Project Governance, and Contribution Pipeline
### Starting v0.7.0 · Type: Governance · Rev. 2

> **Canonical reference** · Ratified by the team · Rev. 2
> Discussion thread and full revision history: [#5577](https://github.com/zeroclaw-labs/zeroclaw/issues/5577)


---

> **A note to the team before you read this.**
>
> Software projects do not fail because the code is bad. They fail because the people writing the code cannot coordinate. Features get built twice. Bugs get lost. Good ideas evaporate because nobody wrote them down. New contributors show up wanting to help and cannot find where to start. This RFC is about building the lightweight scaffolding that prevents those failures — not so the project feels organized, but so the team can move faster, with more confidence, and with less friction. Every recommendation here is chosen specifically for a small, growing, student-led open source team. Nothing here requires a project manager, a Scrum Master, or a formal committee.

---

---

## Revision History

| Rev | Date | Summary |
|---|---|---|
| 1 | 2026-04-09 | Initial draft |
| 2 | 2026-04-09 | Added §6.4 Architectural Compliance: Human Review, AI Support; added Discussion Question on AI automation of architecture reviews |

---

## Table of Contents

1. [The Coordination Problem](#1-the-coordination-problem)
2. [The Three-Part System](#2-the-three-part-system)
3. [GitHub Projects: The Work Pipeline](#3-github-projects-the-work-pipeline)
4. [GitHub Discussions: The Ideas Parking Lot](#4-github-discussions-the-ideas-parking-lot)
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
- The team works reactively — whoever shouts loudest gets attention, whatever breaks gets fixed, nothing gets planned more than a week out
- Architectural decisions get made in PR comments and are never recorded anywhere

This is not a criticism of anyone's effort. It is a description of what happens by default. The solution is not more process — it is the right process, applied at the right level for the size and maturity of the team.

ZeroClaw needs three things:

1. **A pipeline** for turning ideas into shipped code, with visible stages and clear gates at each transition
2. **A parking lot** for capturing ideas that are not ready for the pipeline yet, without losing them or cluttering the active work
3. **A governance model** that defines who can decide what, how architectural decisions get made, and how the team grows

These are three distinct concerns. Conflating them — putting everything in one board, or relying on informal chat for decisions — is what creates the chaos the team is trying to escape.

---

## 2. The Three-Part System

| Concern | Tool | Why This Tool |
|---|---|---|
| Work pipeline (backlog → release) | **GitHub Projects v2** | Custom fields, multiple views, Kanban + roadmap, built-in automation, milestone tracking |
| Ideas parking lot | **GitHub Discussions** | Community-votable, no PR required, separates "maybe someday" from committed work, converts to issues when ready |
| Governance and decision authority | **RFC process + Team Tiers + CODEOWNERS** | Already partially established via `docs/proposals/`; needs formalization and close loop |

The key principle: **the Project board contains only work the team has committed to thinking about.** Ideas that have not been evaluated live in Discussions. Work that has been evaluated, accepted, and scoped lives in the Project. This distinction is what keeps the board useful.

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

### 3.2 The Gate Questions

Every transition has a gate question. The question must be answered "yes" before the item moves forward. This is the project board made operational — the Vision → Architecture → Design → Implementation → Testing → Documentation hierarchy becomes a checklist at each stage.

| Transition | Gate Question | Who Checks |
|---|---|---|
| Idea → Backlog | Does this align with the Vision statement? Does it fit the target architecture? | Core Team triage |
| Backlog → Defined | Is there a clear acceptance criteria? Does it need an ADR or design note? Is the risk tier assigned? | Assignee + reviewer |
| Defined → In Progress | Is there an assignee? Is it sized? Are the related ADRs or docs identified? | Assignee |
| In Progress → In Review | Do tests exist for the new behavior? Is CI passing? Is the PR description complete? | Author (self-check) |
| In Review → Done | Has the correct reviewer tier approved? Is documentation updated? Is the CHANGELOG entry written? | Reviewer |
| Any → Won't Do | Has the decision not to pursue been explained in the item's comments? | Core Team |

**Why explicit gates matter for a student team:** Without gates, cards move because someone feels done, not because done has a definition. This is the single most common source of "done" work that is not actually done. The gates make the definition visible and shared.

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

**View 1: Roadmap**
- Type: Roadmap (timeline)
- Grouped by: Milestone
- Visible fields: Title, Type, Size, Component, Assignee
- Purpose: Public-facing. "Here is what is coming and when." Share this link in the README and with the community. Keep it updated.

**View 2: Board**
- Type: Board (Kanban)
- Columns: Status field values
- Filtered to: Current milestone only
- Visible fields: Title, Assignee, Size, Risk Tier
- Purpose: Day-to-day work visibility. What is everyone working on right now? What is blocked?

**View 3: Backlog**
- Type: Table
- Sorted by: Priority (descending), then Size (ascending)
- Filtered to: Status = Backlog OR Defined
- Visible fields: Title, Type, Priority, Size, Component, Milestone, Risk Tier
- Purpose: Used during grooming sessions. What needs to be worked on next? What is sized and ready to pick up?

**View 4: My Work**
- Type: Board
- Filtered to: Assignee = @me
- Purpose: Personal dashboard. Each contributor can see their own items without noise.

### 3.5 Pinned Items

GitHub allows up to six pinned issues per repository. Use them for high-signal, always-visible communication:

1. The current active RFC under discussion
2. The most wanted community feature (highest-voted Discussion)
3. The next release milestone tracking issue
4. The good-first-issue index (an issue that links to all current `good-first-issue` items)

Pinned issues are a promise to the community: these are the things that matter most right now. Update them when priorities shift.

---

## 4. GitHub Discussions: The Ideas Parking Lot

### 4.1 Enable Discussions and Create Categories

Enable GitHub Discussions on the repository. Create the following categories:

| Category | Format | Purpose |
|---|---|---|
| 📣 **Announcements** | Announcement | Core team posts; community cannot create, only react and comment |
| 💡 **Ideas** | Open-ended discussion | Community feature requests and suggestions; the ideas parking lot |
| 🏗️ **Architecture** | Open-ended discussion | Questions and proposals about system design; feeds the RFC process |
| ❓ **Q&A** | Question/Answer | How do I do X? Answered by contributors and core team |
| 🐛 **Bug Reports (pre-triage)** | Open-ended discussion | For bug reports that need more information before becoming issues |
| 🌐 **Translations** | Open-ended discussion | Community-maintained translations and localization coordination |
| 🎉 **Show and Tell** | Open-ended discussion | What are you building with ZeroClaw? Community showcases |

### 4.2 The Idea-to-Issue Promotion Process

Discussions in the **Ideas** category follow a lightweight promotion process:

```
Community member posts an idea in Discussions → Ideas
         ↓
Community votes with 👍 reactions
         ↓
Threshold: 5 👍 from Contributors or Core Team members
         ↓
Core Team member converts to Issue using GitHub's
"Create issue from discussion" feature
         ↓
Issue enters Project board as 💡 Idea status
         ↓
Discussion is marked Answered with a link to the new issue
```

The threshold of five votes from Contributors or Core Team (not anonymous community members) prevents low-signal requests from flooding the backlog while still giving the community a real voice. Reactions from unaffiliated accounts count as community sentiment data but not toward the threshold.

**Why this process matters:** It separates signal from noise. Not every idea is a good idea, and not every good idea is the right idea for now. The Discussions layer lets the community express what they want without creating the false expectation that every request becomes a ticket.

### 4.3 Ideas That Should Not Wait for Votes

Some items bypass the Discussions promotion process and enter the backlog directly:

- Security vulnerabilities (via private security report, never public)
- Confirmed bugs with reproduction steps (go directly to Bug Report issue template)
- RFC-accepted architecture items (spawned directly from the RFC close loop)
- Items from the project roadmap (placed directly by Core Team)

### 4.4 The Architecture Category

The **Architecture** Discussions category is specifically for questions and proposals that are not yet ready to be formal RFCs. A contributor who notices a problem with the current design or wants to explore an idea can open an Architecture discussion before committing to writing a full RFC. This lowers the barrier to raising concerns.

If an Architecture discussion develops sufficient detail and consensus, a Core Team member can promote it to a formal RFC by moving the content to `docs/proposals/` and opening an RFC issue.

---

## 5. Team Tiers and Contribution Authority

### 5.1 The Three Tiers

Open source projects run on **meritocracy** — influence and authority come from demonstrated contribution, not from seniority, title, or who you know. This is one of the things that makes open source different from corporate software, and it is worth teaching explicitly.

The three tiers reflect increasing demonstrated commitment to the project:

---

**Tier 1: Community**

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

**Tier 2: Contributor**

Community members who have had at least two PRs merged into the `master` branch.

*How to become one:* Have two PRs merged. A Core Team member adds you to the Contributors team in GitHub and to `CONTRIBUTORS.md`.

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

**Tier 3: Core Team**

Contributors who have demonstrated consistent, high-quality contributions over time and have been invited by existing Core Team members.

*How to become one:* Invitation from existing Core Team members, announced publicly in Discussions. There is no formal threshold — it is a judgment call based on the quality, consistency, and alignment of past contributions.

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

For routine decisions — adding a label, closing a stale issue, updating documentation — Core Team members operate under **lazy consensus**: if you announce your intention in the relevant issue and no Core Team member objects within 48 hours, you proceed. This prevents the paralysis of requiring explicit approval for everything while maintaining visibility.

Lazy consensus does not apply to:
- RFC acceptance or rejection
- Releases
- Changes to CODEOWNERS or branch protection rules
- Changes to this governance document
- Additions to the Core Team

These always require explicit Core Team votes.

### 5.3 Recording Team Membership

Team membership is recorded in two places:

**`CONTRIBUTORS.md`** at the repository root — a public record of everyone who has contributed, organized by tier. Updated by Core Team members as contributors are recognized.

**GitHub Teams** in the organization settings — `zeroclaw-core` and `zeroclaw-contributors` teams, referenced in CODEOWNERS and used for notification routing.

---

## 6. CODEOWNERS and Branch Protection

### 6.1 CODEOWNERS

The `CODEOWNERS` file makes governance automatic. It defines which paths require review from which team before a PR can merge. GitHub enforces this as a required review — the PR cannot be merged until the requirement is satisfied.

Create `.github/CODEOWNERS`:

```
# CODEOWNERS — Automatic review routing by risk tier
# See AGENTS.md for risk tier definitions.
# See docs/proposals/project-governance.md for team tier definitions.

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

docs/proposals/**               @zeroclaw-labs/zeroclaw-core
docs/architecture/decisions/**  @zeroclaw-labs/zeroclaw-core
AGENTS.md                       @zeroclaw-labs/zeroclaw-core

# ── Default: any Contributor or Core Team member can review ─────────────────

*                               @zeroclaw-labs/zeroclaw-contributors
```

As specific Core Team members take ownership of components, add their individual handles alongside the team handle. Specificity wins in CODEOWNERS — a more specific path rule overrides a more general one.

### 6.2 Branch Protection Rules

Configure the following branch protection rules for `master`:

| Rule | Setting | Reason |
|---|---|---|
| Require a pull request before merging | Enabled | No direct pushes to master — ever |
| Require approvals | 1 for Low/Medium risk; 2 for High risk | CODEOWNERS enforcement handles the "who" |
| Require status checks to pass | `cargo fmt`, `cargo clippy`, `cargo test` | CI must be green before merge |
| Require branches to be up to date | Enabled | Prevents merging stale code |
| Require conversation resolution | Enabled | All review comments must be resolved |
| Do not allow bypassing the above settings | Enabled | Applies to everyone, including admins |
| Allow force pushes | Disabled | Preserve commit history |
| Allow deletions | Disabled | Protect the branch |

**Why admins cannot bypass:** One of the most common mistakes in small team projects is treating branch protection as "for other people." When an admin can bypass, they will — under time pressure, in an emergency, "just this once." Then it becomes the norm. The rule must apply to everyone for it to mean anything. If there is a genuine emergency, the right response is to follow the process faster, not to skip it.

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

This section exists because the question will come up — it already has — and it deserves a clear, documented answer rather than a debate on every PR.

**The question:** Should we add an automated gate that checks whether a PR conforms to the architecture and design patterns defined in the RFCs?

**The answer:** No. And understanding why is important.

---

**There are two fundamentally different kinds of quality enforcement, and they require different mechanisms.**

The first kind is *structural compliance*: does this code violate a mechanical rule? Does `zeroclaw-kernel` import `TelegramChannel`? Do the dependency graph edges point the wrong way? Are there clippy warnings? These are binary questions. Either the code violates the rule or it does not. The compiler, `cargo deny`, and `cargo clippy --workspace` already enforce this. No human is needed. No AI is needed. The machine is authoritative, fast, and never wrong about a factual violation.

The second kind is *architectural intent*: does this decision belong here? Is this abstraction at the right layer? Does this trade-off align with the vision? Is this coupling going to be painful in Phase 3? Will this PR create a maintenance burden that isn't visible in the diff today? These questions require judgment, context, and an understanding of *why* the architecture exists — not just what the rules are. No automated tool can answer them reliably, because the answer depends on information that is not in the diff: the roadmap, the team's current priorities, the contributor's intent, and the long-term cost of the decision.

**The failure modes of automating architectural judgment are both bad.**

A gate that passes subtle architectural violations creates false confidence. The developer sees ✅ and assumes their decision was validated. The most damaging architectural drift — the kind that takes years to untangle — looks structurally correct. It compiles. It passes lint. The dependency graph is fine. The problem is that it violated the spirit of the design in a way that only becomes apparent later, when the cost of unwinding it is high.

A gate that flags valid architectural decisions because the tool misread the context teaches developers to dismiss the gate entirely. Once a team learns to click past a noisy automated check, the check is gone in practice even if it is still running in CI. The project has spent CI minutes to achieve negative value.

**CODEOWNERS is the architectural compliance gate. The reviewer is the tool.**

The `CODEOWNERS` configuration in §6.1 already enforces that PRs touching high-risk paths — crate boundaries, trait definitions, the dependency graph, `src/security/`, `.github/` — require review from a Core Team member. That Core Team member, equipped with the RFCs as their reference framework, is the architectural compliance check. They bring the contextual judgment that no automation can replicate.

This is why the RFCs, the AGENTS.md files, and the documentation standards exist: not so a machine can parse them and produce a score, but so a human reviewer has a consistent, documented framework to apply. The RFC answers "why does this architecture exist." The reviewer answers "does this PR serve or undermine that why."

**AI belongs in the development loop, not the merge gate.**

AI tools — Claude, Copilot, Cursor, and whatever comes next — are genuinely useful for architectural work when they are used in the right place. The right place is *during development*, not *during the merge gate*.

During development, an AI assistant equipped with the RFC and the crate's AGENTS.md can help a contributor understand which crate a new piece of functionality belongs in before they write it, flag a potential dependency inversion while the code is still being shaped, explain why a design pattern exists, and suggest whether a new abstraction is at the right layer. This is additive. It makes contributors more capable.

During a review, an AI assistant can help a human reviewer draft structured feedback, cross-reference a change against the RFC, and identify which discussion questions in the RFC are relevant to the PR. This is also additive. The reviewer brings the judgment; the AI brings speed and recall.

What AI cannot do is replace the judgment. "AI helps me assess this PR" and "AI automatically gates this PR" are categorically different, and only the first one works for architectural decisions. The day the project routes architectural compliance through an automated gate — however sophisticated — is the day the architecture starts drifting in ways nobody notices until it is too late.

**The practical policy, stated plainly:**

- Structural compliance (import direction, dependency graph, lint, format) is enforced by CI. This is non-negotiable and automated.
- Architectural intent compliance is enforced by CODEOWNERS routing to a Core Team reviewer. This is non-negotiable and human.
- AI tools support contributors during development and support reviewers during review. They do not gate merges on their own authority.
- If the team wants to evaluate AI-assisted review tooling in the future, that evaluation goes through the RFC process first. It does not get added to `.github/workflows/` without a documented decision.

This policy is not a limitation on AI or on automation. It is a recognition that different problems require different tools, and using the right tool in the right place is exactly what the architecture RFC is asking of the codebase.

---

## 7. Issue Templates

Issue templates route incoming reports to the right process before they reach a human. A well-written template gathers the information needed for triage automatically. A missing or ignored template results in issues that take three comment exchanges to understand.

Create the following templates in `.github/ISSUE_TEMPLATE/`:

### Template 1: Bug Report (`bug_report.yml`)

```yaml
name: Bug Report
description: Something is not working as expected
labels: ["type:bug", "status:needs-triage"]
body:
  - type: markdown
    attributes:
      value: |
        Before submitting: search existing issues to avoid duplicates.
        For security vulnerabilities, use the private security report
        process described in SECURITY.md — do not open a public issue.
  - type: textarea
    id: description
    attributes:
      label: What happened?
      description: A clear description of the bug.
    validations:
      required: true
  - type: textarea
    id: reproduction
    attributes:
      label: Steps to reproduce
      description: The exact steps to reproduce the bug.
      placeholder: |
        1. Run `zeroclaw ...`
        2. With config `...`
        3. See error
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: What did you expect to happen?
    validations:
      required: true
  - type: textarea
    id: environment
    attributes:
      label: Environment
      placeholder: |
        - ZeroClaw version:
        - OS and version:
        - Rust version (if built from source):
        - Provider:
    validations:
      required: true
  - type: dropdown
    id: risk
    attributes:
      label: Does this bug have security implications?
      options:
        - "No"
        - "Yes — I have already filed a private security report"
        - "I am not sure"
    validations:
      required: true
```

### Template 2: Feature Request (`feature_request.yml`)

```yaml
name: Feature Request
description: Suggest a new capability or improvement
labels: ["type:feature", "status:needs-triage"]
body:
  - type: markdown
    attributes:
      value: |
        Feature requests that have been discussed and upvoted in GitHub
        Discussions → Ideas are more likely to be prioritized. Consider
        posting there first if you want community feedback before filing.
  - type: textarea
    id: problem
    attributes:
      label: What problem does this solve?
      description: Describe the problem or limitation you are experiencing.
    validations:
      required: true
  - type: textarea
    id: solution
    attributes:
      label: What would you like to happen?
      description: Describe the feature or change you are proposing.
    validations:
      required: true
  - type: textarea
    id: alternatives
    attributes:
      label: What alternatives have you considered?
      description: Other ways to solve the problem, including doing nothing.
  - type: dropdown
    id: component
    attributes:
      label: Which component does this affect?
      options:
        - Kernel / Agent Loop
        - Gateway / Web UI
        - Channels
        - Tools
        - Memory
        - Security
        - Hardware / Peripherals
        - Documentation
        - Infrastructure / CI
        - Not sure
    validations:
      required: true
```

### Template 3: RFC / Architecture Proposal (`rfc.yml`)

```yaml
name: RFC / Architecture Proposal
description: Propose a significant architectural or behavioral change
labels: ["type:rfc", "status:discussion"]
body:
  - type: markdown
    attributes:
      value: |
        RFCs are for significant changes that affect architecture, public
        interfaces, or project direction. Before filing an RFC issue:
        1. Write the proposal document and add it to `docs/proposals/`
        2. Open a PR with that document
        3. Then open this issue to start the formal discussion period

        For smaller changes, open a regular feature request or just a PR.
  - type: input
    id: proposal-pr
    attributes:
      label: Link to the proposal document PR
      placeholder: "https://github.com/zeroclaw-labs/zeroclaw/pull/..."
    validations:
      required: true
  - type: textarea
    id: summary
    attributes:
      label: One-paragraph summary
      description: What is being proposed and why?
    validations:
      required: true
  - type: textarea
    id: impact
    attributes:
      label: Impact and tradeoffs
      description: What does this change affect? What are the tradeoffs?
    validations:
      required: true
  - type: dropdown
    id: breaking
    attributes:
      label: Is this a breaking change?
      options:
        - "No"
        - "Yes — existing configurations or APIs change"
        - "Potentially — needs investigation"
    validations:
      required: true
```

### Template 4: Documentation Issue (`docs_issue.yml`)

```yaml
name: Documentation Issue
description: Something in the docs is missing, wrong, or confusing
labels: ["type:docs", "status:needs-triage"]
body:
  - type: input
    id: location
    attributes:
      label: Where is the documentation issue?
      placeholder: "URL or file path (e.g. docs/reference/api/config-reference.md)"
    validations:
      required: true
  - type: dropdown
    id: issue-type
    attributes:
      label: Type of issue
      options:
        - Missing documentation
        - Incorrect information
        - Confusing or unclear
        - Outdated (code has changed)
        - Broken link
    validations:
      required: true
  - type: textarea
    id: description
    attributes:
      label: Describe the problem
    validations:
      required: true
  - type: textarea
    id: suggestion
    attributes:
      label: Suggested improvement (optional)
      description: If you know what the correct content should be, share it here.
```

### Template 5: Security Report Redirect

Create `.github/ISSUE_TEMPLATE/security.md` as a redirect — GitHub will show it as a template option but the content redirects rather than creating an issue:

```yaml
name: Security Vulnerability
description: ⚠️ Do not use this template. See SECURITY.md for private reporting.
labels: []
body:
  - type: markdown
    attributes:
      value: |
        ## ⚠️ Do not report security vulnerabilities as public issues.

        Security vulnerabilities disclosed publicly before a fix is available
        put all ZeroClaw users at risk. Please follow the private disclosure
        process described in [SECURITY.md](../SECURITY.md).

        If you have already filed this as a public issue by mistake, please
        delete it and re-report privately. A Core Team member will contact
        you to confirm receipt.
```

### Template 6: Good First Issue (`good_first_issue.yml`)

```yaml
name: Good First Issue (Core Team only)
description: Tag an issue as a good entry point for new contributors
labels: ["good-first-issue", "status:needs-triage"]
body:
  - type: markdown
    attributes:
      value: |
        This template is for Core Team members identifying good entry
        points for new contributors. A good first issue must have:
        - A clear, self-contained scope (no cross-cutting changes)
        - Size of XS or S
        - Links to the relevant code files
        - A named mentor the new contributor can ping for help
  - type: textarea
    id: description
    attributes:
      label: What needs to be done?
      description: Be specific. Include file paths and function names where known.
    validations:
      required: true
  - type: textarea
    id: context
    attributes:
      label: Context and background
      description: What does a new contributor need to understand to work on this?
    validations:
      required: true
  - type: input
    id: mentor
    attributes:
      label: Mentor / point of contact
      placeholder: "@username"
    validations:
      required: true
  - type: textarea
    id: acceptance
    attributes:
      label: Acceptance criteria
      description: How will we know when this is done?
    validations:
      required: true
```

---

## 8. The RFC Governance Loop

The RFC process was established in the documentation RFC and the architecture RFC. This section defines the close loop — how an RFC moves from proposal to decision to action.

### 8.1 The Full RFC Lifecycle

```
1. AUTHOR writes proposal → docs/proposals/<slug>.md
           ↓
2. AUTHOR opens PR with the proposal document
           ↓
3. AUTHOR opens RFC issue using the RFC issue template
   linking to the PR
           ↓
4. DISCUSSION PERIOD — minimum 7 days
   Anyone can comment. Core Team members engage substantively.
   Discussions happen on the issue, not the PR.
           ↓
5. CORE TEAM VOTE on the issue
   Format: comment with one of:
     ✅ APPROVE — with brief rationale
     ❌ REJECT — with specific objections
     🔄 REVISE — with specific requests
           ↓
   ┌── Majority APPROVE ──────────────────────────────────────┐
   │  RFC is accepted                                          │
   │  PR is merged                                            │
   │  Issue labeled rfc:accepted                              │
   │  Author writes ADR(s) in docs/architecture/decisions/    │
   │  ADR issue(s) linked back to RFC issue                   │
   │  RFC issue closed                                        │
   └──────────────────────────────────────────────────────────┘
           ↓
   ┌── Any REJECT ────────────────────────────────────────────┐
   │  RFC is rejected                                          │
   │  PR is closed (not merged)                               │
   │  Issue labeled rfc:rejected                              │
   │  Rejecting members document specific objections          │
   │  RFC issue closed with rejection summary comment         │
   └──────────────────────────────────────────────────────────┘
           ↓
   ┌── REVISE requested ──────────────────────────────────────┐
   │  RFC is not voted on until revisions are complete        │
   │  Issue labeled rfc:revision-requested                    │
   │  Author revises proposal document                        │
   │  Author re-requests review via issue comment             │
   │  Process returns to step 4                               │
   └──────────────────────────────────────────────────────────┘
```

### 8.2 Vote Thresholds

| Change Type | Vote Required | Rationale |
|---|---|---|
| Documentation, tooling, non-breaking features | Simple majority of active Core Team members | Low stakes, fast iteration |
| API changes, new subsystems, behavioral changes | Two-thirds majority of Core Team | Moderate stakes, needs real consensus |
| Architecture changes, security model changes, breaking changes | Unanimous agreement of all Core Team members | High stakes, affects everyone |

"Active" Core Team members are those who have participated in at least one vote in the past 90 days. Inactive members do not count against majority thresholds but are notified of votes.

### 8.3 The ADR Connection

Every accepted RFC must produce at least one ADR before the corresponding implementation can begin. The ADR is not a summary of the RFC — it is the permanent record of the specific decision made, in the Nygard format defined in the documentation RFC. The RFC can be long and exploratory. The ADR is short and definitive.

RFCs are proposals. ADRs are decisions. Both are necessary. Neither replaces the other.

### 8.4 Existing RFCs in This Repository

The following RFCs have been filed as of this writing and should be converted to formal RFC issues immediately:

| RFC Document | Issue to create | Priority |
|---|---|---|
| `docs/proposals/microkernel-architecture.md` | Microkernel Architecture RFC (v0.7.0+) | High |
| `docs/proposals/documentation-standards.md` | Documentation Standards and i18n RFC | High |
| `docs/proposals/project-governance.md` | Team Organization and Governance RFC | Medium |

---

## 9. Label Taxonomy

Labels are the metadata layer on issues and PRs. A consistent, well-designed label system makes filtering, reporting, and automation possible. An inconsistent label system (the common case — labels added ad hoc by whoever creates an issue) creates noise.

Use a **namespaced** label system. Each label has a prefix that identifies its category:

### `type:` — What kind of work is this?

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

### `priority:` — How urgent is this?

| Label | Color | Use |
|---|---|---|
| `priority:critical` | `#b91c1c` Dark red | Blocking release or causing data loss |
| `priority:high` | `#f97316` Orange | Important, should be in next milestone |
| `priority:medium` | `#eab308` Yellow | Normal priority |
| `priority:low` | `#22c55e` Green | Nice to have, low urgency |

### `size:` — How large is this work item?

| Label | Color | Use |
|---|---|---|
| `size:xs` | `#dcfce7` Light green | Under 2 hours |
| `size:s` | `#bbf7d0` Green | Half a day |
| `size:m` | `#86efac` Medium green | 1–3 days |
| `size:l` | `#4ade80` Dark green | 1–2 weeks |
| `size:xl` | `#16a34a` Deep green | More than 2 weeks; should be broken down |

### `component:` — Which part of the system?

`component:kernel` · `component:gateway` · `component:channels` · `component:tools` · `component:memory` · `component:security` · `component:hardware` · `component:docs` · `component:infra`

Use `#f1f5f9` (light gray) for all component labels to distinguish them visually from other categories.

### `risk:` — What is the risk tier? (mirrors `AGENTS.md`)

| Label | Color | Use |
|---|---|---|
| `risk:low` | `#dcfce7` | Docs, tests, minor changes |
| `risk:medium` | `#fef9c3` | Most `src/**` changes |
| `risk:high` | `#fee2e2` | Security, gateway, runtime, CI |

### `status:` — Where is this in the process?

| Label | Color | Use |
|---|---|---|
| `status:needs-triage` | `#f8fafc` White | Newly opened, not yet reviewed |
| `status:blocked` | `#dc2626` Red | Waiting on something external |
| `status:help-wanted` | `#059669` Green | Looking for a contributor |
| `status:good-first-issue` | `#059669` Green | Suitable for new contributors |
| `status:discussion` | `#a78bfa` Purple | Needs team discussion before work begins |
| `status:wont-fix` | `#9ca3af` Gray | Explicitly decided not to pursue |
| `status:duplicate` | `#9ca3af` Gray | Duplicate of another issue |

### `rfc:` — RFC-specific status

`rfc:accepted` · `rfc:rejected` · `rfc:revision-requested`

---

## 10. Definition of Done

**"Done" means something specific. If you do not define it, everyone will have a different definition, and the disagreements will surface at the worst possible time — during review, during release, or after a user files a bug.**

An item is **Done** when all of the following are true:

### For code changes:

- [ ] The PR has been reviewed and approved by the required reviewer tier (per CODEOWNERS and risk level)
- [ ] All CI checks pass: `cargo fmt`, `cargo clippy`, `cargo test`
- [ ] Tests exist for the new or changed behavior (unit tests at minimum; integration tests for user-facing features)
- [ ] No test coverage that was passing before the PR was lost
- [ ] The PR description explains *what* changed and *why* (not just "fixed bug" — what bug, what was wrong, what was changed)
- [ ] If the change affects user-facing behavior: the relevant reference documentation is updated in the same PR
- [ ] If the change is significant: a CHANGELOG.md entry is added under the correct milestone section
- [ ] If the change requires an ADR: the ADR is written, linked, and merged before or with the implementation PR

### For documentation changes:

- [ ] YAML frontmatter is present and valid
- [ ] All internal links resolve correctly
- [ ] If the document describes a current behavior: it is accurate against the current `master` branch
- [ ] If the document is an ADR: it follows the Nygard format and has a `status` field

### For releases:

- [ ] All items in the milestone are in `Done` status or explicitly moved to the next milestone with a comment explaining why
- [ ] The CHANGELOG.md entry for the release is complete
- [ ] All ADRs spawned by accepted RFCs in this milestone are written and accepted
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

**Auto-label by changed files (`.github/workflows/label-by-path.yml`):**

Automatically add component and risk labels to PRs based on which files were changed. A PR touching `src/security/` gets `component:security` and `risk:high`. A PR touching `docs/` gets `type:docs` and `risk:low`. This eliminates the requirement for PR authors to remember to label their own PRs and gives reviewers immediate context.

**Auto-request CODEOWNERS review (built into CODEOWNERS — no Action needed):**

GitHub enforces CODEOWNERS automatically when the file exists and branch protection requires it. No Action required.

**Stale issue management (`.github/workflows/stale.yml`):**

Issues with no activity for 45 days are labeled `status:stale` and a comment is posted asking if the issue is still relevant. Issues with no activity for 15 days after the stale label is applied are closed. This prevents the backlog from accumulating hundreds of issues that are months old and no longer relevant. Exclude `status:blocked`, `priority:critical`, and `type:rfc` from the stale process.

**PR size labeling (`.github/workflows/pr-size.yml`):**

Automatically label PRs with `size:xs` through `size:xl` based on lines changed. This gives reviewers and maintainers an immediate sense of scope without opening the diff. Use these thresholds as a starting point: XS < 10 lines, S < 50, M < 250, L < 1000, XL ≥ 1000.

**Milestone check on PR merge (`.github/workflows/milestone-check.yml`):**

Warn (not block) if a PR is merged without a linked issue that has a milestone assigned. This is a gentle nudge, not a hard gate — the goal is to prevent work from happening without being tracked to a release.

### 11.3 What NOT to Automate Yet

- **Automated release drafts:** GitHub's release-drafter is useful but adds configuration overhead. Add it after the team has established a stable release rhythm.
- **Automated dependency updates (Dependabot PRs):** Enable Dependabot security updates (free, low noise), but defer automated version bumps until the team has CI stability. Bumping versions creates noise before the CI foundation is solid.
- **Sprint planning automation:** Do not automate sprint planning. It requires human judgment about capacity, priority, and team context that no automation can replace at this team size.

---

## 12. Phased Rollout

Governance and tooling must be introduced incrementally. Introducing everything at once creates overhead before the team understands why each piece exists.

---

### Phase 1 · This Week — "Foundations"

The minimum viable governance setup. Gets the team coordinating immediately.

- [ ] Create the GitHub Project with Status, Type, Priority, and Milestone fields
- [ ] Create the four Project views (Roadmap, Board, Backlog, My Work)
- [ ] Enable GitHub Discussions with the seven categories defined in Section 4.1
- [ ] Create the three RFC issues for the existing proposals (Section 8.4)
- [ ] Add the six issue templates (Section 7)
- [ ] Create the `CODEOWNERS` file (Section 6.1)
- [ ] Enable branch protection rules on `master` (Section 6.2)
- [ ] Add the remaining label taxonomy (Section 9) to the repository
- [ ] Pin the three RFC issues and the next release milestone issue

**Success signal:** New issues automatically appear in the Project. The team knows where to look for active work and where to post ideas.

---

### Phase 2 · v0.7.0 Milestone — "The Pipeline"

Establish the full workflow and populate the backlog from the accepted RFCs.

- [ ] Add Size, Risk Tier, and Component fields to the Project
- [ ] Populate the Backlog with deliverables from the microkernel architecture RFC
- [ ] Populate the Backlog with deliverables from the documentation standards RFC
- [ ] Conduct the first formal RFC votes on the three existing proposals
- [ ] Write ADRs for accepted RFCs (ADR-001 through ADR-007 per the docs RFC)
- [ ] Add the `CONTRIBUTORS.md` file with current team members in their tiers
- [ ] Implement the auto-label by path Actions workflow
- [ ] Implement the stale issue management workflow
- [ ] Create the `zeroclaw-core` and `zeroclaw-contributors` GitHub Teams

**Success signal:** The team is using the board daily. Items move through stages with visible gate checks. The RFC for the microkernel architecture has a recorded vote outcome.

---

### Phase 3 · v0.8.0 Milestone — "Growing the Community"

As the plugin system becomes usable, external contributors will start arriving. The contribution infrastructure must be ready.

- [ ] Implement the PR size labeling workflow
- [ ] Create the first batch of `good-first-issue` items (minimum 5) for the plugin SDK work
- [ ] Add the `Good First Issue Index` as a pinned issue with links to current good-first-issues
- [ ] Establish the idea promotion threshold and promote the first Discussion idea to an issue
- [ ] Document the Core Team expansion process — criteria for inviting new Core Team members

**Success signal:** At least one external contributor (not on the current team) submits a PR via a good-first-issue. The Discussions Ideas category has active community participation.

---

### Phase 4 · v1.0.0 — "Sustainable Governance"

By v1.0.0, the governance model should be self-sustaining — the team should not need to think about it, it should just work.

- [ ] Review and update the governance document based on what has worked and what has not
- [ ] Establish the release cadence (how often are releases cut, who cuts them)
- [ ] Publish the plugin registry governance document (per the architecture RFC)
- [ ] Consider introducing time-boxed cycles (two or four weeks) if milestone-only planning feels too loose
- [ ] Document the process for a Core Team member to step down or become inactive

**Success signal:** The last six months of development history shows consistent use of the pipeline. Issues are triaged within 3 days. PRs are reviewed within 5 days. The CHANGELOG is updated on every merge.

---


---

## Appendix A: Glossary

**Backlog grooming** — A regular team activity (typically weekly or bi-weekly) in which the team reviews the backlog, reprioritizes items, closes stale ones, and ensures that the top items are "Defined" and ready to be picked up.

**Branch protection** — A GitHub feature that prevents direct pushes to protected branches and enforces requirements (reviews, CI checks) before merging.

**CODEOWNERS** — A GitHub file that automatically requests reviews from specified individuals or teams when files they own are changed in a PR.

**Definition of Done** — A shared checklist that specifies exactly what "done" means for a work item. Without a shared definition, "done" means something different to everyone.

**Lazy consensus** — A decision-making approach in which a proposed action proceeds unless someone objects within a defined time period. Reduces the overhead of requiring explicit approval for routine decisions.

**Meritocracy** — A governance model in which authority and influence are earned through demonstrated contribution, not through seniority or title. Standard in open source projects.

**Milestone** — A GitHub feature that groups issues and PRs by release target. A milestone represents a version of the software.

**T-shirt sizing** — An estimation technique that uses abstract sizes (XS, S, M, L, XL) rather than numeric story points. Easier to use without historical calibration data and sufficient for teams at an early stage.

**Triage** — The process of reviewing new issues to confirm they are valid, assign labels and priority, link them to milestones, and determine whether they belong in the backlog or should be closed.

---

## Appendix B: Further Reading

- **GitHub Projects documentation** — https://docs.github.com/en/issues/planning-and-tracking-with-projects — Complete reference for GitHub Projects v2 features.
- **GitHub Discussions documentation** — https://docs.github.com/en/discussions — Setup guide and governance options for GitHub Discussions.
- **CODEOWNERS syntax reference** — https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-code-owners — The full syntax for CODEOWNERS files.
- **"Producing Open Source Software"** — Karl Fogel — The definitive book on running an open source project. Free online at https://producingoss.com. Chapters on governance, contributor management, and communication are directly applicable.
- **"An Introduction to Open Source Governance Models"** — The Apache Software Foundation's governance documentation is a good model for how a mature open source project formalizes authority and decision-making: https://www.apache.org/foundation/governance/
- **Vale prose linter** — https://vale.sh — Referenced in the documentation RFC; integrates with the `good-first-issue` documentation improvement workflow.

---

*This proposal was developed in the context of ZeroClaw v0.6.8 and the two preceding architecture and documentation RFCs. The governance model proposed here is intentionally lightweight for a student-led project at an early stage of community growth. It is designed to scale — adding process as the team grows, not all at once.*

*The best governance model is the simplest one the team will actually follow. Start here. Adjust based on what you learn.*
