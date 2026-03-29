---
name: upstream-scout
description: "Evaluate and port upstream PRs and issues to Hrafn with proper attribution. Identifies unreviewed, silently closed, or re-submitted contributions worth adopting. Use when scouting for upstream work to port, or when ensuring correct git authorship during cherry-picks."
---

# Upstream Scout

Find valuable contributions that upstream ZeroClaw ignored, closed without comment, or re-submitted under maintainer names. Port them to Hrafn with proper attribution and cross-references.

## Upstream repo

`zeroclaw-labs/zeroclaw` on GitHub.

## Phase 1: Discovery

Use `gh` CLI to search for candidates. Start with PRs, issues later.

### Search queries

```bash
# PRs closed without merge, no comments from maintainers
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --json number,title,author,closedAt,comments,labels,additions,deletions \
  --limit 100 | jq '[.[] | select(.comments == 0)]'

# PRs open for >14 days without review
gh pr list -R zeroclaw-labs/zeroclaw \
  --state open --json number,title,author,createdAt,reviewDecision,labels \
  --limit 100 | jq '[.[] | select(.reviewDecision == null)]'

# PRs by known mistreated contributors (add handles as discovered)
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --author 5queezer --json number,title,state,mergedAt,closedAt
gh pr list -R zeroclaw-labs/zeroclaw \
  --state closed --author creke --json number,title,state,mergedAt,closedAt

# Issues with high engagement but no maintainer response
gh issue list -R zeroclaw-labs/zeroclaw \
  --state open --json number,title,comments,reactionGroups,createdAt \
  --limit 100 | jq '[.[] | select(.comments > 3)]'
```

### Known maintainer handles (do not credit these as community contributors)

- theonlyhennygod (lead)
- JordanTheJet (code owner)
- SimianAstronaut7 (code owner / collaborator)

## Phase 2: Evaluation

For each candidate, score on two axes:

### Axis 1: User impact (1-5)

| Score | Meaning |
|-------|---------|
| 5 | Security fix or crash prevention |
| 4 | Gap-creating feature (differentiates Hrafn from other claw implementations) |
| 3 | Quality-of-life improvement for existing users |
| 2 | Nice to have, minor improvement |
| 1 | Cosmetic or niche |

### Axis 2: Community signal (1-5)

| Score | Meaning |
|-------|---------|
| 5 | PR closed without comment AND re-submitted by maintainer under their name |
| 4 | PR closed without comment, contributor had tests + CI green |
| 3 | PR open >30 days, no review, contributor still active |
| 2 | Issue with >5 upvotes, no maintainer response |
| 1 | Standard closed PR with explanation |

### Priority matrix

- **Port first:** Impact >= 4 OR Community >= 4
- **Port second:** Impact >= 3 AND Community >= 2
- **Port for goodwill:** Impact < 3 AND Community >= 4
- **Skip:** Impact < 3 AND Community < 2

### Output format

For each candidate, produce:

```
PR #NNNN: <title>
Author: @handle
Impact: N/5 -- <reason>
Community: N/5 -- <reason>
Priority: Port first | Port second | Goodwill | Skip
Port method: cherry-pick | rewrite | adapt
Notes: <any context>
```

## Phase 3: Porting

### Method selection

| Situation | Method |
|-----------|--------|
| PR applies cleanly to Hrafn's current codebase | `git cherry-pick` (preserve author) |
| PR has merge conflicts but logic is sound | Rebase onto current main, preserve author |
| PR concept is good but implementation needs rework | Rewrite, use `Co-authored-by:` for original author |
| Only the idea/approach is useful, code is different | New implementation, credit in commit message body |

### Attribution rules (mandatory)

1. **Always preserve the original git author** when cherry-picking or rebasing. Never use `--reset-author`.
2. **Co-authored-by** when the port involves significant rewriting but the original contributor's design/approach is used:
   ```
   Co-Authored-By: Original Author <email@example.com>
   ```
3. **Commit message must reference the upstream PR:**
   ```
   feat(a2a): add outbound task delegation

   Ported from zeroclaw-labs/zeroclaw#4166 by @5queezer.
   Original PR was closed without review.
   ```
4. **CONTRIBUTORS.md entry** for every ported contributor (if file exists).

### Branch naming

```
port/zc-NNNN-short-description
```

### PR template additions for ports

Use the `PR: Port` label. In the PR description, add:

```markdown
## Upstream reference

- Original PR: zeroclaw-labs/zeroclaw#NNNN by @author
- Status: Closed without comment / Reverted and re-submitted / Open without review
- Changes from original: <what was adapted for Hrafn>
```

## Phase 4: Cross-reference (post-merge)

After the port PR is merged in Hrafn, comment on the **original upstream PR** (not the issue, the PR itself):

### Comment template

```markdown
Hi @{author} -- your work from this PR has been ported to
[Hrafn](https://github.com/5queezer/hrafn), a community-driven fork
with modular architecture and transparent governance.

See: https://github.com/5queezer/hrafn/pull/NN

Your original authorship is preserved in the git history. Thank you
for the contribution. If you'd like to contribute directly to Hrafn,
see our [CONTRIBUTING.md](https://github.com/5queezer/hrafn/blob/main/CONTRIBUTING.md).
```

### Rules for cross-referencing

- **Only comment on PRs where the contributor was demonstrably mistreated** (closed without comment, re-submitted by maintainer, ignored >30 days).
- **Never comment on PRs that were closed with a valid explanation.**
- **Never trash-talk ZeroClaw.** State facts: "closed without comment", "re-submitted as #NNNN." Let readers draw their own conclusions.
- **One comment per PR.** No follow-ups, no arguments.
- **Tone: sachlich.** Factual, grateful, inviting. Not promotional.

## Limitations

- This skill searches PRs first, issues later (to avoid overloading the CLI).
- GitHub API rate limits apply. Use `--limit` flags and paginate if needed.
- The skill cannot access private repos or deleted PRs.
- Attribution requires the original contributor's git email. If unavailable, use their GitHub handle with `@handle` in the commit message body.

## When NOT to use this skill

- For porting OpenClaw plugins (use the OC Bridge workflow instead).
- For features that don't exist upstream (just build them).
- For upstream PRs that were closed with a valid technical explanation.
