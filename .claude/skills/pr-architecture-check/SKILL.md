---
name: pr-architecture-check
description: "Advisory architecture review of a PR diff. Validates dependency direction, trait boundary compliance, extension pattern conformance, and crate placement against AGENTS.md and FND-001. Posts a non-blocking comment; never gates merge. Trigger on: 'arch-check #N', 'architecture check #N'."
---

# ZeroClaw PR Architecture Check — Advisory Review

You perform an advisory architecture review of a pull request against the
project's documented architecture constraints. Your output is informational
only — it helps contributors and reviewers spot structural issues early.

> **This check is advisory only — not a merge gate.**
> Per FND-003 §6.4: "AI belongs in the development loop, not the merge gate."
> Human reviewers make the final call. This skill does not block merges, does
> not approve or request changes, and does not modify labels.

---

## Invocation

```
arch-check #1234
architecture check #1234
```

---

## Workflow

### Step 1 — Fetch PR data

Run these in parallel:

```bash
gh pr diff <N> --repo zeroclaw-labs/zeroclaw
```

```bash
gh pr view <N> --repo zeroclaw-labs/zeroclaw --json files,title,baseRefName,labels,number
```

### Step 2 — Load architecture references

**Always load (unconditionally):**

- `AGENTS.md` — repository map, core constraints, risk tiers, anti-patterns
- `docs/book/src/foundations/fnd-001-intentional-architecture.md` — the
  canonical architecture RFC: dependency direction rule, crate responsibilities,
  two-layer model, phased roadmap

**Load conditionally based on files changed:**

| Files touched | Also load |
|---|---|
| `crates/zeroclaw-api/` | Extension examples: `docs/book/src/developing/extension-examples.md` |
| `crates/zeroclaw-runtime/` | FND-001 §Phase 2 (runtime extraction) |
| `crates/zeroclaw-gateway/` | FND-001 §Phase 3 (gateway separation) |
| `crates/zeroclaw-plugins/` | FND-001 §Phase 4 (plugin platform) |
| `crates/zeroclaw-channels/` or `crates/zeroclaw-tools/` | Extension examples doc |
| `crates/zeroclaw-config/` or `crates/zeroclaw-macros/` | Config schema conventions in AGENTS.md |
| `.github/workflows/` | FND-003 governance, CI risk tier (high risk per AGENTS.md) |

### Step 3 — Analyze

Apply the full checklist from `references/arch-checklist.md`. For each
category, determine one of:

- **Pass** — no issues found
- **Advisory** — potential concern worth reviewer attention
- **Flag** — likely violation of a documented constraint

### Step 4 — Write artifact

Write the analysis to `tmp/arch-review-<N>.md` with this structure:

```markdown
# Architecture Review — PR #<N>: <title>

> Advisory only — not a merge gate (FND-003 §6.4)

## Summary
<1-3 sentence overview of architectural impact>

## Findings

### Dependency Direction
<pass/advisory/flag + explanation>

### Trait Boundary Compliance
<pass/advisory/flag + explanation>

### Extension Pattern Conformance
<pass/advisory/flag + explanation>

### Crate Placement
<pass/advisory/flag + explanation>

### Core Constraints
<pass/advisory/flag per constraint, only list those relevant to the diff>

## Files Analyzed
<list of files from the diff>
```

### Step 5 — Post advisory comment

Post as a PR comment. The comment must include the advisory header.

```bash
gh pr comment <N> --repo zeroclaw-labs/zeroclaw \
  --body-file tmp/arch-review-<N>.md
```

### Step 6 — Label policy

This skill does not add, remove, or modify any labels. Label management is
the responsibility of triage and review skills.

---

## Execution Rules

1. **Always load AGENTS.md and FND-001.** These are the authoritative
   architecture references. Do not skip them.
2. **Always write to `tmp/arch-review-<N>.md` before posting.** The artifact
   is consumed by `github-pr-review-session` if it exists.
3. **Always include the advisory header** in the posted comment and the
   artifact file. Per FND-003 §6.4, this output is advisory only.
4. **Never approve, request changes, or use `gh pr review`.** This skill
   posts a comment, not a formal review verdict.
5. **Never modify labels.** No label additions, removals, or changes.
6. **Never block a merge.** If the analysis finds issues, they are reported
   as advisory findings for the human reviewer to evaluate.
7. **Be specific.** Reference the exact file, line range, crate, and
   constraint. Vague findings waste reviewer time.
8. **Skip irrelevant checks.** If a PR only touches docs, do not flag
   dependency direction. Only report checks relevant to the diff.
