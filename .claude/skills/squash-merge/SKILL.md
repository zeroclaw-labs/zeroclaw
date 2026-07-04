# Skill: squash-merge

Squash-merge a PR into `zeroclaw-labs/zeroclaw` `master` with fully preserved commit history in the squash message body. Use this skill when the user explicitly mentions squash-merging, merging a specific PR number, landing a PR, or 合入 — e.g. "squash-merge #123", "merge PR 456", "land #789", "合入 #123", "/squash-merge 123". Do **not** trigger on vague phrases like "ship it" or "merge it" without a PR number or clear upstream-merge context.

## Related Skills

| Step | Skill | When |
|---|---|---|
| Pick / triage issues | `github-issue-triage` | Backlog sweep, label issues, close duplicates |
| File a bug / feature | `github-issue` | No existing issue for the work |
| Open / update PR | `github-pr` | Branch is ready; needs template body and validation evidence |
| Review before merge | `github-pr-review-session` | Maintainer reviewing someone else's PR |
| **Land into master** | **this skill** | PR is approved and CI is green |

## End-to-End Contributor Workflow (issue → merge)

When the user asks to fix an issue and get it merged, follow this sequence:

1. **Read the issue** — `gh issue view <N>`; confirm it is still open and not already fixed on `master`.
2. **Branch** — `git checkout -b fix/<short-description>` from up-to-date `master`.
3. **Implement** — minimal diff; reference canonical state (see `AGENTS.md` no-duplicate-state rule).
4. **Validate** — run `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (or docs gate if docs-only).
5. **Open PR** — use the `github-pr` skill; body must include `Closes #<N>` when the PR fully resolves the issue.
6. **Wait for CI** — before merge, confirm required checks pass (see Pre-merge CI check below).
7. **Squash-merge** — use this skill with explicit user confirmation.

Do not skip straight to merge if no PR exists yet.

## Why This Exists

GitHub's default squash merge omits the PR number from the commit subject and formats the commit body inconsistently with project conventions. Direct-pushing a squash to master bypasses the PR merge mechanism entirely: the PR shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close, no merge commit association). This skill produces both: the purple **Merged** badge and a conventionally formatted squash commit with full commit history in the body.

## Prerequisites

Requires `gh` CLI ≥ 2.50.0 (for `--json name,state,bucket` on `gh pr checks`). Verify with:

```bash
gh --version
```

If the version is older, stop and tell the user to upgrade: `gh upgrade` or install from [cli.github.com](https://cli.github.com).

## Instructions

### Step 1: Resolve the PR and Run Pre-flight Checks

Accept a PR number or URL from the user. If none is given, attempt auto-detection from the current branch — but if that fails (e.g. not on a PR branch), stop and ask the user to provide the PR number explicitly.

Capture the PR number into `$NUMBER` for all subsequent steps:

```bash
NUMBER=$(gh pr view <PR_NUMBER_OR_URL> --repo zeroclaw-labs/zeroclaw --json number --jq '.number')
```

Then fetch PR metadata:

```bash
gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json number,title,headRefName,baseRefName,headRefOid,state,author,mergeable,mergeStateStatus,reviewDecision
```

Save `headRefOid` as `$HEAD_SHA` for the confirmation and merge command.

Run pre-flight checks. **Stop at the first stop condition** and explain clearly:

| Check | Condition | Action |
|---|---|---|
| PR is open | `state != "OPEN"` | Stop: "PR #$NUMBER is already `<state>`, nothing to merge." |
| Targets master | `baseRefName != "master"` | Stop unless explicitly confirmed: "PR #$NUMBER targets `<base>`, not master. Confirm before proceeding." |
| No merge conflicts | `mergeable == "CONFLICTING"` or `mergeStateStatus == "DIRTY"` | Stop: "PR #$NUMBER has merge conflicts or a dirty merge state with master. The author must refresh or resolve conflicts before this can merge." |
| Merge state known | `mergeStateStatus == "UNKNOWN"` | Refresh/retry once; if still unknown, stop and report that GitHub has not computed mergeability yet. |
| Not blocked or draft | `mergeStateStatus` is `BLOCKED` / `DRAFT` | Stop and report the blocking gate or draft state. |
| Behind or unstable | `mergeStateStatus` is `BEHIND` / `UNSTABLE` | Continue to Step 1c, but do not use old green branch checks alone as the freshness basis. |

Then fetch the review decision:

```bash
REVIEW_DECISION=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json reviewDecision --jq '.reviewDecision // ""')
```

- `APPROVED` or `""` → proceed
- `REVIEW_REQUIRED` → warn the user that no required review has been received, and ask if they want to proceed anyway
- `CHANGES_REQUESTED` → stop: "PR #$NUMBER has a changes-requested review outstanding. The reviewer must approve or dismiss their review before this can merge."

### Step 1b: Pre-merge CI Check

Before asking the user to confirm the merge, verify CI status:

```bash
gh pr checks "$NUMBER" --repo zeroclaw-labs/zeroclaw --required
```

Also fetch required checks for a machine-readable summary:

```bash
gh pr checks "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --required \
  --json name,state,bucket
```

| Bucket value | Action |
|---|---|
| `pass` for every required check, including the repo's required aggregate gate (currently `CI Required Gate`) | Proceed to Step 1c |
| `fail` or `cancel` for any required check | Stop — report failing or cancelled check names; do not merge |
| `pending` for any required check | Stop — tell user to wait for CI; offer to retry later |
| `skipping` for any required check | Stop — report the skipped required check names and ask whether the skip is expected before proceeding |
| No required checks are configured or returned | Warn and ask user whether to proceed |

Do not merge on red CI unless the user explicitly overrides after seeing the failure list.

### Step 1c: Establish the Freshness Basis

Before deriving or confirming the merge command, record why the merge is current
enough to run. Do not merge merely because the PR branch had green checks at
some earlier point. This is a merge-readiness gate, not a code-review blocker:
being behind `master` can require fresh merge evidence before merging without
making the reviewed implementation itself wrong.

Set `$FRESHNESS_BASIS` to one concrete sentence that names the selected basis
and evidence. Do not leave it as an option list.

Use one of these freshness bases:

1. **Current official checks** — GitHub reports the PR cleanly mergeable against
   current `master` (`mergeStateStatus` is `CLEAN` or `HAS_HOOKS`), and all
   required checks on the current `$HEAD_SHA` are successful.
2. **Exact queued/merge-result checks** — a merge queue, merge group, or
   equivalent exact-result CI path has validated the result that will land.
3. **Exact merge-result smoke** — you locally construct or inspect the exact
   merge result that will be created, then run an appropriate compile/test smoke
   for the touched surface. Use this only after the user approves the validation
   scope, and see the `BEHIND`/`UNSTABLE` constraint below before choosing it
   over official CI.
4. **Explicit stale-risk acceptance** — if checks or merge-result validation are
   stale or unavailable, tell the user exactly what is stale or unverified and
   get explicit approval to accept that risk for this PR.

When the PR is `BEHIND` or `UNSTABLE`, do not treat old green branch checks as
merge readiness. If current `master` changed the same files or high-risk shared
surfaces such as build/CI/config, generated artifacts, public interfaces,
security or authorization boundaries, provider/channel/runtime paths, or
required test harnesses, prefer updating the branch and waiting for official CI.
If updating is unavailable or intentionally skipped, use exact queued or
merge-result validation, or get explicit stale-risk acceptance that names the
unverified overlap. Do not run local merge-result smoke as a default substitute
for official CI.

Carry the selected freshness basis into the confirmation prompt in Step 4.

### Step 2: Get Commit History

```bash
COMMITS=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json commits \
  --jq '[.commits[] | "- \(.oid[:7]) \(.messageHeadline)"] | join("\n")')
```

If `gh` returns no commit data or hashes are missing, fall back to local git. This requires the contributor's branch to be locally available — fetch first:

```bash
BASE_REF=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json baseRefName --jq '.baseRefName')
HEAD_REF=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json headRefName --jq '.headRefName')

git fetch upstream
git fetch origin

COMMITS=$(git log "upstream/${BASE_REF}..origin/${HEAD_REF}" --format="- %h %s")
```

If `origin/${HEAD_REF}` doesn't exist (contributor's branch is on their own fork), the fallback cannot be used — stick with the `gh` API output.

**Single-commit PRs:** If `$COMMITS` is exactly one line, use the full commit body instead of the bullet list. Get it with:

```bash
SHA=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json commits --jq '.commits[-1].oid')
COMMITS=$(git log -1 --format="%b" "$SHA")
```

Leave `$COMMITS` empty if there is no commit body. A one-item bullet list adds no information.

Note: commits from the API are in API order, which is typically chronological but not guaranteed for rebased histories. Use the `git log` fallback if ordering looks wrong.

### Step 3: Derive the Squash Commit Subject

Before deriving the final merge command, sanitize `$COMMITS`: strip bot/AI
`Co-authored-by` trailers and generated tool footers, while preserving human
co-author trailers only when they credit incorporated contributor work under the
superseding and privacy rules. Then verify the body before asking for merge
confirmation:

```bash
printf '%s\n' "$COMMITS" | rg -i '(^[[:space:]]*(Co-authored-by|Co-Authored-By):.*(Claude|Codex|ChatGPT|Copilot|GitHub Copilot|Gemini|\[bot\]|dependabot|github-actions|web-flow|blacksmith|noreply@(anthropic|openai)\.com)|^[[:space:]]*(Created with Claude Code|Generated with Claude Code)[[:space:]]*$)'
```

If this prints anything, stop and strip the remaining bot attribution or
generated footer before continuing.

```bash
PR_TITLE=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json title --jq '.title')
SUBJECT="${PR_TITLE} (#${NUMBER})"
```

The title should follow conventional commit format, e.g. `feat(scope): description` or `fix: short message`. If it does not, flag it to the user and suggest a corrected title. Do not proceed until the subject is in conventional commit format.

### Step 4: Confirm — MANDATORY, NO EXCEPTIONS

**This step is non-negotiable.** A squash merge into `upstream/master` cannot be undone without a revert commit.

Present the following to the user with `$NUMBER`, `$HEAD_SHA`, `$SUBJECT`,
`$COMMITS`, and `$FRESHNESS_BASIS` substituted with their actual values — never
show variable names or placeholder text:

---

**About to run:**
```
gh pr merge $NUMBER --repo zeroclaw-labs/zeroclaw --squash \
  --match-head-commit "$HEAD_SHA" \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

**Effect:**
- PR #$NUMBER will be permanently merged (state → Merged, purple badge)
- Issues referenced with closing keywords will auto-close
- PR head SHA: `$HEAD_SHA`
- Freshness basis: `$FRESHNESS_BASIS`
- Squash commit subject: `$SUBJECT`
- Squash commit body:
  ```
  $COMMITS
  ```
- Bot/AI attribution has been stripped from the squash commit body.

Run this command? (yes/no)

---

Do not infer consent from silence, prior approval of the commit message, or any earlier step. The user must respond with an unambiguous "yes" (or "y", "go", "do it") **in direct reply to this prompt**. Any other response — including silence, redirection, or "yes but first..." — means stop.

### Step 5: Execute

Only after explicit confirmation in Step 4:

```bash
gh pr merge "$NUMBER" --repo zeroclaw-labs/zeroclaw --squash \
  --match-head-commit "$HEAD_SHA" \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

If the command exits non-zero, stop and report the full error output verbatim. Do not retry or attempt to work around failures.

### Step 6: Verify

```bash
gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json state,mergedAt,mergeCommit \
  --jq '"State: \(.state) | Merged at: \(.mergedAt) | Commit: \(if .mergeCommit then .mergeCommit.oid[:7] else "N/A" end)"'
```

If `state` is not `MERGED`, report the discrepancy and stop — do not assume success.

Report to the user: merge commit SHA and PR URL.

**Post-merge (optional, only if user asks):**
- Fetch latest master: `git checkout master && git pull upstream master` (or `origin master` if no upstream remote)
- Verify linked issue closed: `gh issue view <N> --json state --jq .state` (should be `CLOSED` when PR body used `Closes #N`)

**Never delete contributor branches.** Do not suggest, offer, or run any branch deletion command — not on the upstream remote, not on forks. Branch cleanup is the contributor's responsibility and is always a human decision.

### Step 7: Public Tracker Follow-Through

After a verified merge, do a final-status pass for public tracker follow-through
only when the PR is already tied to a public milestone, release, recovery, RFC,
or umbrella tracker, or when the user asked for tracker cleanup. Use linked
issues, milestone assignment, PR body references, and existing tracking issue
entries as the source of truth.

If a public tracker needs an update:

1. Read the current tracker body first.
2. Match the existing section and row format; do not invent a new tracker
   structure during merge cleanup.
3. Prepare the exact tracker or issue-body diff.
4. Get user approval before editing public issue state unless the approval for
   the merge explicitly included this specific tracker update.
5. Verify the public tracker after editing.

If prior milestone/tracker alignment already made the tracker current, report
`already current`. If there is no known tracker relationship, report that no
known public tracker follow-up applies. Do not move to the next merge while
leaving a known public tracker stale.

## Rules

- **Require a PR number or explicit squash-merge context before triggering** — do not invoke on vague phrases without a clear target.
- **Never push squash commits directly to `upstream/master`** — always use `gh pr merge`. Direct push produces "Closed" not "Merged", breaks issue auto-close, and loses PR association.
- **Never use `gh pr merge --squash` without `--subject` and `--body`** — the auto-generated message omits the PR number and uses inconsistent formatting.
- **Never let GitHub auto-generate the squash message** — no web UI merge, no merge button clicks.
- **Always strip bot/AI attribution from the squash body** before confirmation.
  Preserve intentional human co-author trailers only under the superseding and
  privacy rules.
- **Always assign PR title and commit body to shell variables** — never interpolate untrusted content directly into quoted command arguments.
- **Always run pre-flight checks** (merge conflicts, review decision, CI status) before confirming — do not skip them even if the user says "just merge it."
- **Always record a freshness basis before confirming** — refreshed official checks, exact queued/merge-result checks, exact merge-result smoke, or explicit stale-risk acceptance. Do not treat old green branch checks as merge readiness when current `master` could invalidate them.
- **Always confirm before merging, no exceptions** — show the user the exact expanded command with real values and require an explicit yes. Never infer consent.
- **If the merge command fails, stop and report verbatim** — do not retry or work around failures automatically.
- **Always handle public tracker follow-through after a verified merge** — update relevant public trackers with approval, or report that none apply.
- **Never delete branches** — not on upstream, not on forks. Branch cleanup is always the contributor's decision. Never suggest a deletion command.
- **Self-merge note:** Maintainers routinely merge their own PRs. If the user is the PR author, proceed normally — just note it in the confirmation summary so it's visible in the audit trail.
