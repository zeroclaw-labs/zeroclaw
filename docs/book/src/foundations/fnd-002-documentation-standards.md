# FND-002: Intentional Documentation — Standards, Structure, and i18n Strategy
### Starting v0.7.0 · Type: Documentation · Rev. 1

> **Canonical reference** · Ratified by the team · Rev. 1
> Discussion thread and full revision history: [#5576](https://github.com/zeroclaw-labs/zeroclaw/issues/5576)


---

> **A note to the team before you read this.**
>
> Documentation is not what you write after the code is done. It is a product surface in its own right — the interface between the project and every person who will ever contribute to it, use it, or build on it. A codebase with no documentation forces every new person to rediscover everything from scratch. A codebase with bad documentation is often worse, because it gives people false confidence. This RFC proposes treating documentation with the same intentionality we are applying to the architecture: Vision first, then structure, then content.

---

## Table of Contents

1. [The Documentation Philosophy](#1-the-documentation-philosophy)
2. [Honest Assessment: Where We Are Today](#2-honest-assessment-where-we-are-today)
3. [A Classification Framework: EA Artifacts on a Page](#3-a-classification-framework-ea-artifacts-on-a-page)
4. [The i18n Problem](#4-the-i18n-problem)
5. [The Repo / Wiki Split](#5-the-repo--wiki-split)
6. [ADR Standards](#6-adr-standards)
7. [AGENTS.md as the AI Development Layer](#7-agentsmd-as-the-ai-development-layer)
8. [The Target Structure](#8-the-target-structure)
9. [The Replacement docs-contract](#9-the-replacement-docs-contract)
10. [Standards We Should Adopt](#10-standards-we-should-adopt)
11. [Phased Roadmap](#11-phased-roadmap)

---

## 1. The Documentation Philosophy

Documentation problems almost always come from skipping a question that should have been asked before writing the first sentence: **what kind of document is this, and who is it for?**

Without an answer to that question, documentation accumulates as a pile of pages that are all slightly different shapes of the same vague category: "stuff about the project." Setup guides live next to architecture decisions. User-facing how-tos sit alongside internal coding standards. Thirty language translations of the README compete for space with the single security policy document. Nobody can find anything, everything goes stale at a different rate, and every PR that touches documentation becomes a negotiation about which pages need updating.

The fix is not to write more documentation. The fix is to decide, before writing anything, what type of artifact you are creating. Type determines format, audience, location, lifecycle, and who is responsible for keeping it current. Once type is established, the rest follows naturally.

This RFC adopts the **EA Artifacts on a Page** framework by Svyatoslav Kotusev (https://eaonapage.com) as the classification lens for all ZeroClaw documentation. The framework is evidence-based, deliberately non-prescriptive, and maps directly onto the kinds of documents an open source infrastructure project actually needs.

The core principle, borrowed from the broader development philosophy this team is adopting:

> **Documents, like code, should trace a line upward through Vision → Architecture → Design → Implementation. If you cannot name the artifact type and its audience before writing, you are not ready to write.**

---

## 2. Honest Assessment: Where We Are Today

### 2.1 The i18n Footprint

The most immediately measurable problem in the current documentation is the localization system:

| Metric | Value |
|---|---|
| Non-English README files at repo root | 31 |
| Files in `docs/i18n/` | 169 |
| Disk space consumed by `docs/i18n/` | 2.2 MB |
| Actively "supported" locales per `docs-contract.md` | 6 (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`) |
| Locales with README files at root | 31 |

The i18n system creates a **contributor tax on every documentation PR**. The current `docs-contract.md` contains this requirement:

> *If a change touches docs IA, runtime-contract references, or user-facing wording in shared docs, perform i18n follow-through for supported locales in the same PR.*

This means a contributor fixing a typo in a setup guide must update up to six language versions of that document, or the PR fails review. This is a significant barrier to contribution, particularly for the students and early-career engineers who make up most of this project's contributor base.

### 2.2 The Structure Problem

The current `docs/` hierarchy mixes three fundamentally different document types at the same level:

- **Code-adjacent documents** that must version with the codebase (ADRs, API specs, security policy, contribution process)
- **User-facing operational documents** that should update independently of code releases (setup guides, troubleshooting, deployment how-tos)
- **Community documents** that should be community-maintained and need no formal review process (translations, FAQ, community guides)

All three live in `docs/` with no structural distinction between them. The result is a flat pile with a hand-maintained `SUMMARY.md` that someone has to update every time anything changes.

### 2.3 The ADR Gap

The project has exactly one Architecture Decision Record: `ADR-004-tool-shared-state-ownership.md`. It is excellent — well-structured, code-referenced, specific. But the project has made at least five or six architectural decisions of equal or greater consequence that have never been recorded:

- The choice of Rust over TypeScript
- The trait-driven extensibility model
- The WASM plugin system design
- The choice of SQLite and Markdown as the two memory backends
- The security model (pairing codes, autonomy levels, sandbox layers)

Without these records, every new contributor must rediscover the reasoning through code archaeology. Every AI coding assistant that reads the codebase gets the *what* but not the *why*. This is one of the most expensive forms of undocumented technical debt.

### 2.4 What Is Already Good

The `docs-contract.md` concept — treating documentation as a governed product surface — is the right instinct. It just needs the right rules. The `AGENTS.md` at the root is excellent and sets the right precedent for AI-assisted development. ADR-004 proves the team can write high-quality architectural records.

---

## 3. A Classification Framework: EA Artifacts on a Page

The **EA Artifacts on a Page** framework defines five families of architecture artifacts. Every document in the ZeroClaw repository should belong to one of these families, and that family determines everything about where it lives, how it is formatted, and when it becomes stale.

| EA Artifact Family | The Question It Answers | Examples in ZeroClaw | Location |
|---|---|---|---|
| **Considerations** | What principles and standards guide our decisions? | `AGENTS.md` files, coding standards, security policy, this doc | `docs/book/src/contributing/` or per-crate |
| **Landscapes** | What does the system look like right now? | Component maps, crate topology, dependency diagrams | `docs/book/src/architecture/` |
| **Outlines** | Where are we going? | RFCs and roadmap proposals | GitHub Issues with `type:rfc` |
| **Designs** | How exactly are we doing this specific thing? | ADRs, OpenAPI specs, WIT interface files | `docs/book/src/architecture/` (ADR section) |
| **Standards** | What are the specific rules for how we build? | PR workflow, testing standards, release process | `docs/book/src/contributing/` and `docs/book/src/maintainers/` |

**What is notably absent from this table:** user guides, setup instructions, channel-specific how-tos, troubleshooting, FAQ. These are **operational content**, not EA artifacts. They do not version with the code. They belong on the GitHub Wiki.

### Using the Framework

Before writing any document, ask and answer these two questions:

1. **What artifact family is this?** If you cannot answer this, you are not ready to write.
2. **Does it need to version with the code?** If yes, it goes in the repository. If no, it goes on the Wiki.

A useful test for the second question: *would this document become wrong or misleading if someone read it against a different version of the codebase?* If yes, it lives in the repo, versioned with the code. If no, it lives on the Wiki.

---

## 4. The i18n Problem

### 4.1 The Argument for Removal

The case for removing all non-English content from the repository rests on four pillars:

**1. The audience has on-demand translation.** ZeroClaw's primary users are people running an AI assistant. Every such person has access to instant, high-quality machine translation — either through the agent they are running, through their browser, or through any of dozens of free translation services. The practical benefit of shipping translations in the repository is marginal.

**2. The translations are almost certainly stale.** Machine-translated content was likely generated once and has not been kept synchronised with the English source. Stale documentation is worse than no documentation for AI-assisted development, because language models will confidently derive incorrect conclusions from outdated information.

**3. The contributor tax is real and measurable.** The `docs-contract.md` parity requirement means every documentation PR must touch up to six language versions. This makes documentation contributions expensive and discourages exactly the kind of small, incremental improvements (fixing a typo, clarifying a step, updating a stale reference) that keep documentation healthy.

**4. Localization is community work, not core project work.** The communities best positioned to maintain Japanese documentation are Japanese-speaking contributors. Putting localized content in the main repository with a parity requirement places the burden on the core maintainers instead of the communities who benefit. The GitHub Wiki inverts this correctly: community members can edit and maintain their language's pages without opening PRs.

### 4.2 What Stays

One thing worth preserving: the _structure_ of the i18n approach. The idea of making ZeroClaw accessible in multiple languages is right. Only the _location_ and _ownership model_ is wrong.

### 4.3 The Replacement Strategy

1. **Remove** all `README.*.md` files from the repository root, except `README.md`
2. **Remove** `docs/i18n/` entirely
3. **Remove** all non-English hub files from `docs/` (e.g. `docs/README.zh-CN.md`)
4. **Add** a `Languages` section to the main `README.md`:

   > **Translations:** Community-maintained translations are available in the [GitHub Wiki](https://github.com/zeroclaw-labs/zeroclaw/wiki). To contribute a translation or improve an existing one, edit the Wiki directly. All languages are welcome.

5. **Create** a `Translations` page on the GitHub Wiki with a table of available languages, their completeness, and the contributors maintaining them
6. **Optionally:** add a `zeroclaw docs --translate` CLI feature that uses the configured LLM provider to translate any doc page on demand — a natural fit for a product whose entire purpose is AI assistance

### 4.4 The AGENTS.md Impact

Remove the i18n follow-through requirement from `docs-contract.md`. Replace it with: *Documentation PRs are reviewed in English only. Translations are community-maintained on the Wiki and are not subject to PR review.*

---

## 5. The Repo / Wiki Split

### 5.1 The Decision Rule

> **A document lives in the repository if it would become wrong when the code changes. It lives on the Wiki if it would not.**

This is not a fuzzy rule. Apply it literally.

An ADR records why a specific architectural decision was made at a specific point in time. If the code changes, the ADR still accurately describes what was decided and when. The code may have evolved away from it, but the record remains accurate. → **Repository.**

A setup guide for configuring the Telegram channel describes steps a user takes against the current version of the software. If the configuration format changes, the guide becomes wrong. → **This sounds like it should be in the repo — but it shouldn't.** Setup guides should update on their own timeline, not be coupled to code commits. The right model is: the API reference (which maps directly to configuration structs) lives in the repo, and the setup guide that walks a user through using that API lives on the Wiki, updated by anyone when the steps change.

### 5.2 The Split in Practice

**Stays in the repository (`docs/book/src/`):**

| Current location | Artifact family | Notes |
|---|---|---|
| `docs/book/src/architecture/` | Landscapes + Designs | Component diagrams, ADRs, crate topology |
| `docs/book/src/contributing/` | Considerations + Standards | PR workflow, testing, coding standards |
| `docs/book/src/maintainers/` | Considerations + Standards | Release runbook, reviewer playbook, label policy |
| `docs/book/src/security/` | Considerations + Designs | Security policy, sandboxing design, audit logging |
| `docs/book/src/hardware/` | Designs | Peripheral design docs, datasheets |
| `docs/book/src/reference/config.md` | Designs | Config reference (generated from code) |
| `docs/book/src/reference/cli.md` | Designs | CLI reference (generated from code) |
| `docs/book/src/foundations/` | Considerations | Ratified RFCs that shape everything else |

**Moves to the GitHub Wiki (proposed; not yet executed):**

| Current location | Reason for moving |
|---|---|
| `docs/book/src/setup/` | User-facing how-tos that change independently of code |
| `docs/book/src/ops/service.md` | Operational, user-maintained |
| `docs/book/src/ops/troubleshooting.md` | Operational, changes frequently |
| `docs/book/src/ops/network-deployment.md` | Operational, deployment-specific |
| Per-channel setup pages under `docs/book/src/channels/` | User-facing, change with upstream platform APIs |

**Deleted (i18n removal):**

| Item | Size impact |
|---|---|
| `docs/i18n/` (169 files) | −2.2 MB from repo |
| 31 × `README.*.md` at root | −significant root clutter |
| Non-English hub files in `docs/` | −31 files |
| i18n coverage map, i18n index | −2 files |

### 5.3 The Wiki Structure

```
Home
│
├── Getting Started
│     ├── Installation
│     ├── Quick Start (TL;DR)
│     ├── Migrating from OpenClaw
│     └── Onboarding Walkthrough
│
├── Configuration
│     ├── Providers
│     ├── Channels
│     ├── Memory
│     ├── Security & Pairing
│     └── Tunnels
│
├── Channels
│     ├── Telegram
│     ├── Discord
│     ├── Slack
│     ├── WhatsApp
│     └── ... (one page per channel)
│
├── Operations
│     ├── Troubleshooting
│     ├── Deployment
│     ├── Network Setup
│     └── Performance Tuning
│
├── Hardware
│     ├── Getting Started with Peripherals
│     ├── ESP32 Setup
│     ├── STM32 Nucleo Setup
│     └── Arduino Setup
│
└── Community
      ├── FAQ
      ├── Translations
      └── How to Contribute
```

---

## 6. ADR Standards

### 6.1 The Format

All Architecture Decision Records use the **Nygard format**, extended with YAML frontmatter for machine readability. ADR-004 is the existing model — this section formalizes it.

Every ADR has three sections and five frontmatter fields:

```
---
id: ADR-NNN
title: Short imperative sentence describing the decision
date: YYYY-MM-DD
status: proposed | accepted | deprecated | superseded-by-ADR-NNN
relates-to:
  - ADR-XXX (optional, list of related decisions)
  - crates/zeroclaw-api (optional, affected code paths)
---

# ADR-NNN: Title

## Context

What is the situation, constraint, or problem that required a decision?
What forces were at play? What options were considered?

## Decision

What was decided? State it in the active voice.
"We will..." not "It was decided that..."

## Consequences

What are the results of this decision?
List both positive consequences and negative ones — every decision has tradeoffs.
Note any follow-up decisions or actions this creates.

## References

Links to the relevant code files, issues, and external resources.
```

### 6.2 ADR Lifecycle Rules

- **ADRs are immutable once accepted.** If a decision changes, the old ADR is marked `superseded-by-ADR-NNN` and a new ADR is written describing the new decision and why it superseded the old one.
- **ADRs are numbered sequentially and never renumbered.** Gaps in the sequence are acceptable (a proposed ADR that was rejected can be withdrawn, leaving a gap).
- **ADRs live in `docs/architecture/decisions/`.** They are named `ADR-NNN-short-slug.md`.
- **Significant architectural changes require an ADR.** "Significant" means: a decision that would be surprising to a new contributor, a decision that constrains future choices, or a decision that involves a non-obvious tradeoff.

### 6.3 Retroactive ADRs

The following key decisions should be documented retroactively. They represent the foundational reasoning a new contributor or AI tool needs to understand the codebase:

| Proposed ADR | Decision to record |
|---|---|
| ADR-001 | Rust as the implementation language (replacing TypeScript/OpenClaw) |
| ADR-002 | Trait-driven extensibility as the primary architectural pattern |
| ADR-003 | WASM + Extism as the plugin execution model |
| ADR-004 | Tool shared state ownership contract *(already exists)* |
| ADR-005 | SQLite + Markdown as the two memory backends |
| ADR-006 | CLI as the only built-in channel; all others as plugins |
| ADR-007 | Gateway extraction as a separate optional binary |

Retroactive ADRs should be marked with a note:

> *This is a retroactive record of a decision made prior to the formal ADR process. The date reflects when the decision was made, not when this record was written.*

### 6.4 Why This Matters for AI-Assisted Development

When an AI coding assistant reads a repository, it sees the code as it is now. It does not see the choices that were rejected, the tradeoffs that were weighed, or the reasons a particular structure was chosen over alternatives. Without ADRs, the AI will suggest changes that violate architectural constraints it has no way of knowing about. With ADRs, the reasoning is explicit and machine-readable. The frontmatter makes ADRs queryable: an AI tool can find all ADRs related to `zeroclaw-api` and load them as context before editing that crate.

---

## 7. AGENTS.md as the AI Development Layer

### 7.1 The Pattern

The root `AGENTS.md` is the project's strongest existing contribution to AI-assisted development. It tells AI coding assistants the commands to run, the architecture to respect, the risk tiers to apply, and the anti-patterns to avoid. It works because it is specific, opinionated, and short.

As the workspace decomposes into crates (per the microkernel architecture RFC), each crate should have its own `AGENTS.md`. This is the mechanism by which architectural boundaries become enforceable at the AI-assistance layer — not just at compile time through crate dependencies, but at the reasoning layer before any code is written.

### 7.2 What Each Crate AGENTS.md Contains

Keep them short. An `AGENTS.md` that is longer than 60 lines will not be read. Each file answers five questions:

```markdown
# <crate-name>

## What this crate is
One or two sentences. What problem does this crate solve?

## What this crate is allowed to depend on
List the crates this crate may import. Be explicit.
If a dependency is not listed here, do not add it without an ADR.

## Extension points
Where can new implementations be added? What trait do they implement?
Link to the relevant traits.

## What does NOT belong here
Explicit anti-patterns. What would be a mistake to add to this crate?

## Related ADRs
- ADR-NNN: Short title
```

### 7.3 Examples

**For `crates/zeroclaw-api` (once extracted):**

```markdown
# zeroclaw-api

## What this crate is
Trait definitions and shared data types for the ZeroClaw plugin and kernel
interfaces. This is the contract layer. Everything else depends on it.

## What this crate is allowed to depend on
- serde, serde_json (serialization)
- async-trait (async trait support)
- anyhow (error types)
- tokio (async runtime types, minimal)
Nothing else. No HTTP clients. No database drivers. No external services.

## Extension points
All traits in this crate are extension points:
- `Provider` (src/providers/traits.rs) — LLM provider implementations
- `Channel` (src/channels/traits.rs) — messaging platform integrations
- `Tool` (src/tools/traits.rs) — agent tool implementations
- `Memory` (src/memory/traits.rs) — persistence backends
- `Observer` (src/observability/traits.rs) — observability backends
- `RuntimeAdapter` (src/runtime/traits.rs) — execution environments
- `Peripheral` (src/peripherals/traits.rs) — hardware integrations

## What does NOT belong here
- Any concrete implementation of any trait
- Any dependency on a specific messaging platform, LLM provider, or database
- Any network I/O or filesystem access
- Any binary or executable target

## Related ADRs
- ADR-002: Trait-driven extensibility
```

**For `crates/zeroclaw-kernel` (once extracted):**

```markdown
# zeroclaw-kernel

## What this crate is
The orchestration engine. Runs the agent loop, manages the service registry,
exposes the local IPC API. The kernel knows nothing about specific channels,
providers, or tools — only their abstract interfaces.

## What this crate is allowed to depend on
- zeroclaw-api (traits only)
- zeroclaw-tool-call-parser (parsing, no agent state)
- Standard async/runtime crates (tokio, anyhow, tracing)
- Config and storage crates (toml, serde, rusqlite for core memory)
NOT: any specific channel, provider, or tool implementation crate.

## Extension points
- `Registry::register_channel()` — add a channel at startup
- `Registry::register_tool()` — add a tool at startup
- `Registry::set_provider()` — set the active provider at startup
Implementations are registered by the binary crate, not by the kernel.

## What does NOT belong here
- Any import of TelegramChannel, DiscordChannel, or any named channel
- Any import of AnthropicProvider, OpenAIProvider, or any named provider
- Any tool implementation beyond the 10-12 designated core tools
- The gateway HTTP server or any web serving code

## Related ADRs
- ADR-002: Trait-driven extensibility
- ADR-006: CLI as the only built-in channel
- ADR-007: Gateway extraction
```

### 7.4 The AGENTS.md Hierarchy

The root `AGENTS.md` sets project-wide policy. Crate-level `AGENTS.md` files narrow that policy for their specific scope. When an AI tool reads a file in `crates/zeroclaw-api/`, it should read both the root `AGENTS.md` (project policy) and `crates/zeroclaw-api/AGENTS.md` (crate policy). Crate policy is more specific and takes precedence where they conflict.

---

## 8. The Target Structure

After the changes proposed in this RFC, the repository's documentation layout becomes:

```
docs/
│
├── README.md                    ← Hub: links to wiki for user guides,
│                                  to proposals/ for roadmap, to
│                                  architecture/ for decisions
├── SUMMARY.md                   ← Canonical TOC (English only, repo docs only)
│
├── architecture/
│   ├── README.md                ← Overview: what decisions have been made,
│   │                              current system landscape
│   ├── decisions/               ← ADRs (immutable once accepted)
│   │   ├── ADR-001-rust-first.md
│   │   ├── ADR-002-trait-driven-extensibility.md
│   │   ├── ADR-003-wasm-plugin-model.md
│   │   ├── ADR-004-tool-shared-state-ownership.md  (already exists)
│   │   ├── ADR-005-memory-backends.md
│   │   ├── ADR-006-cli-only-built-in-channel.md
│   │   └── ADR-007-gateway-extraction.md
│   └── diagrams/
│       ├── component-map.md     ← Mermaid: crate topology
│       └── data-flow.md         ← Mermaid: message lifecycle
│
├── proposals/                   ← RFCs (living until accepted/rejected)
│   ├── microkernel-architecture.md    (already exists)
│   └── documentation-standards.md    (this document)
│
├── contributing/
│   ├── README.md
│   ├── docs-contract.md         ← Replaced (see Section 9)
│   ├── pr-workflow.md
│   ├── reviewer-playbook.md
│   ├── ci-map.md
│   ├── actions-source-policy.md
│   ├── testing.md
│   ├── extension-examples.md
│   ├── change-playbooks.md
│   └── pr-discipline.md
│
├── reference/
│   ├── README.md
│   ├── api/
│   │   ├── config-reference.md
│   │   ├── providers-reference.md
│   │   └── channels-reference.md
│   └── cli/
│       └── commands-reference.md
│
├── security/
│   ├── README.md
│   ├── agnostic-security.md
│   ├── frictionless-security.md
│   ├── sandboxing.md
│   ├── audit-logging.md
│   └── security-roadmap.md
│
└── hardware/
    ├── README.md
    ├── hardware-peripherals-design.md
    ├── adding-boards-and-tools.md
    └── datasheets/
        ├── nucleo-f401re.md
        ├── arduino-uno.md
        └── esp32.md
```

**Deleted from current structure:**

```
docs/i18n/                       ← 169 files, 2.2 MB — removed entirely
docs/maintainers/                ← project snapshots and i18n coverage maps
                                   moved to Wiki (operational, not code-adjacent)
docs/setup-guides/               ← moved to Wiki
docs/ops/                        ← moved to Wiki
README.ar.md (and 30 others)     ← removed from repo root
docs/README.ar.md (and 30 others)← removed
```

The root of the repository becomes clean:

```
README.md
AGENTS.md
CHANGELOG.md
CLAUDE.md
CODE_OF_CONDUCT.md
CONTRIBUTING.md
SECURITY.md
LICENSE-APACHE
LICENSE-MIT
NOTICE
Cargo.toml
Cargo.lock
... (build and config files)
```

No language variants. No duplicated READMEs. One authoritative English README that links to the Wiki for user guides and the docs/ tree for technical reference.

---

## 9. The Replacement docs-contract

The legacy `docs/contributing/docs-contract.md` encoded an i18n parity requirement and a directory structure that this RFC supersedes. It has been removed; this section is its replacement.

The replacement governs three things: artifact classification, the repo/wiki split, and ADR governance. It says nothing about i18n — locale parity is now handled by the [Maintainers → Docs & Translations](../maintainers/docs-and-translations.md) page.

**Replacement docs-contract:**

```markdown
# Documentation Contract

## Document Classification

Every document in `docs/` belongs to one artifact family:

- **Considerations** — principles and standards that guide decisions
- **Landscapes** — descriptions of the current system state
- **Outlines** — proposals and roadmaps for future work
- **Designs** — ADRs, API specs, and detailed technical decisions
- **Standards** — specific rules for how we build and operate

If you cannot name the family before writing, do not write yet.

## The Repo / Wiki Rule

A document lives in the repository if it would become wrong when the
code changes. It lives on the Wiki if it would not.

Reference documentation (config reference, CLI reference) lives in the
repository because it maps directly to code structures.

User guides, setup instructions, and operational how-tos live on the Wiki
because they update on their own timeline.

## ADR Governance

See docs/architecture/decisions/ for the ADR format and lifecycle rules.

Major architectural changes require an ADR before implementation begins,
not after.

## Language

All documents in this repository are written in English.
Community-maintained translations live on the GitHub Wiki.
Documentation PRs are reviewed in English only.

## Freshness

Documents should be updated in the same PR as the code change that makes
them stale. A PR that changes a configuration format must update the
config reference. A PR that adds a new command must update the CLI reference.

Proposals in docs/proposals/ are exempt — they describe intent and may
precede implementation by multiple releases.
```

---

## 10. Standards We Should Adopt

These documentation-specific standards complement the broader standards proposed in the architecture RFC.

### Diátaxis Framework (Documentation Structure)

**What it is:** Diátaxis (https://diataxis.fr) is a systematic framework for technical documentation that divides content into four types: tutorials, how-to guides, reference, and explanation. It is the documentation framework behind the Python documentation, Django docs, and many others. It is highly compatible with the EA Artifacts approach — they answer different questions (Diátaxis: how to structure the content of a document; EA Artifacts: what type of document is this and where does it live).

**How it applies:** User-facing documentation on the Wiki should follow Diátaxis structure. Code-adjacent documentation in the repository follows EA Artifacts. The two frameworks operate at different levels and do not conflict.

| Diátaxis Type | Purpose | Example in ZeroClaw | Location |
|---|---|---|---|
| **Tutorial** | Learning-oriented, leads through an experience | "Build your first tool plugin" | Wiki |
| **How-to Guide** | Goal-oriented, solves a specific problem | "Set up Telegram integration" | Wiki |
| **Reference** | Information-oriented, describes the machinery | Config reference, CLI reference | Repo |
| **Explanation** | Understanding-oriented, explains why | ADRs, architecture docs | Repo |

### Markdown Frontmatter for Machine Readability

All documents in `docs/` should include YAML frontmatter. This makes them queryable by AI tools, CI checks, and future tooling:

```yaml
---
type: adr | proposal | reference | contributing | security | hardware
status: draft | proposed | accepted | deprecated | superseded
last-reviewed: YYYY-MM-DD
relates-to:
  - ADR-NNN
  - crates/zeroclaw-api
---
```

A CI check should verify that all documents in `docs/` have valid frontmatter. This prevents documents from being written without first declaring their type and status — enforcing the classification discipline at the tooling level.

### CommonMark + GitHub Flavored Markdown

All documentation uses CommonMark (the standardized Markdown specification) with GitHub Flavored Markdown extensions (tables, task lists, fenced code blocks, Mermaid diagrams). No custom extensions, no MDX, no ReStructuredText. Mermaid diagrams are preferred over image files for architecture diagrams because they version cleanly with the code.

### Vale for Prose Linting

**What it is:** Vale (https://vale.sh) is a prose linter — it checks writing style, consistency, and readability using configurable rules. It can enforce things like: always use "you" not "the user", avoid passive voice in imperative sections, use consistent terminology ("plugin" not "extension" not "module").

**Why it matters:** The current documentation is inconsistent in tone, terminology, and style. Some pages say "plugin", some say "module", some say "extension". Vale makes these rules automatic and enforces them at CI time, the same way Clippy enforces code quality.

---

## 11. Phased Roadmap

The documentation migration follows the same Strangler Fig pattern as the architecture migration: incremental, always in a working state, no big-bang rewrites.

---

### Phase 1 · v0.7.0 — "Clean the Root"

**Deliverables:**

- [ ] Remove all `README.*.md` files from the repo root (keep only `README.md`)
- [ ] Remove `docs/i18n/` entirely
- [ ] Remove all non-English hub files from `docs/`
- [ ] Add the `Languages` section to `README.md` with Wiki link
- [ ] Create the GitHub Wiki with the structural skeleton (Home + top-level pages, content stubs)
- [ ] Remove the i18n parity requirement from `docs-contract.md`
- [ ] Add YAML frontmatter to all existing `docs/` files
- [ ] Create `docs/architecture/decisions/` directory and move ADR-004 into it as `ADR-004-tool-shared-state-ownership.md`

**Success metrics:**
- Repo root contains exactly one README file
- `docs/i18n/` does not exist
- All `docs/` files have valid YAML frontmatter (CI-enforced)
- GitHub Wiki is live and publicly linked from README

---

### Phase 2 · v0.7.0–v0.8.0 — "Write the Missing ADRs"

**Deliverables:**

- [ ] Write ADR-001 through ADR-003 and ADR-005 through ADR-007 (retroactive, see Section 6.3)
- [ ] Add a Vale configuration (`.vale.ini` + style rules) and CI check
- [ ] Replace `docs-contract.md` in full with the version specified in Section 9
- [ ] Migrate `docs/setup-guides/` content to the GitHub Wiki
- [ ] Migrate `docs/ops/` content to the GitHub Wiki
- [ ] Update `SUMMARY.md` to reflect the new structure (repo-only content)
- [ ] Write root-level `AGENTS.md` for `crates/zeroclaw-api` (in anticipation of extraction)

**Success metrics:**
- ADR-001 through ADR-007 exist and are accepted
- Vale CI check passes on all docs
- Wiki has complete content for all migrated sections
- No dead links in `docs/`

---

### Phase 3 · v0.8.0–v0.9.0 — "The AI Layer"

**Deliverables:**

- [ ] Write `AGENTS.md` for each new crate as the workspace decomposes (per architecture RFC phases)
- [ ] Write `docs/architecture/diagrams/component-map.md` (Mermaid, reflects target crate topology)
- [ ] Write `docs/architecture/diagrams/data-flow.md` (Mermaid, message lifecycle)
- [ ] Write the plugin SDK documentation in `docs/book/src/developing/plugin-sdk.md`
- [ ] Write the WIT interface documentation alongside the `wit/` files (generated from WIT + hand-written explanation)
- [ ] Update the OpenAPI spec documentation as the kernel IPC API stabilizes

**Success metrics:**
- Every crate in the workspace has an `AGENTS.md`
- Architecture diagrams are Mermaid (no binary image files in docs/)
- Plugin SDK documentation is sufficient for an external contributor to write a working tool plugin

---

### Phase 4 · v1.0.0 — "The Stable Platform"

**Deliverables:**

- [ ] Mark ADR-001 through ADR-007 as `accepted` (not `proposed`) once the corresponding code is shipped
- [ ] Version the kernel IPC API documentation at `v1` with a stability guarantee
- [ ] Write the Plugin Registry governance document (who controls the registry, how plugins are reviewed, how compromised plugins are revoked)
- [ ] Publish the plugin SDK as a standalone document site (from `docs/book/src/developing/plugin-sdk.md`)
- [ ] Establish the Wiki translation coordinator role (a community member who maintains the Translations page and coordinates volunteer translators)

**Success metrics:**
- All foundational ADRs are accepted
- Plugin SDK is complete and externally linked from the README
- Wiki has active community-maintained translations in at least two languages
- Documentation CI (frontmatter check + Vale) passes on every PR

---


## Appendix A: Glossary

**ADR (Architecture Decision Record)** — An immutable record of a significant architectural decision: the context that prompted it, what was decided, and the consequences. ADRs do not change once accepted; superseded decisions are recorded as new ADRs.

**Diátaxis** — A systematic framework for technical documentation structure that divides content into tutorials (learning), how-to guides (goal-oriented), reference (information), and explanation (understanding). See https://diataxis.fr.

**EA Artifacts on a Page** — A classification framework for enterprise architecture documents developed by Svyatoslav Kotusev. Classifies artifacts into five families: Considerations, Landscapes, Outlines, Designs, and Standards. See https://eaonapage.com.

**Frontmatter** — YAML metadata at the top of a Markdown file, delimited by `---`. Makes documents machine-readable and queryable by tools, CI checks, and AI assistants.

**Nygard Format** — The ADR format introduced by Michael Nygard: three sections (Context, Decision, Consequences) that capture the essential reasoning without unnecessary ceremony.

**Strangler Fig Pattern** — A migration strategy in which new structure is built incrementally around the old, replacing it piece by piece rather than all at once. The system remains functional throughout the migration.

**Vale** — A prose linter for technical documentation. Enforces style, consistency, and readability rules at CI time, the way Clippy enforces Rust code quality. See https://vale.sh.

---

## Appendix B: Further Reading

- **Diátaxis documentation framework** — https://diataxis.fr — The definitive reference for structuring technical documentation by type.
- **EA Artifacts on a Page (v2.2)** — https://eaonapage.com — The classification framework used in Section 3.
- **"Docs for Developers"** — Jared Bhatti et al. — A practical guide to technical documentation written by engineers who have maintained large documentation systems.
- **Vale documentation** — https://vale.sh/docs — Setup guide and configuration reference for the prose linter proposed in Section 10.
- **Michael Nygard on ADRs** — https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions — The original post that introduced the ADR format used in Section 6.
- **GitHub Wikis documentation** — https://docs.github.com/en/communities/documenting-your-project-with-wikis — Reference for setting up and governing the GitHub Wiki proposed in Section 5.

---

*This proposal was developed from direct analysis of the ZeroClaw documentation system at v0.6.8. The metrics cited (169 i18n files, 2.2 MB, 31 language README variants) are based on direct measurement. The recommendations reflect established practices in technical documentation for open source infrastructure projects, adapted to the specific constraints and goals of ZeroClaw.*

*Feedback, corrections, and counterproposals are welcome. Good documentation is a community effort, and the best structure is the one the team will actually maintain.*
