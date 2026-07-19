# PR Review Protocol

This is the procedure followed when reviewing a pull request in `zeroclaw-labs/zeroclaw`. It's loaded by the `github-pr-review-session` skill and read by human reviewers, it's authoritative for both.

The `gh` CLI is assumed available and authenticated.

## Fetch order

Run all of these. The data informs every step that follows.

1. **PR overview**

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh pr view <number> --repo zeroclaw-labs/zeroclaw
   ```

   </div>

   Description, labels, linked issues, validation evidence.

2. **Top-level conversation**

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh pr view <number> --comments --repo zeroclaw-labs/zeroclaw
   ```

   </div>

3. **Inline threads (every reply chain)**

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/comments --paginate
   ```

   </div>

   Read full reply chains before drawing any conclusion about whether something is open or settled. Note author commitments made in replies, they're load-bearing.

4. **Formal reviews**

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/reviews --paginate
   ```

   </div>

   Note which `CHANGES_REQUESTED` are still active (not superseded by a later `APPROVED` or `DISMISSED`). Check whether you've already reviewed this PR.

5. **Relevant foundations documents**

   Always read FND-005 (Contribution Culture). For others, use the relevance
   table below, read what applies to the PR's scope. The ratified versions
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

   <div class="os-tabs-src">

   #### sh

   ```sh
   gh pr diff <number> --repo zeroclaw-labs/zeroclaw
   ```

   </div>

   Read the full diff. Cross-check author commitments from step 3 against what actually shipped. Cross-check against the local repository where the change lands.

## Take stock before writing

Before you write a single line of review, name out loud:

- What's been raised already (across reviews, inline threads, top-level comments).
- What's settled (resolved by author, dismissed by reviewer, addressed in a later commit).
- What's still live (open blockers, unresolved questions, things the author committed to but didn't ship).
- Who holds active blocks, and whether the diff addresses them.
- Whether any obvious PR-template, public metadata, or body-claim gaps affect
  the verdict. Run the full template/truthfulness check before approving.

The take-stock pass is what stops you from re-raising settled points and what surfaces who's actually waiting on what.

## Label hygiene

Labels are maintainer metadata, not a contributor blocker. If the right label is obvious and you have permission, fix it yourself before finalizing the review. If you are acting through an assistant, draft the exact label change and get the human reviewer's approval before mutating GitHub.

Ask the author about labels only when the right label choice is ambiguous or nobody with label permissions is available. Do not request changes or hold merge solely because an author cannot edit labels.

If your request-changes review leaves the next step on the author, include `needs-author-action` in the review posting packet. Skip it when the requested cleanup is maintainer-owned, another maintainer is taking over the branch, or the PR is waiting on a maintainer decision rather than author work.

## Template and public artifact checks

Before approving, compare the live PR body against the current
`.github/pull_request_template.md`. The template is the source of truth: check
every required and applicable prompt, including conditional sections. Custom
narrative is fine only when it still satisfies that template contract.

Missing required substance is a review finding. If the content is present but
the heading or placement needs mechanical cleanup, and a maintainer can safely
repair it, fix or propose the exact cleanup instead of making the author do
metadata work. When acting through an assistant, show the exact PR-body or
metadata diff and get human reviewer approval before mutating GitHub. If the
missing section is substantive, unsupported, or changes reviewer confidence, do
not approve until it is filled.

Also run a truthfulness scrub on the public artifacts before choosing a
verdict:

- Live labels match the PR body's label snapshot and the diff's real risk,
  size, and type.
- Linked issue verbs are accurate: use `Closes` / `Fixes` / `Resolves` only
  when the PR fully resolves the issue; otherwise use `Related`, `Depends on`,
  or `Supersedes`.
- Behavior claims are checked against the controlling contract: the relevant architecture doc, source-of-truth module, trait boundary, existing test, public API shape, source comment, or explicit maintainer decision. Issue-fit alone is not enough.
- Provenance claims are real. If the PR body, commits, docs, or review thread cite an RFC, audit, issue, PR, path, generated artifact, or follow-up finding, verify that the artifact exists and supports the claim.
- Validation evidence names the checks being relied on: required CI, focused local tests, manual smoke, docs/link gates, or full workspace checks when broad coverage proves something narrower evidence would miss. Commands that ran include relevant output or an honest skip reason. Fresh required CI is valid evidence when it covers the changed surface; do not require duplicate local Cargo for the same head, target, and feature set. Pending CI is not evidence yet.
- Security/privacy, compatibility, rollback, and scope-boundary claims match
  the diff and current behavior.
- Public text does not include bot/AI attribution footers, local workflow
  mechanics, private paths, unredacted sensitive logs, excessive raw logs,
  irrelevant dumps, or stale lifecycle wording. Concise, relevant command
  output tails in `How I tested` are expected when the template asks for
  them.

## Verdict decision tree

| Situation | Verdict flag |
|---|---|
| Your review is approving, the template/truthfulness checks are satisfied, and prior substantive concerns are resolved, dismissed, stale, or explicitly reconciled in your review | `--approve` |
| Your review is rejecting on substantive grounds you'd block on personally | `--request-changes` |
| You have nothing new to block on but other reviewers hold unresolved substantive concerns | `--comment` |
| You have specific findings but they're all 🔵 suggestions or non-blocking clarification questions | `--comment` |

Do not ignore another reviewer's visible `CHANGES_REQUESTED`. Before approving, check whether the underlying concern is resolved in the current diff, stale, dismissed, or still valid. A review state left on an older head is not automatically an unresolved concern. If you approve while that state is still visible, explain why the concern has been resolved; your approval does not clear the other review state for merge.

## Validation evidence gaps

When validation is the concern, identify the exact evidence gap instead of asking for "full Cargo" by reflex. Check the current required CI jobs and the changed surface, then ask for extra validation only where required CI does not prove the thing under review: tests for a platform that only received compile checks, Clippy for a platform or path outside the required lint job, desktop coverage when the desktop workflow did not trigger, release targets outside the PR matrix, stale CI, or unavailable CI.

## Shape and generated artifacts

For `size:XL`, over-1k-line, or new channel/provider/tool-family PRs, review the diff shape before relying on CI or prior approval. The public review should say whether the size is justified, whether the slice is merge-justified now, whether it could reasonably be split, and whether the handwritten work is mostly new value rather than duplicated machinery.

Do not dismiss generated artifacts as harmless because they are generated. If a checked-in generated file affects policy, schema, routes, migrations, lockfiles, release artifacts, capabilities, packages, runtime behavior, or reviewer evidence, review it like source and ask the PR to explain the provenance when that provenance matters.

## Feedback taxonomy

Findings in review bodies and inline comments use this PR-review scale, adapted from FND-005. The `✅ [resolved]` entry is for re-reviews that acknowledge addressed findings.

- **🔴 [blocking]**: must be addressed before merge. Use sparingly; every blocker is real or the scale loses meaning.
- **🟡 [warning]**: should be addressed; not blocking but the reviewer wants the author to look.
- **🔵 [suggestion]**: optional. Author can accept or pass.
- **🟢 [praise]**: what's working. Specific praise teaches what to repeat. Generic "great work" teaches nothing.
- **✅ [resolved]**: explicitly acknowledging that a prior finding has been addressed in a later commit. Use this when you're re-reviewing, it shows the author their work registered.

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
- **Bare commit hashes** (never wrap in backticks: GitHub auto-links bare hashes; backticks block the auto-link).
- **`@`-prefixed usernames** in all review content (chat, body, inline). `@WareWolf-MoonWall`, not `WareWolf-MoonWall`.

## Posting

Write the review body to a file under `tmp/review-<number>.md` first: this is the source of truth for what was posted and lets the user inspect before publishing. Then:

<div class="os-tabs-src">

#### sh

```sh
gh pr review <number> --repo zeroclaw-labs/zeroclaw \
  <--approve | --request-changes | --comment> \
  --body-file tmp/review-<number>.md
```

</div>

Always show the full draft and get explicit approval from the human before posting. Continuation words like "next" or "move on" don't count as approval, only an unambiguous "yes" / "approve" / "go" does.

## After posting

If a session-level handoff file exists (`tmp/handoff.md`), update it with the verdict, the head commit reviewed, and what remains open. The handoff is what lets a new session pick up cold without re-reading the whole conversation.

## Never

- **Never approve without resolving or explaining why another reviewer's active `CHANGES_REQUESTED` concern has been resolved.**
- **Never post a review that re-raises a settled point** without explicitly noting it's already resolved.
- **Never merge.** That's a separate decision and a separate skill.
- **Never push to contributor branches** without explicit instruction. `maintainerCanModify: true` allows it; even then, ask before pushing anything other than trivial fixups.
