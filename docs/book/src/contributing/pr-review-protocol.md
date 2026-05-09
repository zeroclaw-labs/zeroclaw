# PR Review Protocol

This is the procedure followed when reviewing a pull request in `zeroclaw-labs/zeroclaw`. It's loaded by the `github-pr-review-session` skill and read by human reviewers — it's authoritative for both.

The `gh` CLI is assumed available and authenticated.

## Fetch order

Run all of these. The data informs every step that follows.

1. **PR overview**

   ```bash
   gh pr view <number> --repo zeroclaw-labs/zeroclaw
   ```

   Description, labels, linked issues, validation evidence.

2. **Top-level conversation**

   ```bash
   gh pr view <number> --comments --repo zeroclaw-labs/zeroclaw
   ```

3. **Inline threads (every reply chain)**

   ```bash
   gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/comments --paginate
   ```

   Read full reply chains before drawing any conclusion about whether something is open or settled. Note author commitments made in replies — they're load-bearing.

4. **Formal reviews**

   ```bash
   gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/reviews --paginate
   ```

   Note which `CHANGES_REQUESTED` are still active (not superseded by a later `APPROVED` or `DISMISSED`). Check whether you've already reviewed this PR.

5. **Relevant foundations documents**

   Always read FND-005 (Contribution Culture). For others, use the relevance
   table below — read what applies to the PR's scope. The ratified versions
   are local files; no API call needed.

   | Foundation | Local file |
   |---|---|
   | Microkernel Architecture | `docs/book/src/foundations/fnd-001-intentional-architecture.md` |
   | Documentation Standards | `docs/book/src/foundations/fnd-002-documentation-standards.md` |
   | Team Governance | `docs/book/src/foundations/fnd-003-governance.md` |
   | Engineering Infrastructure | `docs/book/src/foundations/fnd-004-engineering-infrastructure.md` |
   | Contribution Culture | `docs/book/src/foundations/fnd-005-contribution-culture.md` |
   | Zero Compromise in Practice | `docs/book/src/foundations/fnd-006-zero-compromise-in-practice.md` |

6. **Diff**

   ```bash
   gh pr diff <number> --repo zeroclaw-labs/zeroclaw
   ```

   Read the full diff. Cross-check author commitments from step 3 against what actually shipped. Cross-check against the local repository where the change lands.

## Take stock before writing

Before you write a single line of review, name out loud:

- What's been raised already (across reviews, inline threads, top-level comments).
- What's settled (resolved by author, dismissed by reviewer, addressed in a later commit).
- What's still live (open blockers, unresolved questions, things the author committed to but didn't ship).
- Who holds active blocks, and whether the diff addresses them.

The take-stock pass is what stops you from re-raising settled points and what surfaces who's actually waiting on what.

## Label hygiene

Labels are maintainer metadata, not a contributor blocker. If the right label is obvious and you have permission, fix it yourself before finalizing the review. If you are acting through an assistant, draft the exact label change and get the human reviewer's approval before mutating GitHub.

Ask the author about labels only when the right label choice is ambiguous or nobody with label permissions is available. Do not request changes or hold merge solely because an author cannot edit labels.

## Verdict decision tree

| Situation | Verdict flag |
|---|---|
| Your review is approving and no other reviewer holds an active block | `--approve` |
| Your review is rejecting on substantive grounds you'd block on personally | `--request-changes` |
| You have nothing new to block on but other reviewers hold active blocks | `--comment` |
| You have specific findings but they're all 🔵 suggestions or non-blocking clarification questions | `--comment` |
| You're a maintainer override-approving over another reviewer's `CHANGES_REQUESTED` | **Don't.** Get the other reviewer to dismiss or convert their review first. |

## Feedback taxonomy

Findings in review bodies and inline comments use this PR-review scale, adapted from FND-005. The `✅ [resolved]` entry is for re-reviews that acknowledge addressed findings.

- **🔴 [blocking]** — must be addressed before merge. Use sparingly; every blocker is real or the scale loses meaning.
- **🟡 [warning]** — should be addressed; not blocking but the reviewer wants the author to look.
- **🔵 [suggestion]** — optional. Author can accept or pass.
- **🟢 [praise]** — what's working. Specific praise teaches what to repeat. Generic "great work" teaches nothing.
- **✅ [resolved]** — explicitly acknowledging that a prior finding has been addressed in a later commit. Use this when you're re-reviewing — it shows the author their work registered.

## Review body Markdown format

Formal review body findings should use H3 headings that start with the taxonomy emoji. This keeps severity and required action easy to scan.

Use these canonical forms:

- `### 🔴 Blocking — short issue title`
- `### 🟡 Warning — short issue title`
- `### 🔵 Suggestion — short issue title`
- `### 🟢 What looks good — short positive title`
- `### ✅ Resolved — short resolved item`

Do not write headings like `### Blocking — ...`, `### Finding 1 — ...`, or numbered findings for formal review bodies. Those miss the required taxonomy marker and make the review harder to scan.

## Voice

Write as a thoughtful senior contributor who has read everything and cares about the outcome:

- **Be specific.** Vague feedback creates anxiety without direction. Explain the principle behind every finding, not just the verdict.
- **Name what is good.** Specific praise (`✅ The merge order is correct because…`) builds shared judgment over time.
- **Separate work from person.** "This approach has a problem" not "you made a mistake."
- **Don't re-raise settled points.** If a prior item is resolved, use
  `### ✅ Resolved — ...` so the author sees their work was registered.
- **Reference RFCs by section** when they're the basis for a finding. "Per FND-006 §4.3" is more useful than "per our standards."

## Inline vs body

- **Inline diff comments** for every 🔴 blocking, 🟡 warning, or 🔵 suggestion
  finding tied to a specific line. Anchor the feedback to the code so the
  author can resolve it inline.
- **Review body** for overall verdict, comprehension summary, cross-references to other PRs, and template-level issues that aren't tied to a specific line.
- **Bare commit hashes** (never wrap in backticks — GitHub auto-links bare hashes; backticks block the auto-link).
- **`@`-prefixed usernames** in all review content (chat, body, inline). `@WareWolf-MoonWall`, not `WareWolf-MoonWall`.

## Posting

Write the review body to a file under `tmp/review-<number>.md` first — this is the source of truth for what was posted and lets the user inspect before publishing. Then:

```bash
gh pr review <number> --repo zeroclaw-labs/zeroclaw \
  <--approve | --request-changes | --comment> \
  --body-file tmp/review-<number>.md
```

Always show the full draft and get explicit approval from the human before posting. Continuation words like "next" or "move on" don't count as approval — only an unambiguous "yes" / "approve" / "go" does.

## After posting

If a session-level handoff file exists (`tmp/handoff.md`), update it with the verdict, the head commit reviewed, and what remains open. The handoff is what lets a new session pick up cold without re-reading the whole conversation.

## Never

- **Never approve over another reviewer's active `CHANGES_REQUESTED`.** Resolve the prior block first.
- **Never post a review that re-raises a settled point** without explicitly noting it's already resolved.
- **Never merge.** That's a separate decision and a separate skill.
- **Never push to contributor branches** without explicit instruction. `maintainerCanModify: true` allows it; even then, ask before pushing anything other than trivial fixups.
