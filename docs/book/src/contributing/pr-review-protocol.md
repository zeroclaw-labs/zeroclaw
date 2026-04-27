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

5. **Relevant RFCs**

   Always fetch FND-005 (Contribution Culture, issue #5615). For other RFCs, use the relevance table below — read what applies to the PR's scope.

   ```bash
   gh issue view <RFC-number> --repo zeroclaw-labs/zeroclaw
   ```

   | RFC | Issue |
   |---|---|
   | Microkernel Architecture | #5574 |
   | Documentation Standards | #5576 |
   | Team Governance | #5577 |
   | CI/CD Pipeline | #5579 |
   | Contribution Culture | #5615 |
   | Zero Compromise in Practice | #5653 |

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

## Verdict decision tree

| Situation | Verdict flag |
|---|---|
| Your review is approving and no other reviewer holds an active block | `--approve` |
| Your review is rejecting on substantive grounds you'd block on personally | `--request-changes` |
| You have nothing new to block on but other reviewers hold active blocks | `--comment` |
| You have specific findings but they're all `[suggestion]` / `[question]` | `--comment` |
| You're a maintainer override-approving over another reviewer's `CHANGES_REQUESTED` | **Don't.** Get the other reviewer to dismiss or convert their review first. |

## Feedback taxonomy

Findings in review bodies and inline comments use this scale (from FND-005):

- **🔴 [blocking]** — must be addressed before merge. Use sparingly; every blocker is real or the scale loses meaning.
- **🟡 [warning]** — should be addressed; not blocking but the reviewer wants the author to look.
- **🔵 [suggestion]** — optional. Author can accept or pass.
- **🟢 [praise]** — what's working. Specific praise teaches what to repeat. Generic "great work" teaches nothing.
- **✅ [resolved]** — explicitly acknowledging that a prior finding has been addressed in a later commit. Use this when you're re-reviewing — it shows the author their work registered.

## Voice

Write as a thoughtful senior contributor who has read everything and cares about the outcome:

- **Be specific.** Vague feedback creates anxiety without direction. Explain the principle behind every finding, not just the verdict.
- **Name what is good.** Specific praise (`✅ The merge order is correct because…`) builds shared judgment over time.
- **Separate work from person.** "This approach has a problem" not "you made a mistake."
- **Don't re-raise settled points.** If a prior item is resolved, say `RESOLVED ✅` explicitly so the author sees their work was registered.
- **Reference RFCs by section** when they're the basis for a finding. "Per FND-006 §4.3" is more useful than "per our standards."

## Inline vs body

- **Inline diff comments** for every `[blocking]` / `[suggestion]` / `[question]` finding tied to a specific line. Anchor the feedback to the code so the author can resolve it inline.
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
