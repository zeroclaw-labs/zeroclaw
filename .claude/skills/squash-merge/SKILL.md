# Skill: squash-merge

Squash-merge a PR into `upstream/master` (zeroclaw-labs/zeroclaw) with fully preserved commit history in the squash message body. Use this skill whenever the user wants to merge a PR, land a PR, squash-merge, or close a PR as merged — even if they say things like "land this", "merge it in", "squash and merge", or "ship it".

## Why This Exists

GitHub's default squash-commit message is useless ("Squashed commit of the following: ..."). Direct-pushing a squash to master bypasses the PR merge mechanism and shows "Closed" instead of "Merged" (no purple badge, no linked issue auto-close). This skill gets both: the purple **Merged** badge and a meaningful commit history in the squash message body.

## Instructions

### Step 1: Resolve the PR

Accept a PR number or URL from the user. If none given, detect from current branch:

```bash
gh pr view --repo zeroclaw-labs/zeroclaw --json number,title,headRefName,baseRefName,state,author
```

Confirm the PR is open and targets `master`. If closed or merged already, stop and inform the user.

### Step 2: Get Commit History

Fetch the PR's commits and format them as a bulleted history block:

```bash
gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json commits \
  --jq '.commits[] | "- \(.oid[:7]) \(.messageHeadline)"'
```

If the PR branch is checked out locally and `gh` commit output is missing full hashes, fall back to:

```bash
git log master..<branch> --format="- %h %s"
```

Collect the output — this becomes the body of the squash commit.

### Step 3: Derive the Squash Commit Subject

Build the subject line as a conventional commit following the PR title pattern:

```
<PR title> (#<NUMBER>)
```

The PR title should already follow the conventional commit format (`type(scope): description`). If it does not, flag it to the user and suggest a corrected title before proceeding.

### Step 4: Confirm — MANDATORY, NO EXCEPTIONS

**This step is non-negotiable.** A squash merge into `upstream/master` cannot be undone without a revert commit. Always stop here and present the following to the user before running anything:

1. The exact `gh` command that will be executed, with all arguments expanded (no placeholders):

```
gh pr merge <NUMBER> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "<full subject line>" \
  --body "<full body text>"
```

2. A summary of what will happen:
   - PR #N will be merged (state: Merged, purple badge)
   - Squash commit subject: `<subject>`
   - Squash commit body: `<body lines>`

Ask explicitly: **"Run this command? (yes/no)"**

Do not infer consent from silence, prior approval of the message draft, or anything else. The user must say yes (or an unambiguous equivalent) in direct response to this prompt. If they say anything other than a clear yes, stop.

### Step 5: Execute the Squash Merge

Only after explicit user confirmation in Step 4:

```bash
gh pr merge <NUMBER> --repo zeroclaw-labs/zeroclaw --squash \
  --subject "<PR title> (#<NUMBER>)" \
  --body "$(cat <<'MERGE_BODY_EOF'
<commit history lines>
MERGE_BODY_EOF
)"
```

### Step 6: Confirm

After the command returns, confirm the merge succeeded:

```bash
gh pr view <NUMBER> --repo zeroclaw-labs/zeroclaw --json state,mergedAt,mergeCommit \
  --jq '"State: \(.state) | Merged at: \(.mergedAt) | Commit: \(.mergeCommit.oid[:7])"'
```

Report the merge commit SHA and PR URL to the user.

## Rules

- **Never push squash commits directly to `upstream/master`** — always use `gh pr merge`. Direct push bypasses the PR mechanism: the PR shows "Closed" instead of "Merged", linked issues don't auto-close, and the merge commit is not associated with the PR.
- **Never use `gh pr merge --squash` without `--subject` and `--body`** — the auto-generated message loses all commit context.
- **Never let GitHub auto-generate the squash message** (no web UI merge, no merge button clicks).
- **Always preserve commit hashes** in the body — short 7-char hashes are sufficient.
- **Always confirm before merging, no exceptions** — show the user the exact expanded `gh pr merge` command and require an explicit yes before running it. Never infer consent.
- **Never merge your own PRs** — if the user is the PR author and is asking to merge, confirm they have maintainer authority to self-merge before proceeding.
