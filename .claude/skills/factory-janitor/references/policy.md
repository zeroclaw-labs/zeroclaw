# Factory Janitor Policy

## Decision Classes

| Class | Action |
|---|---|
| `AUTO_CLOSE` | The runner may close in `apply-safe`. |
| `AUTO_COMMENT` | The runner may comment in `comment-only` or `apply-safe`. |
| `QUEUE_FOR_REVIEW` | Human or agent review required; no mutation. |
| `NO_ACTION` | Ignore. |

## Safe Autonomous Closures

### Issues fixed by merged PRs

Allowed only when all are true:

- The PR is merged into `master`.
- The PR body or title explicitly uses a closing keyword for the issue: `closes`, `fixes`, or `resolves`.
- The issue is still open.
- The issue has no protected labels.
- There is no issue activity after the PR merge from a non-bot user.

Closure comment format:

```md
Closing as fixed by #<pr>, merged into master.

The PR explicitly links this issue with a closing keyword. No later issue activity indicates the fix missed the reported scope.
```

### Duplicate issues

Allowed only when all are true:

- A collaborator/member/owner comment explicitly says the issue is a duplicate of another issue.
- The canonical issue exists and is not the same issue.
- The duplicate issue has no protected labels.
- No later maintainer comment disputes the duplicate relationship.

Closure comment format:

```md
Closing as duplicate of #<canonical>.

A maintainer comment already identified the duplicate relationship, and #<canonical> is the better canonical tracker.
```

### Superseded PRs

Allowed only when all are true:

- The open PR has a collaborator/member/owner comment or PR body saying it is superseded/replaced by another PR.
- The superseding PR is merged.
- The open PR is not already draft-only work carrying unique extra scope.

Closure comment format:

```md
Closing as superseded by #<pr>, which has already merged.

The superseding PR carries the lifecycle path forward. Please reopen or follow up if this branch contains unique scope that still needs review.
```

## Safe Autonomous Comments

### Open PR links

Allowed when:

- An open PR explicitly mentions an open issue with `closes`, `fixes`, or `resolves`.
- The issue thread does not already mention that PR.
- A prior Factory Janitor marker for the same action is not already present, unless `--include-marked` is used.

Comment only; never close until the PR merges.

### Partial or related coverage

Do not mutate from the script. Partial coverage requires an agent/human-written comment because the scope nuance matters.

## Protected Targets

Never close issues with any of these labels:

- `security`
- `risk: high`
- `type:rfc`
- `type:tracking`
- `status:blocked`
- `status:needs-maintainer-decision`
- `r:needs-maintainer-decision`
- `no-stale`
- `priority:critical`

Never close PRs with any of these labels:

- `risk: high`
- `status:blocked`
- `status:needs-maintainer-decision`
- `r:needs-maintainer-decision`

## Similarity Candidates

Title/body similarity is useful for discovery but not authority. Similarity-only matches are always `QUEUE_FOR_REVIEW`.

## Implemented-On-Master Candidates

`implemented-on-master-preview` extracts concrete symbols from open issues, such as API routes, CLI commands, config sections, config keys, and backticked identifiers. It then searches the checked-out repository with `rg`.

This check is evidence gathering only:

- Always `QUEUE_FOR_REVIEW`.
- Never comment automatically.
- Never close automatically.
- A match means "the symbol exists somewhere in the checkout", not "the issue is fixed".
- A human or agent must compare the issue acceptance criteria against the code evidence before acting.

## Repeat-Run Safety

Factory Janitor comments include hidden HTML markers. Scheduled runs must skip candidates that already have the same marker. Use `--include-marked` only for audits/debugging.

Non-preview runs must keep a mutation cap (`--max-mutations`) so a bad query or GitHub API shape cannot create a large comment/closure burst.
