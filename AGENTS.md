# AGENTS.md — ZeroClaw Agent Engineering Protocol

This file defines the default working protocol for coding agents in this repository.
Scope: entire repository.

## 1) Project Snapshot (Read First)

ZeroClaw is a Rust-first autonomous agent runtime optimized for:

- high performance
- high efficiency
- high stability
- high extensibility
- high sustainability
- high security

Core architecture is trait-driven and modular. Most extension work should be done by implementing traits and registering in factory modules.

Key extension points:

- `src/providers/traits.rs` (`Provider`)
- `src/channels/traits.rs` (`Channel`)
- `src/tools/traits.rs` (`Tool`)
- `src/memory/traits.rs` (`Memory`)
- `src/observability/traits.rs` (`Observer`)
- `src/runtime/traits.rs` (`RuntimeAdapter`)
- `src/peripherals/traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## 2) Deep Architecture Observations (Why This Protocol Exists)

These codebase realities should drive every design decision:

1. **Trait + factory architecture is the stability backbone**
    - Extension points are intentionally explicit and swappable.
    - Most features should be added via trait implementation + factory registration, not cross-cutting rewrites.
2. **Security-critical surfaces are first-class and internet-adjacent**
    - `src/gateway/`, `src/security/`, `src/tools/`, `src/runtime/` carry high blast radius.
    - Defaults already lean secure-by-default (pairing, bind safety, limits, secret handling); keep it that way.
3. **Performance and binary size are product goals, not nice-to-have**
    - `Cargo.toml` release profile and dependency choices optimize for size and determinism.
    - Convenience dependencies and broad abstractions can silently regress these goals.
4. **Config and runtime contracts are user-facing API**
    - `src/config/schema.rs` and CLI commands are effectively public interfaces.
    - Backward compatibility and explicit migration matter.
5. **The project now runs in high-concurrency collaboration mode**
    - CI + docs governance + label routing are part of the product delivery system.
    - PR throughput is a design constraint; not just a maintainer inconvenience.

## 3) Engineering Principles (Normative)

These principles are mandatory by default. They are not slogans; they are implementation constraints.

### 3.1 KISS (Keep It Simple, Stupid)

**Why here:** Runtime + security behavior must stay auditable under pressure.

Required:

- Prefer straightforward control flow over clever meta-programming.
- Prefer explicit match branches and typed structs over hidden dynamic behavior.
- Keep error paths obvious and localized.

### 3.2 YAGNI (You Aren't Gonna Need It)

**Why here:** Premature features increase attack surface and maintenance burden.

Required:

- Do not add new config keys, trait methods, feature flags, or workflow branches without a concrete accepted use case.
- Do not introduce speculative “future-proof” abstractions without at least one current caller.
- Keep unsupported paths explicit (error out) rather than adding partial fake support.

### 3.3 DRY + Rule of Three

**Why here:** Naive DRY can create brittle shared abstractions across providers/channels/tools.

Required:

- Duplicate small, local logic when it preserves clarity.
- Extract shared utilities only after repeated, stable patterns (rule-of-three).
- When extracting, preserve module boundaries and avoid hidden coupling.

### 3.4 SRP + ISP (Single Responsibility + Interface Segregation)

**Why here:** Trait-driven architecture already encodes subsystem boundaries.

Required:

- Keep each module focused on one concern.
- Extend behavior by implementing existing narrow traits whenever possible.
- Avoid fat interfaces and “god modules” that mix policy + transport + storage.

### 3.5 Fail Fast + Explicit Errors

**Why here:** Silent fallback in agent runtimes can create unsafe or costly behavior.

Required:

- Prefer explicit `bail!`/errors for unsupported or unsafe states.
- Never silently broaden permissions/capabilities.
- Document fallback behavior when fallback is intentional and safe.

### 3.6 Secure by Default + Least Privilege

**Why here:** Gateway/tools/runtime can execute actions with real-world side effects.

Required:

- Deny-by-default for access and exposure boundaries.
- Never log secrets, raw tokens, or sensitive payloads.
- Keep network/filesystem/shell scope as narrow as possible unless explicitly justified.

### 3.7 Determinism + Reproducibility

**Why here:** Reliable CI and low-latency triage depend on deterministic behavior.

Required:

- Prefer reproducible commands and locked dependency behavior in CI-sensitive paths.
- Keep tests deterministic (no flaky timing/network dependence without guardrails).
- Ensure local validation commands map to CI expectations.

### 3.8 Reversibility + Rollback-First Thinking

**Why here:** Fast recovery is mandatory under high PR volume.

Required:

- Keep changes easy to revert (small scope, clear blast radius).
- For risky changes, define rollback path before merge.
- Avoid mixed mega-patches that block safe rollback.

## 4) Repository Map (High-Level)

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module exports and shared command enums
- `src/config/` — schema + config loading/merging
- `src/agent/` — orchestration loop
- `src/gateway/` — webhook/gateway server
- `src/security/` — policy, pairing, secret store
- `src/memory/` — markdown/sqlite memory backends + embeddings/vector merge
- `src/providers/` — model providers and resilient wrapper
- `src/channels/` — Telegram/Discord/Slack/etc channels
- `src/tools/` — tool execution surface (shell, file, memory, browser)
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO); see `docs/hardware-peripherals-design.md`
- `src/runtime/` — runtime adapters (currently native)
- `docs/` — task-oriented documentation system (hubs, unified TOC, references, operations, security proposals, multilingual guides)
- `.github/` — CI, templates, automation workflows

## 4.1 Documentation System Contract (Required)

Treat documentation as a first-class product surface, not a post-merge artifact.

Canonical entry points:

- root READMEs: `README.md`, `README.zh-CN.md`, `README.ja.md`, `README.ru.md`, `README.fr.md`, `README.vi.md`, `README.el.md`
- docs hubs: `docs/README.md`, `docs/i18n/zh-CN/README.md`, `docs/i18n/ja/README.md`, `docs/i18n/ru/README.md`, `docs/i18n/fr/README.md`, `docs/i18n/vi/README.md`, `docs/i18n/el/README.md`
- unified TOC: `docs/SUMMARY.md`
- i18n governance docs: `docs/i18n-guide.md`, `docs/i18n/README.md`, `docs/i18n-coverage.md`

Supported locales (current contract):

- `en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`

Collection indexes (category navigation):

- `docs/getting-started/README.md`
- `docs/reference/README.md`
- `docs/operations/README.md`
- `docs/security/README.md`
- `docs/hardware/README.md`
- `docs/contributing/README.md`
- `docs/project/README.md`

Runtime-contract references (must track behavior changes):

- `docs/commands-reference.md`
- `docs/providers-reference.md`
- `docs/channels-reference.md`
- `docs/config-reference.md`
- `docs/operations-runbook.md`
- `docs/troubleshooting.md`
- `docs/one-click-bootstrap.md`

Required docs governance rules:

- Keep README/hub top navigation and quick routes intuitive and non-duplicative.
- Keep entry-point parity across all supported locales (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`) when changing navigation architecture.
- If a change touches docs IA, runtime-contract references, or user-facing wording in shared docs, perform i18n follow-through for currently supported locales in the same PR:
  - Update locale navigation links (`README*`, `docs/README*`, `docs/SUMMARY.md`).
  - Update canonical locale hubs and summaries under `docs/i18n/<locale>/` for every supported locale.
  - Update localized runtime-contract docs where equivalents exist (currently full trees for `vi` and `el`; do not regress `zh-CN`/`ja`/`ru`/`fr` hub parity).
  - Keep `docs/*.<locale>.md` compatibility shims aligned if present.
- Follow `docs/i18n-guide.md` as the mandatory completion checklist when docs navigation or shared wording changes.
- Keep proposal/roadmap docs explicitly labeled; avoid mixing proposal text into runtime-contract docs.
- Keep project snapshots date-stamped and immutable once superseded by a newer date.

### 4.2 Docs i18n Completion Gate (Required)

For any PR that changes docs IA, locale navigation, or shared docs wording:

1. Complete i18n follow-through in the same PR using `docs/i18n-guide.md`.
2. Keep all supported locale hubs/summaries navigable through canonical `docs/i18n/<locale>/` paths.
3. Update `docs/i18n-coverage.md` when coverage status or locale topology changes.
4. If any translation must be deferred, record explicit owner + follow-up issue/PR in the PR description.

## 5) Risk Tiers by Path (Review Depth Contract)

Use these tiers when deciding validation depth and review rigor.

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

## 6) Agent Workflow (Required)

1. **Read before write**
    - Inspect existing module, factory wiring, and adjacent tests before editing.
2. **Define scope boundary**
    - One concern per PR; avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch**
    - Apply KISS/YAGNI/DRY rule-of-three explicitly.
4. **Validate by risk tier**
    - Docs-only: lightweight checks.
    - Code/risky changes: full relevant checks and focused scenarios.
5. **Document impact**
    - Update docs/PR notes for behavior, risk, side effects, and rollback.
    - If CLI/config/provider/channel behavior changed, update corresponding runtime-contract references.
    - If docs entry points changed, keep all supported locale README/docs-hub navigation aligned (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`).
    - Run through `docs/i18n-guide.md` and record any explicit i18n deferrals in the PR summary.
6. **Respect queue hygiene**
    - If stacked PR: declare `Depends on #...`.
    - If replacing old PR: declare `Supersedes #...`.

### 6.1 Branch / Commit / PR Flow (Required)

All contributors (human or agent) must follow the same collaboration flow:

- Create and work from a non-`main` branch.
- Commit changes to that branch with clear, scoped commit messages.
- Open a PR to `dev`; do not push directly to `dev` or `main`.
- `main` is reserved for release promotion PRs from `dev`.
- Wait for required checks and review outcomes before merging.
- Merge via PR controls (squash/rebase/merge as repository policy allows).
- After merge/close, clean up task branches/worktrees that are no longer needed.
- Keep long-lived branches only when intentionally maintained with clear owner and purpose.

### 6.1A PR Disposition and Workflow Authority (Required)

- Decide merge/close outcomes from repository-local authority in this order: `.github/workflows/**`, GitHub branch protection/rulesets, `docs/pr-workflow.md`, then this `AGENTS.md`.
- External agent skills/templates are execution aids only; they must not override repository-local policy.
- A normal contributor PR targeting `main` is a routing defect, not by itself a closure reason; if intent and content are legitimate, retarget to `dev`.
- Direct-close the PR (do not supersede/replay) when high-confidence integrity-risk signals exist:
  - unapproved or unrelated repository rebranding attempts (for example replacing project logo/identity assets)
  - unauthorized platform-surface expansion (for example introducing `web` apps, dashboards, frontend stacks, or UI surfaces not requested by maintainers)
  - title/scope deception that hides high-risk code changes (for example `docs:` title with broad `src/**` changes)
  - spam-like or intentionally harmful payload patterns
  - multi-domain dirty-bundle changes with no safe, auditable isolation path
- If unauthorized platform-surface expansion is detected during review/implementation, report to maintainers immediately and pause further execution until explicit direction is given.
- Use supersede flow only when maintainers explicitly want to preserve valid work and attribution.
- In public PR close/block comments, state only direct actionable reasons; do not include internal decision-process narration or "non-reason" qualifiers.

### 6.1B Assignee-First Gate (Required)

- For any GitHub issue or PR selected for active handling, the first action is to ensure `@chumyin` is an assignee.
- This is additive ownership: keep existing assignees and add `@chumyin` if missing.
- Do not start triage/review/implementation/merge work before assignee assignment is confirmed.
- Queue safety rule: assign only the currently active target; do not pre-assign future queued targets.

### 6.2 Worktree Workflow (Required for All Task Streams)

Use Git worktrees to isolate every active task stream safely and predictably:

- Use one dedicated worktree per active branch/PR stream; do not implement directly in a shared default workspace.
- Keep each worktree on a single branch and a single concern; do not mix unrelated edits in one worktree.
- Before each commit/push, verify commit hygiene in that worktree (`git status --short` and `git diff --cached`) so only scoped files are included.
- Run validation commands inside the corresponding worktree before commit/PR.
- Name worktrees clearly by scope (for example: `wt/ci-hardening`, `wt/provider-fix`).
- After PR merge/close (or task abandonment), remove stale worktrees/branches and prune refs (`git worktree prune`, `git fetch --prune`).
- Local Codex automation may use one-command cleanup helper: `~/.codex/skills/zeroclaw-pr-issue-automation/scripts/cleanup_track.sh --repo-dir <repo_dir> --worktree <worktree_path> --branch <branch_name>`.
- PR checkpoint rules from section 6.1 still apply to worktree-based development.

### 6.3 Code Naming Contract (Required)

Apply these naming rules for all code changes unless a subsystem has a stronger existing pattern.

- Use Rust standard casing consistently: modules/files `snake_case`, types/traits/enums `PascalCase`, functions/variables `snake_case`, constants/statics `SCREAMING_SNAKE_CASE`.
- Name types and modules by domain role, not implementation detail (for example `DiscordChannel`, `SecurityPolicy`, `MemoryStore` over vague names like `Manager`/`Helper`).
- Keep trait implementer naming explicit and predictable: `<ProviderName>Provider`, `<ChannelName>Channel`, `<ToolName>Tool`, `<BackendName>Memory`.
- Keep factory registration keys stable, lowercase, and user-facing (for example `"openai"`, `"discord"`, `"shell"`), and avoid alias sprawl without migration need.
- Name tests by behavior/outcome (`<subject>_<expected_behavior>`) and keep fixture identifiers neutral/project-scoped.
- If identity-like naming is required in tests/examples, use ZeroClaw-native labels only (`ZeroClawAgent`, `zeroclaw_user`, `zeroclaw_node`).

### 6.4 Architecture Boundary Contract (Required)

Use these rules to keep the trait/factory architecture stable under growth.

- Extend capabilities by adding trait implementations + factory wiring first; avoid cross-module rewrites for isolated features.
- Keep dependency direction inward to contracts: concrete integrations depend on trait/config/util layers, not on other concrete integrations.
- Avoid creating cross-subsystem coupling (for example provider code importing channel internals, tool code mutating gateway policy directly).
- Keep module responsibilities single-purpose: orchestration in `agent/`, transport in `channels/`, model I/O in `providers/`, policy in `security/`, execution in `tools/`.
- Introduce new shared abstractions only after repeated use (rule-of-three), with at least one real caller in current scope.
- For config/schema changes, treat keys as public contract: document defaults, compatibility impact, and migration/rollback path.

## 7) Change Playbooks

### 7.1 Adding a Provider

- Implement `Provider` in `src/providers/`.
- Register in `src/providers/mod.rs` factory.
- Add focused tests for factory wiring and error paths.
- Avoid provider-specific behavior leaks into shared orchestration code.

### 7.2 Adding a Channel

- Implement `Channel` in `src/channels/`.
- Keep `send`, `listen`, `health_check`, typing semantics consistent.
- Cover auth/allowlist/health behavior with tests.

### 7.3 Adding a Tool

- Implement `Tool` in `src/tools/` with strict parameter schema.
- Validate and sanitize all inputs.
- Return structured `ToolResult`; avoid panics in runtime path.

### 7.4 Adding a Peripheral

- Implement `Peripheral` in `src/peripherals/`.
- Peripherals expose `tools()` — each tool delegates to the hardware (GPIO, sensors, etc.).
- Register board type in config schema if needed.
- See `docs/hardware-peripherals-design.md` for protocol and firmware notes.

### 7.5 Security / Runtime / Gateway Changes

- Include threat/risk notes and rollback strategy.
- Add/update tests or validation evidence for failure modes and boundaries.
- Keep observability useful but non-sensitive.
- For `.github/workflows/**` changes, include Actions allowlist impact in PR notes and update `docs/actions-source-policy.md` when sources change.

### 7.6 Docs System / README / IA Changes

- Treat docs navigation as product UX: preserve clear pathing from README -> docs hub -> SUMMARY -> category index.
- Keep top-level nav concise; avoid duplicative links across adjacent nav blocks.
- When runtime surfaces change, update related references (`commands/providers/channels/config/runbook/troubleshooting`).
- Keep multilingual entry-point parity for all supported locales (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`) when nav or key wording changes.
- When shared docs wording changes, sync corresponding localized docs for supported locales in the same PR (or explicitly document deferral and follow-up PR).
- Treat `docs/i18n/<locale>/**` as canonical for localized hubs/summaries; keep docs-root compatibility shims aligned when edited.
- Apply `docs/i18n-guide.md` completion checklist before merge and include i18n status in PR notes.
- For docs snapshots, add new date-stamped files for new sprints rather than rewriting historical context.


## 8) Validation Matrix

Default local checks for code changes:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Preferred local pre-PR validation path (recommended, not required):

```bash
./dev/ci.sh all
```

Notes:

- Local Docker-based CI is strongly recommended when Docker is available.
- Contributors are not blocked from opening a PR if local Docker CI is unavailable; in that case run the most relevant native checks and document what was run.

Additional expectations by change type:

- **Docs/template-only**:
    - run markdown lint and link-integrity checks
    - if touching README/docs-hub/SUMMARY/collection indexes, verify EN/ZH-CN/JA/RU/FR/VI/EL navigation parity
    - if touching bootstrap docs/scripts, run `bash -n bootstrap.sh scripts/bootstrap.sh scripts/install.sh`
- **Workflow changes**: validate YAML syntax; run workflow lint/sanity checks when available.
- **Security/runtime/gateway/tools**: include at least one boundary/failure-mode validation.

If full checks are impractical, run the most relevant subset and document what was skipped and why.

## 9) Collaboration and PR Discipline

- Follow `.github/pull_request_template.md` fully (including side effects / blast radius).
- Keep PR descriptions concrete: problem, change, non-goals, risk, rollback.
- For issue-driven work, add explicit issue-closing keywords in the **PR body** for every resolved issue (for example `Closes #1502`).
- Do not rely on issue comments alone for linkage visibility; comments are supplemental, not a substitute for PR-body closing references.
- Default to one issue per clean commit/PR track. For multiple issues, split into separate clean commits/PRs unless there is clear technical coupling.
- If multiple issues are intentionally bundled in one PR, document the coupling rationale explicitly in the PR summary.
- Commit hygiene is mandatory: stage only task-scoped files and split unrelated changes into separate commits/worktrees.
- Completion hygiene is mandatory: after merge/close, clean stale local branches/worktrees before starting the next track.
- Use conventional commit titles.
- Prefer small PRs (`size: XS/S/M`) when possible.
- Agent-assisted PRs are welcome, **but contributors remain accountable for understanding what their code will do**.

### 9.1 Privacy/Sensitive Data and Neutral Wording (Required)

Treat privacy and neutrality as merge gates, not best-effort guidelines.

- Never commit personal or sensitive data in code, docs, tests, fixtures, snapshots, logs, examples, or commit messages.
- Prohibited data includes (non-exhaustive): real names, personal emails, phone numbers, addresses, access tokens, API keys, credentials, IDs, and private URLs.
- Use neutral project-scoped placeholders (for example: `user_a`, `test_user`, `project_bot`, `example.com`) instead of real identity data.
- Test names/messages/fixtures must be impersonal and system-focused; avoid first-person or identity-specific language.
- If identity-like context is unavoidable, use ZeroClaw-scoped roles/labels only (for example: `ZeroClawAgent`, `ZeroClawOperator`, `zeroclaw_user`) and avoid real-world personas.
- Recommended identity-safe naming palette (use when identity-like context is required):
    - actor labels: `ZeroClawAgent`, `ZeroClawOperator`, `ZeroClawMaintainer`, `zeroclaw_user`
    - service/runtime labels: `zeroclaw_bot`, `zeroclaw_service`, `zeroclaw_runtime`, `zeroclaw_node`
    - environment labels: `zeroclaw_project`, `zeroclaw_workspace`, `zeroclaw_channel`
- If reproducing external incidents, redact and anonymize all payloads before committing.
- Before push, review `git diff --cached` specifically for accidental sensitive strings and identity leakage.

### 9.2 Superseded-PR Attribution (Required)

When a PR supersedes another contributor's PR and carries forward substantive code or design decisions, preserve authorship explicitly.

- In the integrating commit message, add one `Co-authored-by: Name <email>` trailer per superseded contributor whose work is materially incorporated.
- Use a GitHub-recognized email (`<login@users.noreply.github.com>` or the contributor's verified commit email) so attribution is rendered correctly.
- Keep trailers on their own lines after a blank line at commit-message end; never encode them as escaped `\\n` text.
- In the PR body, list superseded PR links and briefly state what was incorporated from each.
- If no actual code/design was incorporated (only inspiration), do not use `Co-authored-by`; give credit in PR notes instead.

### 9.3 Superseded-PR PR Template (Recommended)

When superseding multiple PRs, use a consistent title/body structure to reduce reviewer ambiguity.

- Recommended title format: `feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]`
- If this is docs/chore/meta only, keep the same supersede suffix and use the appropriate conventional-commit type.
- In the PR body, include the following template (fill placeholders, remove non-applicable lines):

```md
## Supersedes
- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>
- #<pr_n> by @<author_n>

## Integrated Scope
- From #<pr_a>: <what was materially incorporated>
- From #<pr_b>: <what was materially incorporated>
- From #<pr_n>: <what was materially incorporated>

## Attribution
- Co-authored-by trailers added for materially incorporated contributors: Yes/No
- If No, explain why (for example: no direct code/design carry-over)

## Non-goals
- <explicitly list what was not carried over>

## Risk and Rollback
- Risk: <summary>
- Rollback: <revert commit/PR strategy>
```

### 9.4 Superseded-PR Commit Template (Recommended)

When a commit unifies or supersedes prior PR work, use a deterministic commit message layout so attribution is machine-parsed and reviewer-friendly.

- Keep one blank line between message sections, and exactly one blank line before trailer lines.
- Keep each trailer on its own line; do not wrap, indent, or encode as escaped `\n` text.
- Add one `Co-authored-by` trailer per materially incorporated contributor, using GitHub-recognized email.
- If no direct code/design is carried over, omit `Co-authored-by` and explain attribution in the PR body instead.

```text
feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]

<one-paragraph summary of integrated outcome>

Supersedes:
- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>
- #<pr_n> by @<author_n>

Integrated scope:
- <subsystem_or_feature_a>: from #<pr_x>
- <subsystem_or_feature_b>: from #<pr_y>

Co-authored-by: <Name A> <login_a@users.noreply.github.com>
Co-authored-by: <Name B> <login_b@users.noreply.github.com>
```

Reference docs:

- `CONTRIBUTING.md`
- `docs/README.md`
- `docs/SUMMARY.md`
- `docs/i18n-guide.md`
- `docs/i18n/README.md`
- `docs/i18n-coverage.md`
- `docs/docs-inventory.md`
- `docs/commands-reference.md`
- `docs/providers-reference.md`
- `docs/channels-reference.md`
- `docs/config-reference.md`
- `docs/operations-runbook.md`
- `docs/troubleshooting.md`
- `docs/one-click-bootstrap.md`
- `docs/pr-workflow.md`
- `docs/reviewer-playbook.md`
- `docs/ci-map.md`
- `docs/actions-source-policy.md`

## 10) Anti-Patterns (Do Not)

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags “just in case”.
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules “while here”.
- Do not bypass failing checks without explicit explanation.
- Do not hide behavior-changing side effects in refactor commits.
- Do not include personal identity or sensitive information in test data, examples, docs, or commits.
- Do not attempt repository rebranding/identity replacement unless maintainers explicitly requested it in the current scope.
- Do not introduce new platform surfaces (for example `web` apps, dashboards, frontend stacks, or UI portals) unless maintainers explicitly requested them in the current scope.

## 11) Handoff Template (Agent -> Agent / Maintainer)

When handing off work, include:

1. What changed
2. What did not change
3. Validation run and results
4. Remaining risks / unknowns
5. Next recommended action

## 12) Vibe Coding Guardrails

When working in fast iterative mode:

- Keep each iteration reversible (small commits, clear rollback).
- Validate assumptions with code search before implementing.
- Prefer deterministic behavior over clever shortcuts.
- Do not “ship and hope” on security-sensitive paths.
- If uncertain, leave a concrete TODO with verification context, not a hidden guess.
