# Skill: squash-merge

Squash-merge a PR into `upstream/master` (zeroclaw-labs/zeroclaw) with fully preserved commit history in the squash message body. Use this skill when the user explicitly mentions squash-merging, merging a specific PR number, or landing a PR by number — e.g. "squash-merge #123", "merge PR 456", "land #789", "/squash-merge 123". Do **not** trigger on vague phrases like "ship it" or "merge it" without a PR number or clear upstream-merge context.

## Why This Exists

GitHub's default squash merge omits the PR number from the commit subject and formats the commit body inconsistently with project conventions. Direct-pushing a squash to master bypasses the PR merge mechanism entirely: the PR shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close, no merge commit association). This skill produces both: the purple **Merged** badge and a conventionally formatted squash commit with full commit history in the body.

## Prerequisites

Requires `gh` CLI ≥ 2.17.0 (for `--subject` and `--body` flags on `gh pr merge`). Verify with:

```bash
gh --version
```

If the version is older, stop and tell the user to upgrade: `gh upgrade` or install from https://cli.github.com.

## Instructions

### Step 1: Resolve the PR and Run Pre-flight Checks

Accept a PR number or URL from the user. If none given, detect from current branch:

```bash
gh pr view --repo zeroclaw-labs/zeroclaw \
  --json number,title,headRefName,baseRefName,state,author,mergeable,mergeStateStatus,reviewDecision
```

Run all four pre-flight checks. **Stop at the first failure** and explain clearly:

| Check | Command | Fail condition | What to tell the user |
|---|---|---|---|
| PR is open | `.state` from above | `state != "OPEN"` | "PR #N is already `<state>`, nothing to merge." |
| Targets master | `.baseRefName` from above | `baseRefName != "master"` | "PR #N targets `<base>`, not master. Confirm before proceeding." |
| No merge conflicts | `.mergeable` from above | `mergeable == "CONFLICTING"` | "PR #N has merge conflicts with master. The author must resolve them before this can merge." |
| CI passing | `gh pr checks <N> --repo zeroclaw-labs/zeroclaw` | Any required check failing | "PR #N has failing required checks: `<names>`. Do not merge until CI passes." |

After the four checks, fetch the review decision:

```bash
gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json reviewDecision \
  --jq '.reviewDecision'
```

- `APPROVED` → proceed
- `REVIEW_REQUIRED` → warn: "PR #N has not yet received a required review. Proceed only if you are waiving this requirement as a maintainer."
- `CHANGES_REQUESTED` → stop: "PR #N has a `CHANGES_REQUESTED` review outstanding. Do not merge until resolved."
- `""` (empty / no rule) → proceed

### Step 2: Get Commit History

Use `gh` to fetch commits in API order (typically chronological):

```bash
COMMITS=$(gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw \
  --json commits \
  --jq '[.commits[] | "- \(.oid[:7]) \(.messageHeadline)"] | join("\n")')
```

If `gh` returns no commit data or hashes are missing, fall back to local git using the PR's actual base ref:

```bash
BASE_REF=$(gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json baseRefName --jq '.baseRefName')
HEAD_REF=$(gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json headRefName --jq '.headRefName')
COMMITS=$(git log "upstream/${BASE_REF}..${HEAD_REF}" --format="- %h %s")
```

**Single-commit PRs:** If the result is exactly one line, omit the bulleted body entirely. Instead, use the full commit body (if any) from `git log -1 --format="%b" <sha>` or leave the body empty. A one-item bullet list adds no information.

Note: commits are returned in API order, which is typically chronological but not guaranteed for rebased histories. If ordering looks wrong, use the `git log` fallback which follows the actual graph order.

### Step 3: Derive the Squash Commit Subject

```bash
PR_TITLE=$(gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json title --jq '.title')
SUBJECT="${PR_TITLE} (#${NUMBER})"
```

The title should follow conventional commit format (`type(scope): description`). If it does not, flag it to the user and suggest a corrected title. Do not proceed until the subject is in conventional commit format.

### Step 4: Build the Final Command Using Variables

Assign subject and body to shell variables — never interpolate them directly into quoted strings, as PR titles and commit messages may contain double-quotes, dollar signs, backticks, or other shell-special characters:

```bash
# Subject already in SUBJECT; commits already in COMMITS (from Steps 2–3)
# Verify they are non-empty before proceeding
echo "Subject: $SUBJECT"
echo "Body lines:"
echo "$COMMITS"
```

The final `gh` command will use these variables:

```bash
gh pr merge <NUMBER> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

### Step 5: Confirm — MANDATORY, NO EXCEPTIONS

**This step is non-negotiable.** A squash merge into `upstream/master` cannot be undone without a revert commit.

Present the following to the user — all values must be real and fully expanded, not placeholders:

---

**About to run:**
```
gh pr merge <NUMBER> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "<actual subject line here>" \
  --body "<actual body lines here, one per line>"
```

**Effect:**
- PR #`<NUMBER>` will be permanently merged (state → Merged, purple badge)
- Linked issues will auto-close
- Squash commit subject: `<actual subject>`
- Squash commit body:
  ```
  <actual body>
  ```

**Run this command? (yes/no)**

---

Do not infer consent from silence, prior approval of the commit message, or any earlier step. The user must respond with an unambiguous "yes" (or "y", "go", "do it") **in direct reply to this prompt**. Any other response — including silence, redirection, or "yes but first..." — means stop.

### Step 6: Execute

Only after explicit confirmation in Step 5:

```bash
gh pr merge <NUMBER> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "$SUBJECT" \
  --body "$COMMITS"
```

If the command exits non-zero, stop immediately and report the full error output. Do not attempt to retry or work around branch protection errors automatically — report them verbatim and let the user decide.

### Step 7: Verify and Clean Up

Confirm the merge succeeded:

```bash
gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw \
  --json state,mergedAt,mergeCommit \
  --jq '"State: \(.state) | Merged at: \(.mergedAt) | Commit: \(.mergeCommit.oid[:7])"'
```

If `state` is not `MERGED`, report the discrepancy and stop — do not assume success.

Report to the user: merge commit SHA, PR URL, and the following cleanup note:

> The contributor's branch `<headRefName>` still exists on the remote. Delete it with:
> ```bash
> gh api -X DELETE repos/zeroclaw-labs/zeroclaw/git/refs/heads/<headRefName>
> ```
> Skip if GitHub's "auto-delete head branches" setting is enabled for the repo.

## Rules

- **Require a PR number or explicit squash-merge context before triggering** — do not invoke on vague phrases without a clear target.
- **Never push squash commits directly to `upstream/master`** — always use `gh pr merge`. Direct push produces "Closed" not "Merged", breaks issue auto-close, and loses PR association.
- **Never use `gh pr merge --squash` without `--subject` and `--body`** — the auto-generated message omits the PR number and uses inconsistent formatting.
- **Never let GitHub auto-generate the squash message** — no web UI merge, no merge button clicks.
- **Always assign PR title and commit body to shell variables** — never interpolate untrusted content directly into quoted command arguments.
- **Always run pre-flight checks** (CI, merge conflicts, review decision) before confirming — do not skip them even if the user says "just merge it."
- **Always confirm before merging, no exceptions** — show the user the exact expanded command with real values and require an explicit yes. Never infer consent.
- **If the merge command fails, stop and report verbatim** — do not retry, do not work around branch protection automatically.
- **Note branch cleanup** after every successful merge — the contributor's remote head branch does not auto-delete unless the repo is configured to do so.
- **Self-merge note:** Maintainers routinely merge their own PRs. If the user is the PR author, proceed normally — just note it in the confirmation summary so it's visible in the audit trail.
