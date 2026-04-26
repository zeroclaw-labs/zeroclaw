# Factory Testbench Policy

## Decision Classes

| Class | Action |
|---|---|
| `SNAPSHOT` | Read GitHub state and write local JSON. |
| `REPLAY` | Evaluate factory decisions against a snapshot. |
| `ASSERT` | Fail if safety invariants are violated. |

## Authority

Factory Testbench must not mutate GitHub. It may:

- call `gh issue list` and `gh pr list`;
- write local snapshot, replay, and invariant JSON files;
- optionally create a local bare mirror clone when explicitly requested with `--clone-dir`.

It must not:

- comment, close, label, merge, approve, request changes, or edit branches;
- create a sandbox repository automatically;
- rewrite live issue or PR bodies;
- treat replay output as authority to mutate production without a later Clerk/Inspector preview.

## Snapshot Scope

Snapshots should include enough data for factory policy decisions:

- issues: number, title, body, labels, comments, updated time, URL, author, state;
- PRs: number, title, body, labels, comments, merge time, state, URL, draft flag, files, base branch;
- metadata: repo, generated time, source commit when available.

## Invariants

Replay must fail on:

- `AUTO_CLOSE` for protected labels;
- `AUTO_CLOSE` from `similarity-preview`;
- `AUTO_CLOSE` from `implemented-on-master-preview`;
- `AUTO_CLOSE` for an issue fixed by a PR whose base branch is not `master`;
- any Inspector issue mutation candidate;
- duplicate markers within one replay result.
