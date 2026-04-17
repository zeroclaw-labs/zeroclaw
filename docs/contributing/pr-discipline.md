# PR Discipline

Rules for pull request quality, attribution, privacy, and handoff in ZeroClaw.

## Privacy / Sensitive Data (Required)

Treat privacy and neutrality as merge gates, not best-effort guidelines.

- Never commit personal or sensitive data in code, docs, tests, fixtures, snapshots, logs, examples, or commit messages.
- Prohibited data includes (non-exhaustive): real names, personal emails, phone numbers, addresses, access tokens, API keys, credentials, IDs, and private URLs.
- Use neutral project-scoped placeholders (e.g., `user_a`, `test_user`, `project_bot`, `example.com`) instead of real identity data.
- Test names/messages/fixtures must be impersonal and system-focused; avoid first-person or identity-specific language.
- If identity-like context is unavoidable, use ZeroClaw-scoped roles/labels only (e.g., `ZeroClawAgent`, `ZeroClawOperator`, `zeroclaw_user`).
- Recommended identity-safe naming palette:
    - actor labels: `ZeroClawAgent`, `ZeroClawOperator`, `ZeroClawMaintainer`, `zeroclaw_user`
    - service/runtime labels: `zeroclaw_bot`, `zeroclaw_service`, `zeroclaw_runtime`, `zeroclaw_node`
    - environment labels: `zeroclaw_project`, `zeroclaw_workspace`, `zeroclaw_channel`
- If reproducing external incidents, redact and anonymize all payloads before committing.
- Before push, review `git diff --cached` specifically for accidental sensitive strings and identity leakage.

## When to Supersede (Required)

Superseding a contributor PR is appropriate in a limited set of situations. Before opening a superseding PR, consider the alternatives in this order:

1. **Push fixups to the contributor's branch.** If the contributor PR has `maintainerCanModify: true` (the default for PRs from personal forks — check with `gh pr view <number> --json maintainerCanModify`), push small fixes directly to their branch and merge the contributor's PR. This preserves full attribution in `git log`, `git blame`, and the contributor's GitHub profile. Coordinate with the contributor if the fix isn't trivial — pushing to their branch while they have unpushed local work creates conflicts they'll need to resolve. If the contributor is actively iterating, prefer option 2 below.

2. **Leave a review with specific requested changes.** If the contributor is active and the fix is within their scope (e.g., a single clippy lint, an edge case, a test addition), request the change and give them an opportunity to push a fixup commit. Single-line fixes are usually better handled by requesting the change or pushing a fixup directly.

3. **Open a follow-up PR after merging.** If the contributor PR is correct as-is and additional hardening is needed, merge the contributor PR first, then open a separate hardening PR. Preserves attribution; the cost is a brief window with known issues on master.

Supersede when one or more of the following apply:

- The contributor is unresponsive (no reply within the project's review SLA).
- The change requires substantially more work than the contributor's original scope.
- Multiple related contributor PRs need to be unified into a single coherent change.
- The contributor has opted out of maintainer edits (`maintainerCanModify: false`) and a follow-up PR is impractical.

When superseding is the right choice, follow the attribution rules in the next section. Always include `Co-authored-by` trailers for materially incorporated contributors, regardless of the circumstances that led to the supersede.

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
