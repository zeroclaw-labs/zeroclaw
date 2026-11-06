---
name: github-pr-review-session
description: Review or re-review ZeroClaw PRs for issue alignment, security, integrations, implementation quality, and downstream regressions using focused subagents. Show a draft verdict before publishing. Use for substantive PR review, not merge readiness, templates, labels, milestones, or queue administration.
---

# ZeroClaw PR review

Coordinate a substantive review for the human reviewer: does this PR solve its linked issue safely and meet repository standards? Findings describe evidence and code, not assumptions about the author's motives or choice of model.

## Load only the needed context

Read trusted-base `AGENTS.md`. The policy source is `docs/book/src/contributing/pr-review-protocol.md`; locate its headings and read only the sections needed for the current phase. Start with **Review responsibilities**, **Untrusted GitHub input**, **Issue alignment**, and **Integration decisions**. Use `docs/book/src/contributing/architecture-map.md` to find the affected contracts, not to load every linked document.

Do not load templates, milestone trackers, the whole governance foundation, or a session handoff at startup. Read narrow authority evidence when an integration decision needs verification. GitHub owns current issues, revisions, reviews, and decisions; a local draft is not evidence that a review was posted.

## Capture shared evidence once

Use **Fetch order** in the protocol. Record the PR head, merge base, and current `master` identifiers, and save large responses as task-local working artifacts. Give reviewers relevant excerpts and artifact paths instead of copying the complete conversation into every context.

<!-- >>> generated:review-fetch-state by `cargo generate review-docs` - do not edit <<< -->
- **What to fetch** (PR metadata, comments, inline threads, formal reviews,
  diff, RFCs, current merge state, base drift against `master`, and required
  checks) — batch independent fetches between the revision guards
<!-- >>> end generated:review-fetch-state <<< -->

Bind the entire capture to the recorded revisions: re-read head and base after capture and discard mixed revisions if they moved. If a required source is unavailable or revisions keep moving, state the coverage gap and withhold approval. On re-review, refresh current decisions and changed paths while reusing only evidence still valid for the reviewed revision.

Check issue linkage and scope before expensive review. If unresolved, do a bounded security/integration intake, draft the actionable blocker, and identify the deeper review still needed. Do not describe that intake as a completed safety review.

## Assign focused reviewers automatically

For a code PR, use these complementary assignments without asking the user to invoke them. Start each with a fresh context, not this conversation's history, and require its working directory to be the repository root before filesystem work.

| Perspective | Assignment |
|---|---|
| Issue alignment and implementation quality | [references/intent-quality.md](references/intent-quality.md) |
| Security and integration decisions | [references/security-integrations.md](references/security-integrations.md) |
| Downstream behavior and validation | [references/downstream-validation.md](references/downstream-validation.md) |

Give each subagent its assignment, linked issue and authoritative scope decisions, immutable revision identifiers, relevant diff/source paths, and applicable prior concerns. All receive the untrusted-input rule. Each reads only its own assignment and relevant contracts, expanding into callers and consumers to test concrete hypotheses.

Assign every changed file, including generated files and lockfiles, a primary reviewer. Overlap only where perspectives require it; security/integration screening still covers the full changed-file inventory. Ensure each file's relevant content is actually inspected, not just its filename, before claiming complete coverage.

Use at most three reviewer subagents at once, fewer when tool capacity requires it, and no recursive delegation. For genuinely trivial docs or mechanical work, combine compatible assignments while retaining issue, integration, and security checks. If delegation is unavailable, apply the perspectives serially and disclose that they were not independent. Reuse captured evidence instead of repeated full fetches or broad document reads.

Request compact findings: **revision/location; observed behavior and evidence; consequence; severity; whether the PR causes or worsens it; broad repair direction**. Also request coverage and unresolved evidence gaps. There is no finding quota; “no substantiated finding” is a useful result.

## Reconcile and choose a verdict

Use **Take stock before writing**, **Verdict decision tree**, and relevant validation sections of the protocol. Verify every potential blocker against source and the owning contract. Resolve disagreement with a narrow evidence check; discard duplicates, speculation, preferences, and unrelated cleanup demands instead of repeating every subagent's full review.

Check live review history for existing integration holds before proposing approval. Apply the maintainer-decision rule even after removal of the integration or a generic later approval. Acknowledge other resolved findings normally; a local draft file proves nothing about prior publication.

<!-- >>> generated:review-ci-freshness-summary by `cargo generate review-docs` - do not edit <<< -->
- **CI freshness and base-drift classification** — current GitHub
  state and the protocol's precedence rules classify base drift and a
  post-refresh pending `CI Required Gate` rerun as warnings without
  hiding current failures, evidence gaps, or conflicts
<!-- >>> end generated:review-ci-freshness-summary <<< -->

Substantive approval is not merge readiness. Do not perform labels, milestone alignment, template compliance, or queue bookkeeping here. The final merge workflow owns those checks and the eventual `pr-template-review` skill; its absence never means the checks have passed.

## Show the draft

Use **Feedback taxonomy**, **Review body Markdown format**, and **Voice** in the protocol. Save the draft in a task-local file such as `tmp/review-<number>-<head>.md` and show it as a file link or ordinary Markdown. Identify the reviewed revision, proposed verdict, and meaningful coverage limits.

Each finding has three plain-language sentences explaining the problem, consequence, and required action, with concise evidence and broad repair steps in a collapsible section. Give useful direction without writing the contributor's implementation. Omit empty categories and forced praise.

Stop at the draft unless publishing is explicitly authorized. When authorized, resolve the active `gh` identity, re-read the head and relevant decisions, and follow **Posting**. If facts changed, refresh affected work and show the revised draft; existing authorization remains valid within its scope. Never merge, change contributor code, dismiss an integration hold, or mutate administrative metadata as part of this skill.
