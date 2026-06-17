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
gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json number,title,headRefName,baseRefName,state,author,mergeable,reviewDecision
```

Run pre-flight checks. **Stop at the first failure** and explain clearly:

| Check | Fail condition | What to tell the user |
|---|---|---|
| PR is open | `state != "OPEN"` | "PR #$NUMBER is already `<state>`, nothing to merge." |
| Targets master | `baseRefName != "master"` | "PR #$NUMBER targets `<base>`, not master. Confirm before proceeding." |
| No merge conflicts | `mergeable == "CONFLICTING"` | "PR #$NUMBER has merge conflicts with master. The author must resolve them before this can merge." |

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
gh pr checks "$NUMBER" --repo zeroclaw-labs/zeroclaw
```

Also fetch rollup for a machine-readable summary:

```bash
gh pr view "$NUMBER" --repo zeroclaw-labs/zeroclaw \
  --json statusCheckRollup \
  --jq '[.statusCheckRollup[]? | {name: .name, state: .state, conclusion: .conclusion}]'
```

| State | Action |
|---|---|
| All required checks `SUCCESS` | Proceed to Step 2 |
| Any required check `FAILURE` / `CANCELLED` | Stop — report failing check names; do not merge |
| Checks `PENDING` / `IN_PROGRESS` | Stop — tell user to wait for CI; offer to retry later |
| No checks configured | Warn and ask user whether to proceed |

Do not merge on red CI unless the user explicitly overrides after seeing the failure list.

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

Present the following to the user with `$NUMBER`, `$SUBJECT`, and `$COMMITS` substituted with their actual values — never show variable names or placeholder text:

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

Do not infer consent from silence, prior approval of the commit message, or any earlier step. The user must respond with an unambiguous "yes" (or "y", "go", "do it") **in direct reply to this prompt**. Any other response — including silence, redirection, or "yes but first..." — means stop.

### Step 5: Execute

Only after explicit confirmation in Step 4:

```bash
gh pr merge "$NUMBER" --repo zeroclaw-labs/zeroclaw --squash \
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
- **Always confirm before merging, no exceptions** — show the user the exact expanded command with real values and require an explicit yes. Never infer consent.
- **If the merge command fails, stop and report verbatim** — do not retry or work around failures automatically.
- **Never delete branches** — not on upstream, not on forks. Branch cleanup is always the contributor's decision. Never suggest a deletion command.
- **Self-merge note:** Maintainers routinely merge their own PRs. If the user is the PR author, proceed normally — just note it in the confirmation summary so it's visible in the audit trail.
