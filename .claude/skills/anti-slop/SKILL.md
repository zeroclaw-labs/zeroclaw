---
name: anti-slop
description: "Run and remediate ZeroClaw's canonical changed-line Rust anti-slop gate. Use for Rust changes before opening or updating a PR, when CI reports an anti-slop failure, or when asked to check production dead-code suppression, unsafe justification, or panic invariants. Trigger on: anti-slop, slop check, dead_code allow, SAFETY comment, INVARIANT comment, undocumented unsafe, undocumented panic."
---

# ZeroClaw Rust Anti-Slop Gate

Use the repository wrapper as the only entry point. This keeps local, skill,
and CI behavior identical and does not require `just`.

## Run

From the repository root:

```sh
bash scripts/ci/anti_slop_delta_gate.sh
```

For an existing PR, discover its base branch with `gh pr view --json
baseRefName` and map the base repository to its local remote with `git remote
-v`. Refresh that tracking ref before relying on the result; the wrapper is
intentionally offline and prints the exact ref and commit it used.

Pass an explicit tracking ref when the PR does not use the wrapper's default
`upstream/master` or `origin/master`:

```sh
bash scripts/ci/anti_slop_delta_gate.sh upstream/release-0.8
```

The gate includes committed, staged, unstaged, and untracked Rust changes. It
checks findings on touched lines plus findings newly exposed by context changes
such as a deleted justification. Do not replace the wrapper with a direct Cargo
command.

## Interpret

- Exit `0`: the checked diff is clean.
- Exit `1`: policy findings were found; remediate them and rerun.
- Exit `2`: the scan could not complete; fix or report the operational error.
- Diagnostics use `path:line:column: rule: message`.
- The first output line identifies the base ref and abbreviated commit.

Do not treat an incomplete or stale run as passing evidence.

## Remediate

- `no-dead-code-allow`: remove the unused production item or connect it to
  its intended path. For a deliberately retained compatibility surface, use
  only a narrowly scoped, compiler-checked expectation with a concrete reason
  when repository policy permits it; never add a broad allowance.
- `require-safety-comment-for-unsafe`: keep the unsafe operation only when
  necessary and add a nearby `SAFETY:` comment stating why its actual
  preconditions hold.
- `require-invariant-comment-for-panics`: prefer returning or propagating an
  error. When panic is genuinely impossible, document the fact with a specific
  `INVARIANT:` comment or descriptive `expect` message.

Comments must explain the real condition. Tag-only claims such as
`SAFETY: safe` or `INVARIANT: cannot happen` are not acceptable even if the
syntax checker recognizes them.

If a diagnostic appears incorrect, preserve a minimal reproducer and report
the checker issue rather than bypassing the gate.

After remediation, run any focused test needed for behavioral edits, then
rerun the canonical gate.

## Report

Report the exact command, final success line, files changed, and any focused
tests. When preparing a PR, include the successful command and output in
`How I tested`.
