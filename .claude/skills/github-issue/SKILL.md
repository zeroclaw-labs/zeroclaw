# Skill: github-issue

File a structured GitHub issue for ZeroClaw interactively from Claude Code.

## When to Use

Trigger when the user wants to create or route a ZeroClaw GitHub issue through the repository's current issue forms. Keywords: "file issue", "report bug", "feature request", "RFC", "tracker", "docs issue", "support issue", "contributor task", "open issue", "create issue", "github issue".

## Instructions

You are filing a GitHub issue against the ZeroClaw repository using structured issue forms. Follow this workflow exactly.

### Step 1: Route the Request and Read the Template

Read `.github/ISSUE_TEMPLATE/config.yml` first. It is contact-link metadata, not an issue form. Use it to route requests before drafting a public issue:

- Security vulnerabilities: use the private security reporting route. Do not draft or file a public issue.
- Non-security content containing secrets, tokens, private URLs, personal data, or sensitive logs: stop, redact or sanitize the content, then continue only with safe public text.
- Quick non-durable setup or usage help: use the configured Discord/support contact link unless durable tracking is needed.
- Community Q&A, show-and-tell, polls, or early design exploration: use the configured Discussions contact link unless durable tracking is needed.
- Contribution mechanics, RFC process, or PR workflow questions: use the relevant docs or contact link unless durable tracking is needed.
- Durable tracked bugs, features, RFCs, docs gaps, support/configuration records, and contributor tasks: continue to the issue forms.

Discover the issue forms from the current repository. Enumerate `.github/ISSUE_TEMPLATE/*.yml` excluding `config.yml`, then parse each form's `name`, `description`, `title`, `labels`, and `body`.

Choose the best form from the parsed inventory. If the type is unclear, use AskUserQuestion with the parsed form names and descriptions. Do not collapse unclear issues to bug or feature by default.

Then read the selected issue template to understand the required fields:

Parse the YAML to extract:
- The `title` prefix, for example `[Bug]:`, `[Feature]:`, `RFC:`, or `[Tracker]:`
- The `labels` array, if present
- Each field in the `body` array: its `type` (dropdown, textarea, input, checkboxes, markdown), `id`, `attributes.label`, `attributes.options` (for dropdowns and checkboxes), `attributes.description`, `attributes.placeholder`, `attributes.render` (for rendered text areas), and `validations.required`

This is the source of truth for what forms exist, what fields exist, what they're called, what options are available, and which fields are required. Do not assume or hardcode any form names, field names, labels, or options; always derive them from the current template files.

### Step 2: Auto-Gather Context

After selecting a form, silently gather only the environment and repo context that maps to the selected template fields. Use these helpers when the selected fields ask for version, operating system, Rust toolchain, current behavior, reproduction, or recent local changes:

```bash
# Git context
git log --oneline -5
git status --short
git diff --stat HEAD~1 2>/dev/null

# For bug reports and support/configuration issues: environment detection
uname -s -r -m                          # OS info
sw_vers 2>/dev/null                     # macOS version
rustc --version 2>/dev/null             # Rust version
cargo metadata --format-version=1 --no-deps 2>/dev/null | jq -r '.packages[] | select(.name=="zeroclaw") | .version' 2>/dev/null   # ZeroClaw version
git rev-parse --short HEAD              # commit SHA fallback
```

Also read recently changed files when they help infer the affected component, docs location, contributor scope, or architecture impact. For RFC/design, roadmap/tracker, docs, support/configuration, and contributor-task issues, search named paths/components and one or two exact title or keyword queries for related issues, PRs, RFCs, docs, or code paths before drafting. If no obvious related work appears, say so instead of widening the search indefinitely.

### Step 3: Pre-Fill and Present the Form

Using the parsed template fields and gathered context, draft values for ALL fields from the template:

- **dropdown** fields: select the most likely option from `attributes.options` based on context. For dropdowns where you're uncertain, note your best guess and flag it for the user.
- **textarea** fields: draft content based on the user's description, git context, and the field's `attributes.description`/`attributes.placeholder` for guidance on what's expected. If the field has `attributes.render`, preserve exact output and plan to wrap the value in a fenced code block using that render language in Step 5.
- **input** fields: fill with auto-detected values (versions, OS) or draft from user context.
- **checkboxes** fields: auto-check only items you actually enforced or verified. For external attestations such as latest `master` reproduction, user confirmation, or upstream CI state, either gather real evidence, ask the user, or leave the item unchecked with a note that confirmation is needed.
- **markdown** fields: skip these; they're informational headers, not form inputs.
- **optional fields** (where `validations.required` is false): fill if there's enough context, otherwise note "(optional, not enough context to fill)".

Present the complete draft to the user in a clean readable format:

```
## Issue Draft: <template title prefix><title>
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
- Is an RFC/design proposal being filed as an ordinary feature request?
- Is an active coordination surface being filed as one ordinary bug or feature instead of a roadmap/tracker?
- Is a docs-only gap being mixed with a behavior change that should have its own bug or feature issue?

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

For textarea fields with `attributes.render`, wrap the value in a fenced code block using that render language:

````markdown
### <attributes.label>

```<attributes.render>
<exact value>
```
````

For checkbox fields, render each option as:
```markdown
- [X] <option label text>
```

### Step 6: Final Preview and Submit

Show the final constructed issue (title + labels + full body) for one last confirmation. If the selected template has no labels, show `Labels: none` and omit `--label` from the create command.

Then submit using a HEREDOC for the body to preserve formatting:

```bash
gh issue create --title "<title prefix><user title>" --label "<label1>,<label2>" --body "$(cat <<'ISSUE_EOF'
<body content>
ISSUE_EOF
)"
```

When the selected template has no labels:

```bash
gh issue create --title "<title prefix><user title>" --body "$(cat <<'ISSUE_EOF'
<body content>
ISSUE_EOF
)"
```

Return the resulting issue URL to the user.

### Important Rules

- **Always discover and read the current template files**: enumerate issue forms from `.github/ISSUE_TEMPLATE/*.yml` excluding `config.yml`, then parse the selected template. Never assume field names, options, labels, render modes, or structure.
- **Use `config.yml` as a routing gate**: route private security reports, quick support, Discussions, and docs/process contacts before drafting a durable public issue.
- **Never include personal/sensitive data** in the issue. Redact secrets, tokens, emails, real names, private URLs, and sensitive logs before public drafting. Security vulnerabilities must use the private route instead.
- **Use neutral project-scoped placeholders** per ZeroClaw's privacy contract.
- **One concept per issue**: enforce the scope guard.
- **Auto-detect, don't guess**: use real command output for environment fields.
- **Quote observed output verbatim**: error messages, stack traces, warnings, and command output must be copy-pasted into the relevant fields (`Steps to reproduce`, `Observed behavior`, `Logs`) exactly as they appeared. Do not paraphrase. Do not summarize. The maintainer searching for this bug later will grep for the exact string; paraphrase breaks that search. If the output is long, include the head and tail with a `...` marker in the middle rather than rewriting it.
- **Match GitHub's rendering**: use `### Field Label` sections so issues look consistent whether filed via web UI or this skill.
