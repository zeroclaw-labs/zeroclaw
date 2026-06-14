# Claude Code Skills

The repo ships a set of [Claude Code skills](https://docs.claude.com/en/docs/agents/skills) under `.claude/skills/` that automate the heavier parts of the maintainer workflow: PR reviews, issue triage, squash-merging, changelog generation, and more.

Each skill lives in its own directory with a `SKILL.md` file. Claude Code loads them automatically when you open the repo; invoke them by describing what you want in plain language, or by explicit reference (e.g. `/squash-merge 1234`).

## Available skills

| Skill | Use it when |
|---|---|
| `github-pr-review-session` | Reviewing a specific PR or working through the review queue: drafts the review body, cross-checks against source, posts via `gh` under the active account holder's identity |
| `github-issue-triage` | Running a backlog sweep, closing stale/duplicate issues, applying labels, enforcing the RFC stale policy |
| `github-issue` | Filing a structured issue (bug report or feature request) |
| `github-pr` | Opening or updating a PR with a fully-populated template body |
| `squash-merge` | Landing an approved PR into `master` with preserved commit history and the purple **Merged** badge |
| `changelog-generation` | Preparing `CHANGELOG-next.md` for a release: summarises merges since the last tag |
| `skill-creator` | Creating, editing, or benchmarking the skills themselves |
| `zeroclaw` | Operating the running ZeroClaw instance (CLI + gateway API) |

## PR review workflow

The `github-pr-review-session` skill is the main tool for review days. A typical session looks like:

```
> review 1234
```

The skill reads `AGENTS.md`, the reviewer playbook, and the PR's diff + commits, then drafts a review. It uses:

- **Inline diff comments** for every 🔴 blocking, 🟡 warning, or 🔵 suggestion finding tied to a specific line
- **Review body** for overall verdict, comprehension summary, cross-references, and template-level issues not tied to a line
- **Bare commit hashes** (never wrapped in backticks: GitHub auto-links them)
- **@-prefixed usernames** in all review content

Findings follow the [feedback taxonomy](../contributing/pr-review-protocol.md#feedback-taxonomy): 🔴 [blocking] holds the PR, 🟡 [warning] should be addressed, 🔵 [suggestion] is optional, 🟢 [praise] names what works, and ✅ [resolved] acknowledges an addressed finding on re-review. The [PR Review Protocol](../contributing/pr-review-protocol.md) is canonical for the tiers and the review-body Markdown format.

The skill always shows a draft for approval before posting. Reviews are posted under the human reviewer's identity, not as a bot.

Re-review after changes:

```
> re-review 1234
```

Or work through the queue:

```
> go through the queue
```

## Issue triage workflow

The `github-issue-triage` skill runs autonomous backlog sweeps within defined authority bounds. With no argument it runs an **accounting** pass (backlog state, then prompts for a mode); otherwise the modes are:

- **Triage**: process issues with no triage labels: classify, apply labels, link to open PRs, flag thin bug reports, redirect security issues
- **Sweep**: full backlog pass in priority order (fixed-by-merged-PR → duplicates → `r:support` → stale candidates)
- **Stale**: RFC stale-policy enforcement (`status:stale` then close per the policy window and exclusion rules)
- **Won't-fix**: close issues that violate a named core engineering constraint, citing the constraint and its `AGENTS.md`/RFC reference
- **Single**: handle one issue by number or URL

Label definitions live in [Labels](./labels.md); the triage labels the skill applies (`r:needs-repro`, `r:support`, `stale-candidate`, the `status:*` lifecycle labels, and the resolution labels) are all defined there. Stale procedure lives in the issue-triage skill protocol, with reviewer-side context in [Reviewer playbook → Issue triage](./reviewer-playbook.md#issue-triage). The skill escalates ambiguity to the user before acting.

## Squash-merge strategy

ZeroClaw uses squash-merge for all PRs. The `squash-merge` skill produces both the purple **Merged** badge *and* a conventional-commits formatted squash message with full commit history in the body.

### Why the skill exists

GitHub's default squash-merge:

- Omits the PR number from the subject
- Formats the body inconsistently
- Doesn't match project conventions

Direct-pushing a squash to master bypasses the PR merge mechanism: the PR shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close, no merge association). The skill uses `gh pr merge --subject --body` to get both the badge and the correctly formatted commit.

### Format

- **Subject:** `<PR title> (#<number>)`: must be conventional commits (`feat(scope): …`, `fix: …`, etc.)
- **Body (multi-commit PR):** bulleted list of `- <short sha> <commit subject>` from the PR branch
- **Body (single-commit PR):** full commit body, or blank if there isn't one

### Pre-flight checks

The skill stops on:

1. PR not open
2. PR targets a branch other than `master`
3. Merge conflicts present (user must ask author to rebase)
4. `CHANGES_REQUESTED` review outstanding
5. `gh` CLI < 2.17.0 (missing `--subject`/`--body` flags)

A `REVIEW_REQUIRED` state prompts confirmation but doesn't block.

### Invocation

```
> squash-merge 1234
```

or explicit:

```
> /squash-merge 1234
```

The skill always confirms the generated subject and body before calling `gh pr merge`.

## Changelog generation

`changelog-generation` builds `CHANGELOG-next.md` for a release by querying `gh` for merged PRs since the last tag, grouping them by conventional-commits prefix, and formatting them into the house changelog style. Use it as part of the release runbook, before dispatching `release-stable-manual.yml`.

## Editing the skills

Skills are plain Markdown with YAML frontmatter. Their `description` field is what Claude Code uses to decide when to trigger them: be specific and include concrete trigger phrases (`"review 1234"`, `"triage issues"`, etc.). Use `skill-creator` to edit them; it enforces the structure and helps run evals to measure trigger accuracy.

When a skill's behaviour diverges from what the docs describe (e.g. the reviewer playbook changes), update the skill **and** any docs referencing it. The skill's `SKILL.md` is canonical for the automation; the contributing docs are canonical for the humans.
