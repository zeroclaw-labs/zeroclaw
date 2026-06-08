# Superseding PRs

When a maintainer-authored PR replaces a contributor's open PR, attribution and process discipline keep the contributor relationship healthy. This page is the rulebook.

## Try the alternatives first

Superseding is the heaviest option. Before you open one, try in this order:

1. **Push fixups to the contributor's branch.** If the PR has `maintainerCanModify: true` (the default for PRs from personal forks — confirm with `gh pr view <number> --json maintainerCanModify`), push your fixups directly and merge the contributor's PR. Attribution stays clean in `git log`, `git blame`, and the contributor's GitHub profile. Coordinate with the contributor first if your fix isn't trivial — pushing while they have unpushed work creates conflicts they have to resolve.

2. **Leave a review with specific requested changes.** If the contributor is responsive and the fix is within their original scope (a clippy lint, an edge case, a test addition), request the change and let them push the fixup. Single-line fixes are almost always better as a requested change than a supersede.

3. **Open a follow-up PR after merging.** If the contributor's PR is correct as-is and you want additional hardening, merge first, then open a separate PR. Attribution preserved; the cost is a brief window with known issues on `master`.

Supersede only when one of these applies:

- The contributor is unresponsive (no reply within the project's review SLA).
- The change requires substantially more work than the contributor's original scope.
- Multiple related contributor PRs need to be unified into a single coherent change.
- The contributor opted out of maintainer edits (`maintainerCanModify: false`) and a follow-up PR is impractical.

## Attribution rules

When you do supersede and you carry forward substantive code or design decisions, preserve authorship explicitly:

- Add one `Co-authored-by: Name <email>` trailer per superseded contributor whose work was materially incorporated. Use a GitHub-recognized email — either the contributor's `<login@users.noreply.github.com>` form or their verified commit email.
- Trailers go on their own lines after a blank line at the end of the commit message. Never encode them as escaped `\n` text.
- In the PR body, list the superseded PR links and briefly state what was incorporated from each.
- If no actual code or design was incorporated (only inspiration), don't use `Co-authored-by` — give credit in the PR notes section instead.

These trailers route GitHub's contributor recognition correctly. Without them, the original author shows up as "Closed" on their PR with no record of the carry-forward.

## PR title and body template

```md
feat(<scope>): unify and supersede #<pr_a>, #<pr_b> [and #<pr_n>]
```

```md
## Supersedes

- #<pr_a> by @<author_a>
- #<pr_b> by @<author_b>

## Integrated scope

- From #<pr_a>: <what was materially incorporated>
- From #<pr_b>: <what was materially incorporated>

## Attribution

- `Co-authored-by` trailers added for materially incorporated contributors: Yes/No
- If No, explain why

## Non-goals

- <explicitly list what was not carried over>

## Risk and rollback

- Risk: <summary>
- Rollback: <revert commit/PR strategy>
```

## Commit message template

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

## Closing the superseded PRs

Close each with a comment that names the new PR and the carry-forward:

```text
Superseded by #<new_pr>. Your work is incorporated as `Co-authored-by` —
specifically the <X> approach in <Y>. Thanks for the original take here;
closing this one in favor of the unified PR.
```

If the contributor pushed back on a particular design choice during their original PR and the supersede took a different direction, name that explicitly. Don't pretend it's a clean carry-forward when it's actually a redesign.

## Handoff template (agent → agent or agent → maintainer)

When handing off mid-flight work, include:

1. **What changed.**
2. **What did not change.**
3. **Validation run and results.**
4. **Remaining risks / unknowns.**
5. **Next recommended action.**

This applies to supersedes that span multiple work sessions, agent-assisted handovers between maintainers, and any case where one person needs to pick up another's in-progress branch.
