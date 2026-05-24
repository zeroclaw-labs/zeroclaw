# Architecture and Contribution Map

Use this page when a change is larger than a typo and you are not sure which architecture, foundation, contributor, or maintainer documents apply.

This page is only a map. The linked files remain the source of truth.

## Start Here

1. Read the repo-root `AGENTS.md` first. It contains the current risk tiers, protected files, anti-patterns, localization rules, and agent-specific workflow contracts.
2. Read [How to contribute](./how-to.md) for the PR mechanics, validation expectations, and review process.
3. Use the tables below to choose the architecture and foundation documents that match the change.
4. If the change crosses subsystem, config, security, workflow, governance, or release boundaries, check the [RFC process](./rfcs.md) before implementing.

## Common Change Paths

| Change | Read first | Why |
|---|---|---|
| New provider | [Architecture overview](../architecture/overview.md), [Crates](../architecture/crates.md), [Custom providers](../providers/custom.md), [Provider configuration](../providers/configuration.md) | Providers are edge adapters behind the provider trait, with config and routing contracts. |
| New channel | [Architecture overview](../architecture/overview.md), [Crates](../architecture/crates.md), [Channels overview](../channels/overview.md), existing implementations in `crates/zeroclaw-channels/` | Channels are user-visible boundaries; validate both inbound and outbound behavior. |
| New tool or tool policy | [Tools overview](../tools/overview.md), [Plugin protocol](../developing/plugin-protocol.md), [Security overview](../security/overview.md), [Tool receipts](../security/tool-receipts.md) | Tools execute actions for the agent, so security, approval, audit, and receipts matter. |
| Runtime, agent loop, cron, SOP, memory, or streaming behavior | [Request lifecycle](../architecture/request-lifecycle.md), [Crates](../architecture/crates.md), [FND-001](../foundations/fnd-001-intentional-architecture.md), [Testing](./testing.md) | Runtime changes often affect multiple user paths and need boundary-level tests. |
| Gateway, web API, webhooks, or dashboard behavior | [Gateway HTTP API](../gateway/api.md), [Request lifecycle](../architecture/request-lifecycle.md), [Security overview](../security/overview.md), [Reviewer playbook](../maintainers/reviewer-playbook.md) | Gateway changes can affect auth, public exposure, pairing, webhooks, and review risk. |
| Config schema, environment variables, or defaults | [Environment variables](../reference/env-vars.md), [Provider configuration](../providers/configuration.md), [FND-001](../foundations/fnd-001-intentional-architecture.md), [RFC process](./rfcs.md) | Config changes affect upgrade paths and may require migration or RFC discussion. |
| CI, release, GitHub Actions, or allowed actions | [CI & Actions](../maintainers/ci-and-actions.md), [FND-004](../foundations/fnd-004-engineering-infrastructure.md), [PR workflow](../maintainers/pr-workflow.md) | Infrastructure changes are high-risk when they alter what code can run or ship. |
| Docs structure, contributor guidance, or knowledge organization | [FND-002](../foundations/fnd-002-documentation-standards.md), [Docs & Translations](../maintainers/docs-and-translations.md), this page | Documentation changes should reduce search cost and preserve the decision trail. |
| Governance, labels, board workflow, or contribution process | [FND-003](../foundations/fnd-003-governance.md), [RFC process](./rfcs.md), [Labels](../maintainers/labels.md), [Reviewer playbook](../maintainers/reviewer-playbook.md) | Process changes affect maintainers and contributors; keep them durable and explicit. |
| AI-assisted contribution, superseding, or review culture | [FND-005](../foundations/fnd-005-contribution-culture.md), [Superseding PRs](../maintainers/superseding.md), [PR review protocol](./pr-review-protocol.md) | AI-assisted work is welcome, but the human sponsor owns accuracy, attribution, and review response. |
| Production code health, error handling, or dead-code cleanup | [FND-006](../foundations/fnd-006-zero-compromise-in-practice.md), [Testing](./testing.md), repo-root `AGENTS.md` | Error discipline, unused code, and production readiness are review gates, not style preferences. |

## Foundation Documents In One Screen

| Foundation | Read when the change asks... |
|---|---|
| [FND-001: Intentional architecture](../foundations/fnd-001-intentional-architecture.md) | Does this fit the microkernel/runtime direction? Which layer should own it? |
| [FND-002: Documentation standards](../foundations/fnd-002-documentation-standards.md) | Where should knowledge live? How should docs stay navigable and durable? |
| [FND-003: Governance](../foundations/fnd-003-governance.md) | Who decides? Which labels, project board, or RFC process should carry the state? |
| [FND-004: Engineering infrastructure](../foundations/fnd-004-engineering-infrastructure.md) | How should CI, release automation, or GitHub Actions behave? |
| [FND-005: Contribution culture](../foundations/fnd-005-contribution-culture.md) | How should contributors, maintainers, and AI-assisted work communicate and review? |
| [FND-006: Zero compromise in practice](../foundations/fnd-006-zero-compromise-in-practice.md) | What quality bar applies to production code, errors, dead code, and release readiness? |

## Coding Agent Entry Points

Coding agents should use the same public docs as humans, plus the repository-local agent contracts.

- Follow the repo-root `AGENTS.md` and the matching in-repo skill listed there when one applies.
- Treat foundation documents as decision context. They explain why a review may ask for a split, an RFC, stronger validation, or a different owner.
- Keep private workflow mechanics out of public PR bodies, issue comments, and reviews. Public text should cite concrete behavior, source paths, commands, validation evidence, linked issues, and user-visible risk.
- If a generated or skill-authored draft conflicts with source code, current `AGENTS.md`, or a ratified foundation document, stop and reconcile before posting or implementing.

## RFC And PR Checkpoints

This map does not replace the [RFC process](./rfcs.md) or the PR template.
It exists to make architecture and contribution scope easier to find; see the discussion in [#6808](https://github.com/zeroclaw-labs/zeroclaw/issues/6808) for the context behind adding this routing page.

- Check or open an RFC first when the RFC page says the change is RFC-shaped: established default changes, breaking config or schema migration, new subsystem or protocol, cross-cutting refactor, governance, release, or contribution-model changes.
- If a change is ambiguous but not clearly RFC-shaped, ask a maintainer or narrow the PR before implementation.
- Before opening a PR, answer the template's summary, validation, compatibility, and rollback prompts. If those answers are not clear, write the design note or RFC first.
