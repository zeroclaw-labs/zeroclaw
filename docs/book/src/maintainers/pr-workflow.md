# PR Workflow

The maintainer-side governance contract for PRs targeting `master`. Branch-protection settings, the DoR/DoD readiness contracts, and the failure-recovery protocol live here. Day-to-day reviewing lives in the [Reviewer Playbook](./reviewer-playbook.md). The contributor-facing flow lives in [How to contribute](../contributing/how-to.md).

## Governance goals

The workflow exists to keep five things true under high PR volume:

1. Merge throughput is predictable.
2. CI signal quality stays high — fast feedback, low false positives.
3. Security review is explicit on risky surfaces.
4. Changes are easy to reason about and easy to revert.
5. Repository artifacts stay free of personal or sensitive data.

The control loop that delivers this is layered on purpose:

- **Intake classification** — path/size/risk labels route the PR to the right depth.
- **Deterministic validation** — the merge gate depends on reproducible checks, not subjective comments.
- **Risk-based review depth** — high-risk paths get deep review, low-risk paths stay fast.
- **Rollback-first merge contract** — every merge path includes a concrete recovery story.

Automation handles intake labels and CI gating. Final merge accountability stays with human maintainers and PR authors.

## Required repository settings

Branch protection on `master`:

- Require status checks before merge.
- Require check `CI Required Gate`.
- Require pull request reviews before merge.
- Require CODEOWNERS review for protected paths.
- For `.github/workflows/**`, require owner approval via `CI Required Gate` (`WORKFLOW_OWNER_LOGINS`); keep branch / ruleset bypass limited to org owners.
- Default workflow-owner allowlist is configured via the `WORKFLOW_OWNER_LOGINS` repository variable (see CODEOWNERS for the current list).
- Dismiss stale approvals when new commits are pushed.
- Restrict force-push.
- All contributor PRs target `master` directly.

## Definition of Ready (DoR)

Before requesting review, the PR has all of these:

- PR template fully completed.
- Scope boundary explicit (what changed / what did not).
- Validation evidence attached — actual command output, not "CI will check."
- Security & privacy and rollback fields completed for risky paths.
- Privacy and data-hygiene rules satisfied — neutral, project-scoped test wording. See [Privacy](../contributing/privacy.md).
- Identity-like wording, where unavoidable, uses ZeroClaw / project-native labels.

## Definition of Done (DoD)

Before merge:

- `CI Required Gate` is green.
- Required reviewers approved (including any CODEOWNERS paths).
- Risk labels match touched paths. See [Labels](./labels.md).
- Migration / compatibility impact is documented.
- Rollback path is concrete and fast.

## Maintainer merge process

Merging is a separate maintainer action from reviewing. A PR can be approved but still not ready to merge if the branch moved, CI restarted, labels are stale, rollback is unclear, or the final squash message would misrepresent the work.

Use this process immediately before landing any PR.

### 1. Refresh live state

Before deciding anything, refresh the PR from GitHub. Do not rely on an old review tab, local queue snapshot, or earlier CI status.

Verify:

- PR is open, non-draft, and targets `master`.
- Head SHA is the one you intend to merge.
- Merge state is clean / mergeable.
- `CI Required Gate` is green.
- Required reviewers approved, including CODEOWNERS paths.
- No active `CHANGES_REQUESTED` review remains.
- No unresolved inline thread contains a merge-blocking question.
- Risk, size, and scope labels match the final touched paths.
- PR body still matches the final diff, validation evidence, security/privacy impact, compatibility notes, and rollback plan.
- Docs-quality checks are green when docs changed.
- Linked issues / stacked PRs / superseded PRs are understood and named correctly.

If a maintainer pushed a fixup after approval, re-check review staleness and CI after that commit. Do not assume the previous ready state survived the new head.

### 2. Decide whether a fixup is needed

If the PR is conceptually ready but has a tiny mechanical defect, prefer the lightest correction that preserves contributor attribution:

1. Push a trivial maintainer fixup to the contributor branch when `maintainerCanModify` is true and the fix is clearly within the PR's scope.
2. Ask the author for changes when the fix is non-trivial, design-sensitive, or likely to conflict with their local work.
3. Merge as-is and open a follow-up only when the current PR is correct and the follow-up is hardening or polish.
4. Supersede only when the [superseding rules](./superseding.md) apply.

After any fixup, return to step 1.

### 3. Choose the merge strategy

ZeroClaw's default and expected merge strategy is **squash merge for all PRs**.

Use squash merge for:

- Contributor PRs.
- Docs-only changes.
- Bug fixes and feature slices.
- PRs with maintainer fixup commits.
- PRs with iterative, messy, or review-response commits.
- Any PR where "one PR = one revertable commit" is the right history model.

Use a regular merge only with an explicit maintainer exception, and document why before merging. Valid exceptions are rare:

- A long-lived integration branch where the branch topology is itself meaningful.
- A release, backport, or stabilization branch where preserving merge ancestry matters.
- A deliberately structured multi-commit series where each commit must remain separately bisectable on `master`.

If a normal PR seems to need regular merge because it contains several independently meaningful changes, pause and consider splitting or stacking it instead. Do not use regular merge just to preserve messy review history.

Never direct-push a squash commit to `master`. Direct pushes bypass the PR merge mechanism, so the PR shows as closed instead of merged and linked issues may not close correctly. Do not use GitHub's web UI auto-generated squash message either; it does not consistently preserve the project commit-message format.

### 4. Prepare the squash message

The squash subject must be a conventional commit and include the PR number:

```text
<PR title> (#<number>)
```

If the PR title is not a good conventional commit subject, fix the title or prepare a corrected subject before merging.

The squash body preserves the PR's commit history:

- For a multi-commit PR, include one bullet per PR commit: `- <short sha> <commit subject>`.
- For a single-commit PR, use the original commit body, or leave the squash body blank if there is no useful body.
- If maintainer fixups were pushed, include them in the preserved commit list rather than hiding them.
- If superseded work was materially incorporated, include the required `Co-authored-by` trailers and supersede notes from [Superseding PRs](./superseding.md).

For a PR with contributor commits plus maintainer fixups, the final `master` history should read as one coherent project change while the body still makes the branch history auditable.

### 5. Confirm before merging

Before running the merge command, show the maintainer the exact expanded command, final subject, final body, target PR, and effect. Explicit approval for the review, the fixup, or the checklist is not enough; merging needs its own confirmation.

The confirmation should state:

- PR number and title.
- Head SHA being merged.
- Merge strategy: squash.
- Final squash subject.
- Final squash body.
- Expected effect: PR becomes merged, linked issues may close, and a new commit lands on `master`.

Stop if the maintainer changes the subject/body, asks a question, or redirects the task. Resume only after the final command has been shown again and approved.

### 6. Execute through the PR merge mechanism

Use `gh pr merge` with an explicit subject and body:

```bash
gh pr merge <number> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "<subject>" \
  --body "<body>"
```

If the command fails, report the error and stop. Do not retry, force-push, close/reopen, dismiss reviews, bypass checks, or use a different merge strategy unless a maintainer explicitly chooses that recovery path.

### 7. Verify and clean up

After the command succeeds, verify:

- PR state is `MERGED`.
- Merge commit SHA is present.
- The final commit subject/body match what was approved.
- Linked issues closed only when that was intended.
- Any local handoff or queue document that was tracking the PR is updated or cleared.

Never delete contributor branches. Branch cleanup is the contributor's choice.

### Stop conditions

Stop before merge if any of these are true:

- CI Required Gate is missing, pending, or failing.
- Merge state is dirty, blocked, or unknown.
- Required review is missing.
- Any active `CHANGES_REQUESTED` review remains.
- CODEOWNERS or workflow-owner approval is missing for protected paths.
- The PR body materially contradicts the diff or validation evidence.
- Risk labels are clearly wrong for the touched paths.
- Privacy/security fields are incomplete or the diff contains unredacted sensitive data.
- The rollback path is missing or wrong for a medium/high-risk PR.
- You cannot produce a conventional squash subject.
- You think regular merge might be appropriate but no maintainer exception has been recorded.

## AI / Agent contribution policy

AI-assisted PRs are welcome. Review can also be agent-assisted.

**Required:**

1. Clear PR summary with scope boundary.
2. Explicit test / validation evidence.
3. Security impact and rollback notes for risky changes.

**Recommended:**

1. Brief tool / workflow notes when automation materially influenced the change.
2. Optional prompt / plan snippets for reproducibility.

We do **not** require contributors to quantify AI-vs-human line ownership. The diff and the validation evidence carry the load.

For AI-heavy PRs, reviewers focus on:

- Contract compatibility.
- Security boundaries.
- Error handling and fallback behavior.
- Performance and memory regressions.
- Whether the author can answer questions about behavior and blast radius (intent comprehension).

## Review SLA and queue discipline

- First maintainer triage target: **within 48 hours**.
- Blocked PRs get one actionable checklist comment, not a series of partial reviews.
- `no-stale` reserved for accepted-but-blocked work.

For stacked work, require explicit `Depends on #...` so review order is deterministic.

For replacements, require explicit `Supersedes #...`. See [Superseding PRs](./superseding.md) for attribution and template rules.

The reviewer-side queue management — backlog pruning order, stale handling, label hygiene — is in [Reviewer Playbook](./reviewer-playbook.md).

## Security and stability rules

These paths require stricter review and stronger test evidence:

- `crates/zeroclaw-runtime/src/security/`
- The rest of `crates/zeroclaw-runtime/`
- `crates/zeroclaw-gateway/` (ingress, authentication, pairing)
- `crates/zeroclaw-tools/` (anything with execution capability)
- Filesystem access boundaries.
- Network and authentication behavior.
- `.github/workflows/` and the release pipeline.

**Minimum for risky PRs:** threat / risk statement, mitigation notes, rollback steps.

**Recommended for high-risk PRs:** a focused test proving boundary behavior, plus one explicit failure-mode scenario with expected degradation.

For agent-assisted contributions on these paths, reviewers also verify the author can talk through runtime behavior and blast radius — not just paste validation output.

## Failure recovery

If a merged PR causes regressions:

1. Revert on `master` immediately.
2. Open a follow-up issue with root-cause analysis.
3. Re-introduce the fix only with regression tests covering the failure mode.

Prefer fast restoration of service quality over a delayed perfect fix.

## What this page does NOT cover

- **Day-to-day review mechanics** — see [Reviewer Playbook](./reviewer-playbook.md) and [PR Review Protocol](../contributing/pr-review-protocol.md).
- **Label thresholds and definitions** — see [Labels](./labels.md).
- **Privacy and PII rules** — see [Privacy](../contributing/privacy.md).
- **Supersede attribution and templates** — see [Superseding PRs](./superseding.md).
- **CI workflow inventory and triage** — see [CI & Actions](./ci-and-actions.md).
- **Release procedure** — see [Release Runbook](./release-runbook.md).
