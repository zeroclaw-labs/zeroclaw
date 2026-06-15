# Skill: squash-merge

Squash-merge or queue a PR into `upstream/master` (zeroclaw-labs/zeroclaw) with project-conventional squash metadata. Use this skill when the user explicitly mentions squash-merging, queueing, merging a specific PR number, or landing a PR by number — e.g. "squash-merge #123", "queue PR 456", "merge PR 456", "land #789", "/squash-merge 123". Do **not** trigger on vague phrases like "ship it" or "merge it" without a PR number or clear upstream-merge context.

## Why This Exists

GitHub's default direct squash merge can omit the PR number from the commit subject or format the commit body inconsistently with project conventions. Direct-pushing a squash to master bypasses the PR merge mechanism entirely: the PR shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close, no merge commit association). This skill produces both: the purple **Merged** badge and a conventionally formatted squash commit.

When merge queue is used, GitHub controls the final squash commit from repository settings and the PR's title/commit messages. In that mode, do not use direct `--subject` / `--body` flags; verify the generated metadata and enqueue the PR so GitHub tests the exact merge group before landing.

## Prerequisites

Requires `gh` CLI ≥ 2.17.0 (for `--subject` and `--body` flags on `gh pr merge`). Verify with:

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
PR_JSON=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json id,number,title,headRefName,headRefOid,baseRefName,state,author,mergeable,mergeStateStatus,reviewDecision,isDraft)
```

Run pre-flight checks. **Stop at the first failure** and explain clearly:

| Check | Fail condition | What to tell the user |
|---|---|---|
| PR is open | `state != "OPEN"` | "PR #$NUMBER is already `<state>`, nothing to merge." |
| Targets master | `baseRefName != "master"` | "PR #$NUMBER targets `<base>`, not master. Confirm before proceeding." |
| No merge conflicts | `mergeable == "CONFLICTING"` | "PR #$NUMBER has merge conflicts with master. The author must resolve them before this can merge." |
| Not draft for queue mode | `isDraft == true` and queue mode is intended | "PR #$NUMBER is draft. Do not enqueue until it is ready for review." |

Then fetch the review decision:

```bash
REVIEW_DECISION=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json reviewDecision --jq '.reviewDecision // ""')
```

- `APPROVED` or `""` → proceed
- `REVIEW_REQUIRED` → warn the user that no required review has been received, and ask if they want to proceed anyway
- `CHANGES_REQUESTED` → stop: "PR #$NUMBER has a changes-requested review outstanding. The reviewer must approve or dismiss their review before this can merge."
- Queue mode is stricter than direct mode: do not enqueue unless `reviewDecision == "APPROVED"`, `isDraft == false`, the PR is clean/mergeable, and required branch checks are green.

### Step 2: Choose the Landing Mode

Use one of these modes and say which one you are using before asking for final confirmation:

- **Direct squash merge**: use when the user explicitly wants an immediate maintainer merge, merge queue is unavailable, or the PR cannot be queued. This mode prepares `gh pr merge --squash --subject ... --body ...`.
- **Merge queue**: use only for open, non-draft, approved, clean/mergeable PRs with green required branch checks. This mode verifies the PR title, generated squash body source, and repository squash settings, then enqueues the PR for exact merge-group CI. Do not pass `--subject` or `--body` in queue mode.

For merge queue mode, verify repository settings before continuing:

```bash
gh api repos/zeroclaw-labs/zeroclaw \
  --jq '{allow_squash_merge, squash_merge_commit_title, squash_merge_commit_message}'
```

`allow_squash_merge` must be `true`. The title/message settings must generate a conventional squash subject from the PR title and a safe body from the configured body source. If the settings are unsafe for the intended PR, stop and use direct squash merge or fix the PR/repository metadata before queueing.

Also verify required branch checks are green before queue mode:

```bash
gh pr checks "$NUMBER" --repo zeroclaw-labs/zeroclaw --required
```

### Step 3: Derive the Direct Squash Body or Queue Generated Metadata

Direct squash merge and merge queue use different body sources:

- Direct squash merge uses the `$COMMITS` body you pass to `gh pr merge --squash --body`.
- Merge queue uses GitHub's repository settings and cannot be overridden with `--subject` / `--body`.

For direct squash merge, get commit history:

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

For merge queue mode, derive and scan the exact generated body source based on `squash_merge_commit_message`:

- `COMMIT_MESSAGES`: fetch every commit's `messageHeadline` and `messageBody`, then scan both. Do not scan headlines only; trailers can live in commit bodies.
- `PR_BODY`: scan the current PR body.
- `BLANK`: verify the generated body is empty.

Example for the current ZeroClaw setting, `COMMIT_MESSAGES`:

```bash
QUEUE_BODY_SOURCE=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json commits \
  --jq '[.commits[] | "\(.messageHeadline)\n\(.messageBody // "")"] | join("\n\n")')
```

### Step 4: Validate the Squash Metadata

Before deriving the final direct merge command or queueing the PR, sanitize the body source that the chosen landing mode will actually use: `$COMMITS` for direct squash merge, or `$QUEUE_BODY_SOURCE` for merge queue. Strip bot/AI `Co-authored-by` trailers and generated tool footers, while preserving human co-author trailers only when they credit incorporated contributor work under the superseding and privacy rules. Set exactly one body source.

For direct squash merge:

```bash
BODY_TO_CHECK="$COMMITS"
```

For merge queue:

```bash
BODY_TO_CHECK="$QUEUE_BODY_SOURCE"
```

Then verify it before asking for confirmation:

```bash
printf '%s\n' "$BODY_TO_CHECK" | rg -i '(^[[:space:]]*(Co-authored-by|Co-Authored-By):.*(Claude|Codex|ChatGPT|Copilot|GitHub Copilot|Gemini|\[bot\]|dependabot|github-actions|web-flow|blacksmith|noreply@(anthropic|openai)\.com)|^[[:space:]]*(Created with Claude Code|Generated with Claude Code)[[:space:]]*$)'
```

If this prints anything, stop. For direct squash merge, strip the remaining bot attribution or generated footer before continuing. For merge queue, fix the PR title/body/commit metadata or use another approved landing path, because queue mode cannot override the final squash body.

```bash
PR_TITLE=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json title --jq '.title')
SUBJECT="${PR_TITLE} (#${NUMBER})"
```

The title should follow conventional commit format, e.g. `feat(scope): description` or `fix: short message`. If it does not, flag it to the user and suggest a corrected title. Do not proceed until the subject is in conventional commit format.

For merge queue mode, also confirm that GitHub's generated squash subject will match project convention. With repository title setting `PR_TITLE`, the expected subject is `${PR_TITLE} (#${NUMBER})`. If the repository settings or PR title would produce a non-conventional subject, stop and fix the PR title/settings or use direct squash merge.

### Step 5: Confirm — MANDATORY, NO EXCEPTIONS

**This step is non-negotiable.** A squash merge into `upstream/master`, whether direct or through merge queue, cannot be undone without a revert commit.

For direct squash merge, present the following to the user with `$NUMBER`, `$SUBJECT`, and `$COMMITS` substituted with their actual values — never show variable names or placeholder text:

---

**About to run:**
```
gh pr merge $NUMBER --repo zeroclaw-labs/zeroclaw --squash \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

**Effect:**
- PR #$NUMBER will be permanently merged (state → Merged, purple badge)
- Issues referenced with closing keywords will auto-close
- Squash commit subject: `$SUBJECT`
- Squash commit body:
  ```
  $COMMITS
  ```
- Bot/AI attribution has been stripped from the squash commit body.

Run this command? (yes/no)

---

For merge queue mode, first resolve the GraphQL PR id and head SHA, then present the exact queue command or GraphQL mutation plus the metadata that GitHub will use. The approval prompt must include the actual PR id, PR number, head SHA, generated squash subject, and generated squash body source. Do not show `$PULL_REQUEST_ID`, `$HEAD_SHA`, an abbreviated `PR_kw...`, `#1234`, or any other placeholder in the confirmation prompt. Include `expectedHeadOid` in the mutation so the approved head is the only head that can be queued.

Show Dan:

- PR number, title, and head SHA.
- Queue mutation with actual `pullRequestId` and `expectedHeadOid`.
- Squash subject from the PR title.
- Squash body source from the repository setting, already scanned for bot/AI attribution.
- Any linked issue or public tracker follow-through.

Ask: `Run this queue command? (yes/no)`

Do not infer consent from silence, prior approval of the commit message, or any earlier step. The user must respond with an unambiguous "yes" (or "y", "go", "do it") **in direct reply to this prompt**. Any other response — including silence, redirection, or "yes but first..." — means stop.

### Step 6: Execute

Only after explicit confirmation in Step 5.

For direct squash merge:

```bash
gh pr merge "$NUMBER" --repo zeroclaw-labs/zeroclaw --squash \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

For merge queue mode, first resolve the GraphQL PR id and head SHA:

```bash
PULL_REQUEST_ID=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json id --jq '.id')
HEAD_SHA=$(gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw --json headRefOid --jq '.headRefOid')
```

Then enqueue and capture the queue entry id:

```bash
MERGE_QUEUE_ENTRY_ID=$(gh api graphql \
  -f query='mutation($pullRequestId:ID!, $expectedHeadOid:GitObjectID!) { enqueuePullRequest(input:{pullRequestId:$pullRequestId, expectedHeadOid:$expectedHeadOid}) { mergeQueueEntry { id position state baseCommit { oid } headCommit { oid } pullRequest { number title url } } } }' \
  -f pullRequestId="$PULL_REQUEST_ID" \
  -f expectedHeadOid="$HEAD_SHA" \
  --jq '.data.enqueuePullRequest.mergeQueueEntry.id')
```

If the command exits non-zero, stop and report the full error output verbatim. Do not retry or attempt to work around failures.

### Step 7: Verify

For direct squash merge:

```bash
gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json state,mergedAt,mergeCommit \
  --jq '"State: \(.state) | Merged at: \(.mergedAt) | Commit: \(if .mergeCommit then .mergeCommit.oid[:7] else "N/A" end)"'
```

If `state` is not `MERGED`, report the discrepancy and stop — do not assume success.

Report to the user: merge commit SHA and PR URL.

For merge queue mode, verify the specific queue entry returned by the enqueue mutation. Do not rely on listing the first N queue entries when you have an entry id:

```bash
gh api graphql -f id="$MERGE_QUEUE_ENTRY_ID" \
  -f query='query($id:ID!) { node(id:$id) { ... on MergeQueueEntry { id position state pullRequest { number title url } baseCommit { oid } headCommit { oid } } } }'
```

Report the PR number, queue position, queue state, base commit, and merge-group head commit. Inspect the merge-group checks for the queue `headCommit.oid` before treating the queue validation as green. Do not report the PR as merged until a later `gh pr view` confirms `state == "MERGED"`.

**Never delete contributor branches.** Do not suggest, offer, or run any branch deletion command — not on the upstream remote, not on forks. Branch cleanup is the contributor's responsibility and is always a human decision.

## Rules

- **Require a PR number or explicit squash-merge context before triggering** — do not invoke on vague phrases without a clear target.
- **Never push squash commits directly to `upstream/master`** — always use `gh pr merge`. Direct push produces "Closed" not "Merged", breaks issue auto-close, and loses PR association.
- **For direct merges, never use `gh pr merge --squash` without `--subject` and `--body`** — the auto-generated direct-merge message may omit the PR number or use inconsistent formatting.
- **For direct merges, never let GitHub auto-generate the squash message** — no web UI merge, no merge button clicks for direct landing.
- **For merge queue, verify generated metadata before enqueueing** — repository squash settings, PR title, and commit messages control the final squash commit.
- **Always strip or reject bot/AI attribution from the squash body** before confirmation. Preserve intentional human co-author trailers only under the superseding and privacy rules.
- **Always assign PR title and commit body to shell variables** — never interpolate untrusted content directly into quoted command arguments.
- **Always run pre-flight checks** (merge conflicts, review decision) before confirming — do not skip them even if the user says "just merge it."
- **Always confirm before merging, no exceptions** — show the user the exact expanded command with real values and require an explicit yes. Never infer consent.
- **If the merge command fails, stop and report verbatim** — do not retry or work around failures automatically.
- **Never delete branches** — not on upstream, not on forks. Branch cleanup is always the contributor's decision. Never suggest a deletion command.
- **Self-merge note:** Maintainers routinely merge their own PRs. If the user is the PR author, proceed normally — just note it in the confirmation summary so it's visible in the audit trail.
