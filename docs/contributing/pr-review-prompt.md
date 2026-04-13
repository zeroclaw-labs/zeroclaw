# PR Review Prompt

You are reviewing a pull request in the `zeroclaw-labs/zeroclaw` repository.
The GitHub CLI (`gh`) is available and authenticated. Use it to fetch all
content and to post your review — do not ask the user to paste anything.

---

## Authority hierarchy

The ZeroClaw Maturity Framework is the canonical authority for all review
decisions. It comprises six ratified RFCs that currently live in GitHub Issues
while `docs/foundations/` awaits completion. Fetch the ones whose scope
intersects this PR. Always fetch #5615 — it is your primary reference for
how to review, not just what buckets to use.

| RFC | Issue | Covers |
|-----|-------|--------|
| Microkernel Architecture    | #5574 | Crate structure, dependency rules, feature gates |
| Documentation Standards     | #5576 | Docs IA, i18n parity, knowledge transfer |
| Team Governance             | #5577 | Decision authority, coordination |
| CI/CD Pipeline              | #5579 | Workflows, release automation, action pinning |
| Contribution Culture        | #5615 | Review taxonomy, feedback discipline — always fetch |
| Zero Compromise in Practice | #5653 | Code health, error discipline, production readiness |

`docs/contributing/` files may be outmoded by the RFCs. Only cite them if
they are explicitly referenced in RFC content you have read in this session.
Do not cite documents reached via `AGENTS.md` alone — that file predates the
foundations folder and has not been updated to point to the ratified RFCs.

---

## Process

1. `gh pr view <number> --repo zeroclaw-labs/zeroclaw`
   Read the description, labels, linked issues, and validation evidence.

2a. `gh pr view <number> --comments --repo zeroclaw-labs/zeroclaw`
    Read the PR-level conversation: top-level timeline comments, and any
    high-level decisions or commitments made outside of review threads.

2b. `gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/comments --paginate`
    Read all inline review comments — the line-level back-and-forth on the
    diff itself. This is where prior findings, author explanations, and
    contested points most commonly appear. For each comment, note the
    `path`, `line`, `in_reply_to_id` (to reconstruct reply chains), and
    the comment body. Read every reply in a thread before drawing any
    conclusion about whether the thread is live or settled.

2c. `gh api repos/zeroclaw-labs/zeroclaw/pulls/<number>/reviews --paginate`
    Read all formal review submissions. Note each reviewer's verdict
    (APPROVED / CHANGES_REQUESTED / COMMENTED), the body of their summary,
    and whether a later APPROVED or DISMISSED event superseded an earlier
    CHANGES_REQUESTED. A verdict that has been superseded should not be
    treated as a current blocker. From this data, produce a list of
    reviewers who currently hold an active block — a CHANGES_REQUESTED
    that has not been superseded by their own subsequent APPROVED or a
    DISMISSED event. Record their names; you will need this in step 7.
    Also check whether you have already submitted a review on this PR and
    what your prior verdict was. If you have an existing CHANGES_REQUESTED,
    note what it covered so you do not file a duplicate.

2.5 Before writing a single finding, build a thread inventory:
    - List every inline thread: what was raised, who replied, what the
      author said, and whether the thread appears open or settled.
    - List every formal review verdict that is still in effect.
    - Flag any author commitment made in a reply ("I'll address this",
      "fixing in the next commit", etc.) — these must be cross-checked
      against the diff in step 4.
    - List all active blocks from prior reviews (from step 2c): who holds
      them and whether the diff appears to have addressed the underlying
      concern. You can observe that the diff appears to address a prior
      concern, but you cannot close the block on the reviewer's behalf —
      only they can do that.
    Do not skip this step. Findings written without it routinely re-raise
    resolved points or miss the actual open disputes.

3. `gh issue view <RFC-number> --repo zeroclaw-labs/zeroclaw`
   Fetch each relevant RFC. Read it before citing it. Always fetch #5615.
   Fetching RFCs before reading the diff ensures you apply the evaluative
   framework from the start rather than retrofitting it to prior impressions.

4. `gh pr diff <number> --repo zeroclaw-labs/zeroclaw`
   Read the full diff.

5. Cross-check code claims against the local repository where needed.
   Also cross-check any author commitments identified in step 2.5 — verify
   whether the diff actually delivers on them. A commitment without a
   corresponding change is a gap worth flagging.

6. Write your review. For each finding, decide:
   - Is this new, not yet raised by anyone in any thread?
   - Does this corroborate an open, unresolved thread with additional
     evidence? If so, say so explicitly — name the thread and note that
     your evidence adds weight to it. Corroboration is valuable; repetition
     is noise.
   - Has this been covered adequately and settled (author addressed it or
     gave a satisfactory explanation that was accepted)? If so, do not
     repeat it.
   - Did the author commit to addressing this in a comment, but the diff
     does not reflect that commitment? If so, flag the gap specifically:
     quote the commitment and describe what is still missing.

7. Before choosing a verdict, consult your thread inventory for active
   blocks held by other reviewers.

   - If your own findings include blockers: use `--request-changes`.
     Declare your findings regardless of what other reviewers have already
     blocked on. The existence of prior blocks does not suppress yours.
   - If your own findings are clear but active blocks from other reviewers
     remain: use `--comment`. Name each blocking reviewer and state that
     their block stands and only they can clear it.
   - If your own findings are clear and no prior blocks are active:
     use `--approve`.
   - Never use `--approve` while another reviewer's CHANGES_REQUESTED is
     in effect, even if you have no findings of your own.

   Write the review to a temp file, post it with the appropriate verdict
   flag, then delete the temp file:
   ```
   gh pr review <number> --repo zeroclaw-labs/zeroclaw \
     <verdict-flag> --body-file <file>
   ```
   where `<verdict-flag>` is `--request-changes`, `--approve`, or
   `--comment` per the decision above.

---

## Discipline

RFC #5615 is your reference for how to apply the taxonomy. Read it. The
prompt does not summarise it — the full text contains the judgment that
makes the structure useful.

---

The PR to review is: #