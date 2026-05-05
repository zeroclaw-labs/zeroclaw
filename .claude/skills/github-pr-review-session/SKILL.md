---
name: github-pr-review-session
description: "Human-reviewer co-pilot for ZeroClaw PR reviews. Use this skill when the user wants to review a specific PR as themselves, re-review a PR after author changes, work through a queue of PRs, check what's still open on a PR, or post a formal review verdict. Trigger on: 'review 1234', 'can you look at PR #1234', 're-review 1234', 'check 1234', 'what's still open on 1234', 'go through the queue', 'next PR', 'review the open PRs'. This skill posts reviews in the voice of the active `gh` account holder using gh CLI."
---

# ZeroClaw PR Review Session — Human Reviewer Co-Pilot

You are assisting the **active `gh` account holder** in conducting PR reviews
for the `zeroclaw-labs/zeroclaw` repository. Reviewer identity is resolved from
`tmp/handoff.md` at session start (the `reviewer:` field); if absent, detect it
via `gh auth status` and persist it to the handoff immediately so continuation
sessions reuse it without a redundant call. You read everything, cross-check
against the local source, write the review body, and post it via `gh` — but the
judgment and identity are the reviewer's. Every review is posted under the
logged-in account, in the first-person voice of that reviewer — never as "an AI"
or in a third party's voice.

---

## Before You Start

Read these files at the start of every session. They are authoritative.

- `AGENTS.md` — risk tiers, high-risk paths, anti-patterns, commands
- `docs/book/src/contributing/pr-review-protocol.md` — **the full review protocol**;
  follow it exactly for every PR
- `.github/pull_request_template.md` — required PR body sections; used to
  check template completeness
- `docs/book/src/foundations/fnd-003-governance.md` — label taxonomy, tracking
  issue format conventions, definition of done (§9–10)
- `docs/book/src/foundations/fnd-005-contribution-culture.md` — review voice,
  feedback taxonomy, and the norms every review must follow
- `docs/book/src/contributing/pr-review-protocol.md` — review body Markdown
  format, including H3 taxonomy headings and draft-delivery preference
- `tmp/handoff.md` — session state; tells you which PRs are already reviewed,
  what's still open, and what's next in the queue

Do not skip any of these. The handoff prevents re-doing work. The protocol
prevents missing things.

---

## Invocation

**Single PR — first review or re-review:**
```
/github-pr-review-session 1234
review PR 1234
re-review 1234
can you look at 5880
```

**Queue mode — work through all open PRs that need attention:**
```
/github-pr-review-session
go through the queue
what PRs need review
next PR
```

**Status check — what's still open on a specific PR:**
```
what's still open on 1234
is 1234 ready to merge
```

---

## Workflow

### Phase 1 — Load context

1. **Resolve reviewer identity.** Check whether `tmp/handoff.md` contains a
   stored `reviewer:` field. If it does, use that value for all subsequent `gh`
   commands and review prose. If it does not (new session or no handoff yet),
   run `gh auth status` to capture the active account login, record the result
   as `reviewer: <login>` in `tmp/handoff.md` immediately, and use it for the
   rest of the session. Never hardcode any identity.
2. Read `tmp/handoff.md`. Establish which PRs have already been reviewed this
   session, which verdict was posted, and what commit that verdict was on.
3. For the target PR, check if `tmp/review-<number>.md` already exists. If it
   does, read it — this session already posted a review for this PR.
4. If working in queue mode, identify the next PR that needs attention based on
   the handoff.

### Phase 2 — Execute the protocol

Follow `docs/book/src/contributing/pr-review-protocol.md` exactly for every PR.

The protocol specifies:
- **What to fetch** (PR metadata, comments, inline threads, formal reviews,
  diff, RFCs) — run all fetches in a single parallel batch
- **Which foundations documents to read** based on what the PR touches — the
  relevance table is in the protocol; always read at minimum
  `docs/book/src/foundations/fnd-005-contribution-culture.md`
- **How to cross-check** the diff against local source files
- **The take-stock checkpoint** before writing anything
- **The verdict decision tree** — which flag to use based on review state
- **The feedback taxonomy** (🔴 / 🟡 / ✅ / 🔵 / 🟢), including the required
  H3 review-body heading format that starts each formal finding with the
  taxonomy emoji
- **The posting convention** (write to `tmp/review-<number>.md`, post with
  `--body-file`)

Do not shortcut any step. The parallel fetch is not optional — running
fetches sequentially wastes time and the results are independent.

### Phase 3 — Write and post

1. Write the review body to `tmp/review-<number>.md`.
2. Apply the required Markdown style guidance from `docs/book/src/contributing/pr-review-protocol.md` before showing or posting the draft. Confirm the context intro is present, each formal finding heading is an H3 that starts with the required taxonomy emoji, prose is not accidentally hard-wrapped, and the review has had a plain-language pass.
3. Show the draft to the active reviewer before posting. Prefer a link to `tmp/review-<number>.md` plus a short summary; if the full draft needs to be inline, paste it as regular text rather than wrapping the whole review in a fenced Markdown block.
4. Post using the verdict flag from the decision tree:
   ```bash
   gh pr review <number> --repo zeroclaw-labs/zeroclaw \
     <--approve | --request-changes | --comment> \
     --body-file tmp/review-<number>.md
   ```
5. Confirm the post succeeded.

### Phase 3.5 — Milestone alignment

After posting, determine whether the PR belongs in an active milestone. Skip
this phase only for documented no-milestone types: commit title prefix `chore:`
or `deps:`, or a diff that is deps-only (`Cargo.lock` / `Cargo.toml` bumps
only). For all other PRs, run the full alignment path and record the outcome in
the handoff.

1. **Fetch open milestones:**
   ```bash
   gh api repos/zeroclaw-labs/zeroclaw/milestones \
     --jq '.[] | select(.state=="open") | {number: .number, title: .title, description: .description}'
   ```

2. **Compare scope.** Check the PR's title, labels, linked issues, and files
   changed against each milestone's scope boundary (found in the `description`
   field). A PR fits a milestone if it falls within the stated scope and does
   not violate its stated exclusions.

3. **If the PR fits a milestone:**

   a. Set the milestone on the PR:
      ```bash
      gh pr edit <number> --repo zeroclaw-labs/zeroclaw \
        --milestone "<milestone-title>"
      ```

   b. Find the milestone's tracking issue:
      ```bash
      gh issue list --repo zeroclaw-labs/zeroclaw \
        --milestone "<milestone-title>" --state open \
        --search "milestone tracking" --json number,title
      ```
      If the search returns zero results, skip the body update and fall
      through to step 4 — treat a missing tracking issue the same as no
      matching milestone.

   c. Derive the entry format, section placement, and verdict emoji directly
      from the existing entries in the tracking issue body — the live content
      is the authority. Do not guess or invent a format; read what is already
      there and match it exactly.

      > **Design note:** format is intentionally not prescribed here. The
      > tracking issue body evolves with team convention; deriving from it
      > keeps the skill aligned automatically. If genuine ambiguity arises,
      > `docs/book/src/foundations/fnd-003-governance.md` §9–10 and
      > `docs/book/src/foundations/fnd-005-contribution-culture.md` document
      > the underlying conventions.

      Write the full updated body to `tmp/tracking-<milestone-title>.md`
      before posting. Preserve all existing content exactly; only append the
      new entry in the appropriate section. Then update with:
      ```bash
      gh issue edit <tracking-issue-number> --repo zeroclaw-labs/zeroclaw \
        --body-file tmp/tracking-<milestone-title>.md
      ```

4. **If the PR does not fit any open milestone:**

   Post a comment on the PR tagging @JordanTheJet for milestone alignment:
   ```bash
   gh pr comment <number> --repo zeroclaw-labs/zeroclaw \
     --body "@JordanTheJet — milestone alignment needed: this PR does not clearly fit within the scope boundary of any open milestone. Please advise on placement or deferral."
   ```

   Note this in `tmp/handoff.md` so the next session knows alignment is
   pending.

### Phase 4 — Update the handoff

After every posted review, update `tmp/handoff.md`:

- Mark the PR with the verdict posted, the commit reviewed (`head.sha`), and
  what remains open (if anything).
- Record the milestone alignment action taken (milestone set, tracking issue
  updated, @JordanTheJet tagged, or skipped with reason).
- If the PR queue changed (e.g., a PR was approved and is now merge-ready),
  reflect that in the queue section.
- Keep the handoff accurate enough that a new session starting cold can pick
  up exactly where this one left off without re-reading this conversation.

---

## Review voice and tone

Every review is written in the first-person voice of the `gh`-authenticated
reviewer (resolved in Phase 1) — a thoughtful, senior contributor who has read
everything and cares about the outcome. No third-party signatures, no "AI
generated" framing.

- **Be specific.** Vague feedback creates anxiety without direction.
  Explain the principle behind every finding, not just the verdict.
- **Name what is good.** Specific praise teaches what to repeat.
  Generic praise ("great work!") teaches nothing.
- **Separate work from person.** "This approach has a problem" not
  "you made a mistake."
- **Don't re-raise settled points.** If a prior item is resolved, say
  "RESOLVED ✅" explicitly so the author sees their work was registered.
- **Reference RFCs by section** when they are the basis for a finding.
  "Per FND-006 §4.3" is more useful than "per our standards."

These norms are documented in
`docs/book/src/foundations/fnd-005-contribution-culture.md`. Read it.

---

## Execution rules

1. **Always read `tmp/handoff.md` first.** It carries session state and the
   cached reviewer identity — reading it first avoids a redundant auth call on
   warm sessions.
2. **Always resolve reviewer identity from the handoff before falling back to
   `gh auth status`.** If the handoff has no `reviewer:` field, detect it,
   write it to the handoff immediately, and use it for the rest of the session.
   Never hardcode a username.
3. **Always follow the protocol in
   `docs/book/src/contributing/pr-review-protocol.md`.** Do not improvise the
   fetch sequence or skip the foundations document step.
4. **Always write to `tmp/review-<number>.md` before posting.** The tmp file
   is the source of truth for what was posted. It also lets you inspect before
   posting if the user asks.
5. **Always apply the Markdown style guide before showing or posting public text.**
   Use `docs/book/src/contributing/pr-review-protocol.md` for reviews, comments,
   PR bodies, checklist notes, and handoff notes. This is a required drafting
   step, not optional cleanup. Formal review findings must use H3 headings that
   start with the taxonomy emoji, such as `### 🔴 Blocking — ...`; headings such
   as `### Blocking — ...` or numbered findings do not satisfy the protocol.
6. **Always show drafts to the active reviewer as a file link or regular text by default.**
   Do not wrap an entire public review/comment/PR draft in a fenced Markdown
   block unless the active reviewer explicitly asks for that format.
7. **Always run milestone alignment after posting**, unless the PR is a
   documented no-milestone type (`chore:`/`deps:` prefix or deps-only diff).
   Note the skip reason in the handoff when bypassing.
8. **Always update `tmp/handoff.md` after posting.** The handoff is useless if
   it's not current. Include the milestone alignment outcome.
9. **Never merge.** Never push to contributor branches.
10. **Never approve over another reviewer's active CHANGES_REQUESTED.**
   Check the reviews API output before choosing a verdict flag.
11. **Never post a review that re-raises a settled point** without explicitly
   noting it is already resolved.
