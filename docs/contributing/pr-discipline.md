# PR Discipline

Rules for pull request quality, attribution, privacy, and handoff in JhedaiClaw.

## Privacy / Sensitive Data (Required)

Treat privacy and neutrality as merge gates, not best-effort guidelines.

- Never commit personal or sensitive data in code, docs, tests, fixtures, snapshots, logs, examples, or commit messages.
- Prohibited data includes (non-exhaustive): real names, personal emails, phone numbers, addresses, access tokens, API keys, credentials, IDs, and private URLs.
- Use neutral project-scoped placeholders (e.g., `user_a`, `test_user`, `project_bot`, `example.com`) instead of real identity data.
- Test names/messages/fixtures must be impersonal and system-focused; avoid first-person or identity-specific language.
- If identity-like context is unavoidable, use JhedaiClaw-scoped roles/labels only (e.g., `JhedaiClawAgent`, `JhedaiClawOperator`, `jhedaiclaw_user`).
- Recommended identity-safe naming palette:
  - actor labels: `JhedaiClawAgent`, `JhedaiClawOperator`, `JhedaiClawMaintainer`, `jhedaiclaw_user`
  - service/runtime labels: `jhedaiclaw_bot`, `jhedaiclaw_service`, `jhedaiclaw_runtime`, `jhedaiclaw_node`
  - environment labels: `jhedaiclaw_project`, `jhedaiclaw_workspace`, `jhedaiclaw_channel`
- If reproducing external incidents, redact and anonymize all payloads before committing.
- Before push, review `git diff --cached` specifically for accidental sensitive strings and identity leakage.

## Superseded-PR Attribution (Required)

When a PR supersedes another contributor's PR and carries forward substantive code or design decisions, preserve authorship explicitly.

- In the integrating commit message, add one `Co-authored-by: Name <email>` trailer per superseded contributor whose work is materially incorporated.
- Use a GitHub-recognized email (`<login@users.noreply.github.com>` or the contributor's verified commit email).
- Keep trailers on their own lines after a blank line at commit-message end; never encode them as escaped `\\n` text.
- In the PR body, list superseded PR links and briefly state what was incorporated from each.
- If no actual code/design was incorporated (only inspiration), do not use `Co-authored-by`; give credit in PR notes instead.

## Superseded-PR Templates

### PR Title/Body Template

- Recommended title format: `feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]`
- In the PR body, include:

```md
## Supersedes

- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>

## Integrated Scope

- From #<pr_a>: <what was materially incorporated>
- From #<pr_b>: <what was materially incorporated>

## Attribution

- Co-authored-by trailers added for materially incorporated contributors: Yes/No
- If No, explain why

## Non-goals

- <explicitly list what was not carried over>

## Risk and Rollback

- Risk: <summary>
- Rollback: <revert commit/PR strategy>
```

### Commit Message Template

```text
feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]

<one-paragraph summary of integrated outcome>

Supersedes:
- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>

Integrated scope:
- <subsystem_or_feature_a>: from #<pr_x>
- <subsystem_or_feature_b>: from #<pr_y>

Co-authored-by: <Name A> <login_a@users.noreply.github.com>
Co-authored-by: <Name B> <login_b@users.noreply.github.com>
```

## Handoff Template (Agent -> Agent / Maintainer)

When handing off work, include:

1. What changed
2. What did not change
3. Validation run and results
4. Remaining risks / unknowns
5. Next recommended action
