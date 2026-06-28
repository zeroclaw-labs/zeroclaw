# CLAUDE.md — ZeroClaw (Claude Code)

> **Shared instructions live in [`AGENTS.md`](./AGENTS.md).**
> This file contains only Claude Code-specific directives.

## Claude Code Settings

Claude Code should read and follow all instructions in `AGENTS.md` at the repository root for project conventions, commands, risk tiers, workflow rules, and anti-patterns.

## Hooks

_No custom hooks defined yet._

## Slash Commands

_No custom slash commands defined yet._


## Ship Cycle (HARD RULE — Always Enforced) <!-- SHIP-CYCLE-ENFORCED -->

Every code/content change ships **end-to-end, immediately, without being asked**:

> **branch → commit → push → open PR → merge (squash) → deploy LIVE**

- Never stop at an uncommitted tree, "committed but not pushed," "merged but not deployed," or a "want me to deploy?" question. **Done = live in production.**
- Branch per change off fresh `origin/main`; never commit to `main`/`master` directly.
- End every commit message with `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`. End PR bodies with the Claude Code generation line.
- Wait for CI/status checks to go green before merging. **Never merge a red or pending build** — report the failure and stop.
- NEVER commit secrets, credentials, or `.env` files.
- **Deploy is part of the change.** After merge, sync `main` and deploy to production. Route deploys through `~/.claude/scripts/ship-deploy.sh <project-dir> [-- <deploy cmd>]` so concurrent agents serialize; `unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID` first to use the OAuth session.
- **No remote** (local-only repo): do branch → commit and note no PR/remote exists. **Not deployable** (library, legal-matter vault, research repo): the cycle ends at merge — state that explicitly.
