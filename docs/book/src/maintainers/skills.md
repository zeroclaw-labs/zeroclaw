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
| `factory-clerk` | Conservative autonomous records cleanup for exact duplicates, issues fixed by merged PRs, superseded PRs, and PR-to-issue links |
| `factory-inspector` | Intake quality checks for PR templates, validation evidence, linked issues, risk labels, and thin issue reports |
| `factory-testbench` | Offline replay and safety invariants for factory automation decisions |
| `factory-foreman` | Preview-first orchestration across Testbench, Clerk, and Inspector |
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

## Factory clerk workflow

The `factory-clerk` skill is the software-factory records cleanup layer for low-ambiguity lifecycle work. It is intentionally narrower than issue triage: it finds exact cleanup candidates and either previews, comments, or applies safe closures.

Run locally:

```bash
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode preview
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode comment-only
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode apply-safe
```

Run from Actions with **Factory Clerk**. The scheduled run is preview-only and uploads a JSON audit artifact plus a Markdown job summary. Manual dispatch can run `comment-only` or `apply-safe` with a mutation cap.

Autonomous closure is limited to:

- open issues explicitly fixed by merged PRs with closing keywords;
- issues explicitly marked duplicate by a maintainer comment;
- open PRs explicitly superseded by a merged PR.

Similarity-only matches, partial coverage, competing config surfaces, and anything with protected labels are preview-only and must go through normal maintainer review.
Protected label matching is case-insensitive; exact `security` and `security:*` labels are protected.

Repeated runs are safe by default: comments include hidden Factory Clerk markers, and later runs skip candidates with matching markers unless `--include-marked` is passed.

## Factory inspector workflow

The `factory-inspector` skill is the intake quality gate. It checks whether PRs and issues are reviewable before maintainers spend deeper review time.

Run locally:

```bash
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --mode preview
python3 .claude/skills/factory-inspector/scripts/factory_inspector.py --mode comment-only --checks pr-intake
```

Run from Actions with **Factory Inspector**. The scheduled run is preview-only and uploads a JSON audit artifact plus a Markdown job summary. Manual dispatch can run `comment-only`, which posts a single checklist comment on PRs with deterministic intake failures.

The first version comments only on PR intake issues. Issue intake findings remain preview-only so maintainers can tune the signal before automating contributor-facing issue comments.

## Factory testbench workflow

The `factory-testbench` skill is the factory safety harness. It snapshots GitHub issue/PR state, replays Clerk and Inspector decisions offline, and fails when core safety invariants are violated.

Run local fixture tests:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py fixture-test
```

Snapshot and replay live GitHub state:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py roundtrip --repo zeroclaw-labs/zeroclaw
```

Run from Actions with **Factory Testbench**. The workflow is read-only and uploads snapshot/replay artifacts. Optional `--clone-dir` creates or updates a local bare mirror clone for deeper replay experiments. Private GitHub sandbox creation is intentionally local/manual only.

Create a private GitHub sandbox locally:

```bash
python3 .claude/skills/factory-testbench/scripts/factory_testbench.py sandbox \
  --repo zeroclaw-labs/zeroclaw \
  --target-repo OWNER/zeroclaw-factory-sandbox \
  --run-foreman-mode preview
```

Sandbox replay creates a private target repository, mirror-pushes code, recreates labels/issues/PRs/comments, records number mappings, and can run Foreman against the sandbox instead of production. It is intentionally explicit and is not scheduled by default.

## Factory foreman workflow

The `factory-foreman` skill coordinates factory roles. It always runs Testbench first, then Clerk, then Inspector.

Run preview locally:

```bash
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py --mode preview
```

Manual non-preview runs:

```bash
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py --mode comment-only --max-mutations 10
python3 .claude/skills/factory-foreman/scripts/factory_foreman.py --mode apply-safe --allow-apply-safe --max-mutations 10
```

Run from Actions with **Factory Foreman**. Scheduled runs are preview-only. `apply-safe` requires the explicit `allow_apply_safe=true` workflow input and still runs Testbench before any Clerk/Inspector action.

## Squash-merge strategy

ZeroClaw uses squash-merge for all PRs. The `squash-merge` skill produces both the purple **Merged** badge *and* a conventional-commits formatted squash message with full commit history in the body.

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
