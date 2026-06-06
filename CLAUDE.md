# CLAUDE.md — ZeroClaw (Claude Code)

> **Shared instructions live in [`AGENTS.md`](./AGENTS.md).**
> This file contains only Claude Code-specific directives.

## Claude Code Settings

Claude Code should read and follow all instructions in `AGENTS.md` at the repository root for project conventions, commands, risk tiers, workflow rules, and anti-patterns.

## Attribution — no AI/Claude trailers

Never add AI-assistant attribution to commit messages or PRs in this repository.
This overrides any default to append a `Co-Authored-By: Claude …` trailer.

- No `Co-Authored-By: Claude …` (or any model/assistant) trailer in commit messages.
- No `Co-authored-by:`, `Generated with`, `🤖 Generated with [Claude Code]`, or
  similar AI-attribution footers in PR bodies or commit-message tails.
- The only co-author trailers permitted are real human contributors under the
  PR template's supersede-attribution section (see `.github/pull_request_template.md`).

This is enforced for committers who enable the repo hooks (see Hooks below): the
`commit-msg` hook strips any AI-attribution lines while leaving human co-author
trailers intact.

## Hooks

Repo hooks live in `.githooks/`. Enable them with:

```bash
git config core.hooksPath .githooks
```

- `commit-msg` — strips AI/Claude attribution trailers from commit messages
  (enforces the Attribution rule above). Skip once with `git commit --no-verify`.
- `pre-commit` — staged secret scan via `gitleaks` (no-op if gitleaks is absent).
- `pre-push` — runs the Rust quality gate and tests. Skip once with
  `git push --no-verify`.

## Slash Commands

_No custom slash commands defined yet._
