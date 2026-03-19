# Skill: github-pr

Open or update a GitHub Pull Request for JhedaiClaw. Handles creating new PRs with a fully filled-out template body, and updating existing PRs (title, body sections, labels, comments). Use this skill whenever the user wants to open a PR, create a pull request, update a PR, edit PR description, add labels to a PR, or sync a PR after new commits — even if they don't say "PR" explicitly (e.g., "submit this for review", "push and open for merge").

## Instructions

This skill supports two modes: **Open** (create a new PR) and **Update** (edit an existing PR). Detect the mode from context — if there's already an open PR for the current branch and the user didn't say "open a new PR", default to update mode.

The PR template at `.github/pull_request_template.md` is the source of truth for the PR body structure. Read it every time — never assume or hardcode section names, fields, or their order. The template may change over time and the skill should always reflect its current state.

---

## Shared: Read the PR Template

Before opening or updating a PR body, read `.github/pull_request_template.md` and parse it to understand:

- The `## ` section headers (these are the top-level sections of the PR body)
- The bullet points, fields, and prompts within each section
- Which sections are marked `(required)` vs optional/recommended
- Any inline formatting conventions (backtick options, Yes/No fields, etc.)

This parsed structure drives how you fill, present, and edit the PR body.

---

## Mode: Open a New PR

### Step 1: Gather Context

Collect information to pre-fill the PR body. Run these in parallel:

```bash
# Branch and commit context
git branch --show-current
git log master..HEAD --oneline
git diff master...HEAD --stat

# Check if branch is pushed
git rev-parse --abbrev-ref --symbolic-full-name @{u} 2>/dev/null

# Environment (for validation evidence)
rustc --version 2>/dev/null
```

Also review the changed files and commit messages to understand the nature of the change (bug fix, feature, refactor, docs, chore, etc.) and which subsystems are affected.

### Step 2: Pre-Fill the Template

Using the parsed template structure and gathered context, draft a complete PR body:

- For each `## ` section from the template, fill in the bullet points and fields based on context from the commits, diff, and changed files.
- Use the field descriptions and placeholder text in the template as guidance for what each field expects.
- For Yes/No fields, infer from the diff (e.g., if no files in `src/security/` changed, security impact is likely all No).
- For required sections, always provide a substantive answer. For optional sections, fill if there's enough context, otherwise leave the template prompts in place.
- Draft a conventional commit-style PR title based on the changes (e.g., `feat(provider): add retry budget override`, `fix(channel): handle disconnect gracefully`, `chore(ci): update workflow targets`).

### Step 3: Present Draft for Review

Show the user the complete draft:

```
## PR Draft: <title>
**Branch**: <head> -> master
**Labels**: <suggested labels>

<full body with all sections filled>
```

Ask the user to review: "Here's the pre-filled PR. Review and let me know what to change, or say 'submit' to open it."

Iterate on changes until the user approves.

### Step 4: Push and Create

1. If the branch isn't pushed yet, push it:

   ```bash
   git push -u origin <branch>
   ```

2. Create the PR using a HEREDOC for the body:

   ```bash
   gh pr create --title "<title>" --base master --body "$(cat <<'PR_BODY_EOF'
   <full body>
   PR_BODY_EOF
   )"
   ```

3. If labels were agreed on, add them:

   ```bash
   gh pr edit <number> --add-label "<label1>,<label2>"
   ```

4. Return the PR URL to the user.

---

## Mode: Update an Existing PR

### Step 1: Identify the PR

1. **If a PR number or URL is given**: use that directly.
2. **If on a branch with an open PR**: auto-detect:
   ```bash
   gh pr view --json number,title,body,labels,state,author,url,headRefName 2>/dev/null
   ```
3. **If neither**: ask the user for the PR number.

Verify the current user is the PR author:

```bash
CURRENT_USER=$(gh api user --jq '.login')
PR_AUTHOR=$(gh pr view <number> --json author --jq '.author.login')
```

If not the author, stop and inform the user.

### Step 2: Fetch Current State

```bash
gh pr view <number> --json number,title,body,labels,state,baseRefName,headRefName,url,author,reviewDecision,statusCheckRollup,commits
```

Display a summary:

```
## PR #<number>: <title>
**State**: <open/closed/merged>
**Branch**: <head> -> <base>
**Labels**: <label list>
**Checks**: <pass/fail/pending>
**URL**: <url>
```

### Step 3: Determine What to Update

Support these operations:

| Operation                          | How                                                                     |
| ---------------------------------- | ----------------------------------------------------------------------- |
| **Edit title**                     | `gh pr edit <number> --title "<new title>"`                             |
| **Edit full body**                 | `gh pr edit <number> --body "<new body>"`                               |
| **Add labels**                     | `gh pr edit <number> --add-label "<label1>,<label2>"`                   |
| **Remove labels**                  | `gh pr edit <number> --remove-label "<label1>"`                         |
| **Edit specific section**          | Parse body by `## ` headers, modify target section, re-submit full body |
| **Add a comment**                  | `gh pr comment <number> --body "<comment>"`                             |
| **Link an issue**                  | Edit the linked-issue section in the body                               |
| **Smart update after new commits** | Re-analyze and suggest section updates                                  |

### Step 4: Handle Body Section Edits

When editing a specific section:

1. Parse the current PR body into sections by `## ` headers
2. Match the user's request to the corresponding section from the template
3. Show the current content of that section and the proposed replacement
4. On confirmation, modify only that section, reconstruct the full body, and submit

### Step 5: Smart Update After New Commits

When the user wants to sync the PR description after pushing new changes:

1. Identify new commits:

   ```bash
   gh pr view <number> --json commits --jq '.commits[].messageHeadline'
   git log <base>..<head> --oneline
   git diff <base>...<head> --stat
   ```

2. Re-read the PR template. Analyze which sections are now stale based on the new changes — use the template's section names and field descriptions to identify what needs updating rather than relying on hardcoded assumptions.

3. Present proposed updates section-by-section and confirm before applying.

### Step 6: Apply Updates

For title/label changes, use direct `gh pr edit` flags.

For body edits, use a HEREDOC:

```bash
gh pr edit <number> --body "$(cat <<'PR_BODY_EOF'
<full updated body>
PR_BODY_EOF
)"
```

For comments:

```bash
gh pr comment <number> --body "$(cat <<'COMMENT_EOF'
<comment text>
COMMENT_EOF
)"
```

### Step 7: Confirm

Fetch and display the updated state:

```bash
gh pr view <number> --json number,title,labels,url
```

Return the PR URL.

---

## Important Rules

- **Always read `.github/pull_request_template.md`** before filling or editing a PR body. Never assume section names, fields, or structure — derive everything from the template. It's the source of truth and may change.
- **For updates, only modify requested sections.** Preserve everything else exactly as-is.
- **Always show diffs before applying body edits.** Present current vs proposed for each changed section.
- **Never include personal/sensitive data** in PR content per JhedaiClaw's privacy contract.
- **For label changes**, only use labels that exist in the repository. Check with `gh label list` if unsure.
- **Fetch the latest body before editing** to avoid clobbering concurrent changes.
- **For new PRs**, push the branch before creating (with `-u` to set upstream tracking).
