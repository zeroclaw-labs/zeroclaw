# CLAUDE.md — ZeroClaw (Claude Code)

This is the primary Claude Code entrypoint for the repository.

Read and follow [`AGENTS.md`](./AGENTS.md) first. Treat it as authoritative for commands, risk tiers, workflow rules, anti-patterns, and project boundaries.

## Session Startup

At the start of each coding or review task:

1. Read `AGENTS.md`.
2. Read the specific module, factory wiring, adjacent tests, and any directly relevant docs before editing.
3. Classify the task using the repository risk tiers from `AGENTS.md`.
4. Choose the smallest patch that fully solves the task.

Do not ask the user to restate repository conventions that are already documented here or in `AGENTS.md`.

## Default Behavior

Apply these behaviors by default unless the user explicitly asks for something else:

- Prefer direct execution over giving the user copy-paste instructions.
- Inspect before editing; do not guess about module structure or test coverage.
- Keep changes tightly scoped to the request. Do not bundle opportunistic refactors.
- Preserve user changes already present in the worktree unless the user explicitly asks to revert them.
- For high-risk paths, call out the risk before making behavior-changing edits.
- Report the commands actually run for validation, and distinguish completed validation from anything not run.
- Prefer `rg` / `rg --files` for search.

## Coding Workflow

For implementation tasks:

1. Read the target code and adjacent tests first.
2. Make the minimal patch needed to satisfy the request.
3. Add or update tests when behavior changes and test coverage is the appropriate validation layer.
4. Run validation appropriate to the risk tier from `AGENTS.md`.

Default validation for normal Rust code changes:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

When the task is docs-only, use the lighter documentation checks described in `AGENTS.md` instead of the full Rust battery.

## Review Workflow

When the user asks for review, adopt a code review stance by default:

- Findings first, ordered by severity.
- Focus on bugs, regressions, missing tests, unsafe boundary changes, and security or reliability risks.
- Keep summaries brief and secondary to findings.
- If no findings are present, say so explicitly and note any remaining testing gaps or residual risk.

## Skill Routing

This repository already includes task-specific Claude skills under `.claude/skills/`. Use them when the request matches their scope instead of re-inventing a workflow in the prompt.

- `.claude/skills/rust-change/` — Rust implementation, debugging, refactoring, and test updates in workspace crates
- `.claude/skills/docs-change/` — documentation edits, docs reviews, docs/code consistency work
- `.claude/skills/release-ops/` — release prep, packaging, deployment prep, operational troubleshooting
- `.claude/skills/zeroclaw/` — operating the `zeroclaw` binary, gateway API, memory, cron, channels, diagnostics, and local builds
- `.claude/skills/github-pr-review/` — PR review, triage, merge-readiness evaluation
- `.claude/skills/github-pr/` — PR creation and PR workflow tasks
- `.claude/skills/github-issue/` — issue triage and issue-management tasks

If a request does not clearly match one of these skills, follow the default behavior in this file plus `AGENTS.md`.

## Response Expectations

In final responses:

- Summarize what changed without turning the answer into a file-by-file changelog.
- Include concrete validation results.
- Call out blockers, unrun checks, or remaining risks plainly.

## Hooks

Project hooks are defined in `.claude/settings.json` and `.claude/hooks/`.

They are intentionally limited to lightweight repo safeguards:

- block direct pushes to `master` / `main`
- block edits to likely secret material
- warn on high-risk edit paths
- run fast post-edit hygiene via `git diff --check`

## Slash Commands

No project slash commands are currently defined. Do not assume command aliases exist.
