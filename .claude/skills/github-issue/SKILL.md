# Skill: github-issue

File a structured GitHub issue (bug report or feature request) for ZeroClaw interactively from Claude Code.

## When to Use

Trigger when the user wants to file a GitHub issue, report a bug, or request a feature for ZeroClaw. Keywords: "file issue", "report bug", "feature request", "open issue", "create issue", "github issue".

## Instructions

You are filing a GitHub issue against the ZeroClaw repository using structured issue forms. Follow this workflow exactly.

### Step 1: Detect Issue Type and Read the Template

Determine from the user's message whether this is a **bug report** or **feature request**.
- If unclear, use AskUserQuestion to ask: "Is this a bug report or a feature request?"

Then read the corresponding issue template to understand the required fields:

- Bug report: `.github/ISSUE_TEMPLATE/bug_report.yml`
- Feature request: `.github/ISSUE_TEMPLATE/feature_request.yml`

Parse the YAML to extract:
- The `title` prefix (e.g. `[Bug]: `, `[Feature]: `)
- The `labels` array
- Each field in the `body` array: its `type` (dropdown, textarea, input, checkboxes, markdown), `id`, `attributes.label`, `attributes.options` (for dropdowns), `attributes.description`, `attributes.placeholder`, and `validations.required`

This is the source of truth for what fields exist, what they're called, what options are available, and which are required. Do not assume or hardcode any field names or options — always derive them from the template file.

### Step 2: Auto-Gather Context

Before asking the user anything, silently gather environment and repo context:

```bash
# Git context
git log --oneline -5
git status --short
git diff --stat HEAD~1 2>/dev/null

# For bug reports — environment detection
uname -s -r -m                          # OS info
sw_vers 2>/dev/null                     # macOS version
rustc --version 2>/dev/null             # Rust version
cargo metadata --format-version=1 --no-deps 2>/dev/null | jq -r '.packages[] | select(.name=="zeroclaw") | .version' 2>/dev/null   # ZeroClaw version
git rev-parse --short HEAD              # commit SHA fallback
```

Also read recently changed files to infer the affected component and architecture impact.

### Step 3: Pre-Fill and Present the Form

Using the parsed template fields and gathered context, draft values for ALL fields from the template:

- **dropdown** fields: select the most likely option from `attributes.options` based on context. For dropdowns where you're uncertain, note your best guess and flag it for the user.
- **textarea** fields: draft content based on the user's description, git context, and the field's `attributes.description`/`attributes.placeholder` for guidance on what's expected.
- **input** fields: fill with auto-detected values (versions, OS) or draft from user context.
- **checkboxes** fields: auto-check all items (the skill itself ensures compliance with the stated checks).
- **markdown** fields: skip these — they're informational headers, not form inputs.
- **optional fields** (where `validations.required` is false): fill if there's enough context, otherwise note "(optional — not enough context to fill)".

Present the complete draft to the user in a clean readable format:

```
## Issue Draft: [Bug]: <title> / [Feature]: <title>
**Labels**: <from template>

### <Field Label>
<proposed value or selection>

### <Field Label>
<proposed value>
...
```

Use AskUserQuestion to ask the user to review:
- "Here's the pre-filled issue. Please review and let me know what to change, or say 'submit' to file it."

If the user requests changes, update the draft and re-present. Iterate until the user approves.

### Step 4: Scope Guard

Before final submission, analyze the collected content for scope creep:
- Does the bug report describe multiple independent defects?
- Does the feature request bundle unrelated changes?

If multi-concept issues are detected:
1. Inform the user: "This issue appears to cover multiple distinct topics. Focused, single-concept issues are strongly preferred and more likely to be accepted."
2. Break down the distinct groups found.
3. Offer to file separate issues for each group, reusing shared context (environment, etc.).
4. Let the user decide: proceed as-is or split.

### Step 5: Construct Issue Body

Build the issue body as markdown sections matching GitHub's form-field rendering format. GitHub renders form-submitted issues with `### <Field Label>` sections, so use that exact structure.

For each non-markdown field from the template, in order:

```markdown
### <attributes.label>

<value>
```

For optional fields with no content, use `_No response_` as the value (this matches GitHub's native rendering for empty optional fields).

For checkbox fields, render each option as:
```markdown
- [X] <option label text>
```

### Step 6: Final Preview and Submit

Show the final constructed issue (title + labels + full body) for one last confirmation.

Then submit using a HEREDOC for the body to preserve formatting:

```bash
gh issue create --title "<title prefix><user title>" --label "<label1>,<label2>" --body "$(cat <<'ISSUE_EOF'
<body content>
ISSUE_EOF
)"
```

Return the resulting issue URL to the user.

### Important Rules

- **Always read the template file** — never assume field names, options, or structure. The templates are the source of truth and may change over time.
- **Never include personal/sensitive data** in the issue. Redact secrets, tokens, emails, real names.
- **Use neutral project-scoped placeholders** per ZeroClaw's privacy contract.
- **One concept per issue** — enforce the scope guard.
- **Auto-detect, don't guess** — use real command output for environment fields.
- **Quote observed output verbatim** — error messages, stack traces, warnings, and command output must be copy-pasted into the relevant fields (`Steps to reproduce`, `Observed behavior`, `Logs`) exactly as they appeared. Do not paraphrase. Do not summarize. The maintainer searching for this bug later will grep for the exact string; paraphrase breaks that search. If the output is long, include the head and tail with a `...` marker in the middle rather than rewriting it.
- **Match GitHub's rendering** — use `### Field Label` sections so issues look consistent whether filed via web UI or this skill.
