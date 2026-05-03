# Claude Code Skills

The repo ships a set of [Claude Code skills](https://docs.claude.com/en/docs/agents/skills) under `.claude/skills/` that automate the heavier parts of the maintainer workflow — PR reviews, issue triage, squash-merging, changelog generation, and more.

Each skill lives in its own directory with a `SKILL.md` file. Claude Code loads them automatically when you open the repo; invoke them by describing what you want in plain language, or by explicit reference (e.g. `/squash-merge 1234`).

## Available skills

| Skill | Use it when |
|---|---|
| `github-pr-review-session` | Reviewing a specific PR or working through the review queue — drafts the review body, cross-checks against source, posts via `gh` as WareWolf-MoonWall |
| `github-issue-triage` | Running a backlog sweep, closing stale/duplicate issues, applying labels, enforcing the RFC stale policy |
| `github-issue` | Filing a structured issue (bug report or feature request) |
| `github-pr` | Opening or updating a PR with a fully-populated template body |
| `squash-merge` | Landing an approved PR into `master` with preserved commit history and the purple **Merged** badge |
| `changelog-generation` | Preparing `CHANGELOG-next.md` for a release — summarises merges since the last tag |
| `skill-creator` | Creating, editing, or benchmarking the skills themselves |
| `zeroclaw` | Operating the running ZeroClaw instance (CLI + gateway API) |

## PR review workflow

The `github-pr-review-session` skill is the main tool for review days. A typical session looks like:

```
> review 1234
```

The skill reads `AGENTS.md`, the reviewer playbook, and the PR's diff + commits, then drafts a review. It uses:

- **Inline comments** for every `[blocking]` / `[suggestion]` / `[question]` finding
- **Review body** only for overall verdict and template-level issues
- **Bare commit hashes** (never wrapped in backticks — GitHub auto-links them)
- **@-prefixed usernames** in all review content

Findings follow the house tier system: `[blocking]` holds the PR, `[suggestion]` is optional, `[question]` asks for clarification.

The skill always shows a draft for approval before posting. Reviews are posted under the human reviewer's identity — not as a bot.

Re-review after changes:

```
> re-review 1234
```

Or work through the queue:

```
> go through the queue
```

## Issue triage workflow

The `github-issue-triage` skill runs autonomous backlog sweeps within defined authority bounds. Modes:

- **Triage pass** — label, link to related PRs, apply `needs-author-action` where applicable
- **Stale pass** — close issues that have been idle past the policy threshold
- **Wont-fix pass** — close issues that won't be accepted, with a brief rationale
- **Specific issue** — handle a single issue by number

Labels and the stale policy are defined in [Reviewer playbook → Issue triage](./reviewer-playbook.md#issue-triage). The skill escalates ambiguity to the user before acting.

PRs with merge conflicts receive `needs-author-action` only — no review, no diff comment — per `feedback_conflicts_label_only`.

## Squash-merge strategy

ZeroClaw uses squash-merge for all PRs; see [PR Workflow](./pr-workflow.md#maintainer-merge-process) for the human merge-readiness process and rare regular-merge exceptions. The `squash-merge` skill handles the final execution step by producing both the purple **Merged** badge *and* a conventional-commits formatted squash message with full commit history in the body.

### Why the skill exists

GitHub's default squash-merge:

- Omits the PR number from the subject
- Formats the body inconsistently
- Doesn't match project conventions

Direct-pushing a squash to master bypasses the PR merge mechanism — the PR shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close, no merge association). The skill uses `gh pr merge --subject --body` to get both the badge and the correctly formatted commit.

### Format

- **Subject:** `<PR title> (#<number>)` — must be conventional commits (`feat(scope): …`, `fix: …`, etc.)
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

Skills are plain Markdown with YAML frontmatter. Their `description` field is what Claude Code uses to decide when to trigger them — be specific and include concrete trigger phrases (`"review 1234"`, `"triage issues"`, etc.). Use `skill-creator` to edit them; it enforces the structure and helps run evals to measure trigger accuracy.

When a skill's behaviour diverges from what the docs describe (e.g. the reviewer playbook changes), update the skill **and** any docs referencing it. The skill's `SKILL.md` is canonical for the automation; the contributing docs are canonical for the humans.
