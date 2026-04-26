---
name: factory-clerk
description: "Conservative autonomous factory records cleanup for ZeroClaw. Use this skill when the user wants to reduce duplicate issues/PRs, close issues already fixed by merged PRs, close PRs superseded by merged work, link open PRs to issues, run a software-factory cleanup sweep, or run the Factory Clerk automation. Trigger on: 'factory clerk', 'reduce duplicates', 'close implemented issues', 'close superseded PRs', 'link PRs to issues', 'software factory cleanup', or 'clerk sweep'."
---

# Factory Clerk

Autonomous factory records cleanup for low-ambiguity lifecycle actions. This skill complements `github-issue-triage`: issue triage decides what an issue is; Factory Clerk finds cleanup actions that can be safely repeated across the issue and PR backlog.

## Authority

Read `references/policy.md` before taking any destructive action. The short version:

- `preview`: always safe; produces candidate actions only.
- `comment-only`: may post issue/PR comments for exact deterministic links.
- `apply-safe`: may close only explicit duplicates, issues explicitly fixed by merged PRs, and PRs explicitly superseded by merged PRs.
- Anything semantic, partial, competing, or inferred from similarity is preview-only unless the user explicitly approves a specific action.

Never close security, RFC, tracking, blocked, or maintainer-decision issues. Never close based on open PRs. Never close when the relationship is only "related" or "partial".

## Runner

Use the deterministic runner first:

```bash
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py \
  --repo zeroclaw-labs/zeroclaw \
  --mode preview
```

Modes:

```bash
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode preview
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode comment-only
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py --mode apply-safe
```

Useful scoped runs:

```bash
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py \
  --checks fixed-by-merged-pr,explicit-duplicates

python3 .claude/skills/factory-clerk/scripts/factory_clerk.py \
  --checks open-pr-links,superseded-prs \
  --mode comment-only

python3 .claude/skills/factory-clerk/scripts/factory_clerk.py \
  --checks implemented-on-master-preview \
  --mode preview
```

The runner writes a JSON audit log to `artifacts/factory-clerk/` unless `--no-audit-file` is passed.

Useful safety controls:

```bash
python3 .claude/skills/factory-clerk/scripts/factory_clerk.py \
  --mode apply-safe \
  --max-mutations 10 \
  --summary-file artifacts/factory-clerk/summary.md
```

- Hidden comment markers prevent repeat comments on later runs.
- `--max-mutations` caps comments/closures in one non-preview run.
- `--label <name>` can tag mutated issues/PRs when the label already exists.
- `--include-marked` shows candidates already handled by earlier Factory Clerk comments.
- `implemented-on-master-preview` searches the checked-out codebase for concrete symbols mentioned by open issues and queues review candidates only.

## Workflow

1. Run `preview`.
2. Inspect `AUTO_CLOSE`, `AUTO_COMMENT`, and `QUEUE_FOR_REVIEW` groups.
3. If the preview is clean, rerun `comment-only` or `apply-safe`.
4. Report counts to the user: candidates, comments, closures, skipped protected targets.

If the runner emits `QUEUE_FOR_REVIEW`, do not act autonomously. Review those cases with the normal `github-issue-triage` or `github-pr-review-session` skill.
