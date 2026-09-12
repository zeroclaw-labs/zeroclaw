# ZeroClaw Rust anti-slop gate

This stable-toolchain checker protects three production-code rules already
stated by ZeroClaw's contributor contract:

- Do not hide unused production code with `#[allow(dead_code)]` or broader
  groups such as `#[allow(unused)]` and `#[allow(warnings)]`.
- Explain every unsafe boundary with a nearby `SAFETY:` comment.
- Propagate production failures or document impossible panic paths with a
  nearby `INVARIANT:` comment or a descriptive `expect("...")` message.

It parses Rust with `syn` so strings, comments, attributes, and test modules
are distinguished reliably. It is developer and CI tooling only: the package is
not published, has no runtime dependents, and is not an agent-callable tool.

## Run the canonical gate

From the repository root:

```sh
bash scripts/ci/anti_slop_delta_gate.sh
```

The wrapper prefers `upstream/master` for local fork work and falls back to
`origin/master`. Pass an explicit base when needed:

```sh
bash scripts/ci/anti_slop_delta_gate.sh origin/master
```

Refresh the relevant tracking ref first. The wrapper deliberately performs no
network operation and prints the exact base ref and commit it checked.

The checker diffs from the merge-base through the current working tree, so the
scan includes committed, staged, unstaged, and untracked Rust files. It reports
findings on touched lines and findings newly exposed by context changes, such
as deleting a `SAFETY:` comment or removing `cfg(test)`. An unchanged finding
that already existed at the merge-base does not block an unrelated branch.

The PR-authoring skill treats this command as a pre-submission gate. CI runs the
same command in advisory mode for 20 representative Rust PRs while false
positives are recorded in #10118; removing the workflow's `continue-on-error`
then promotes it to required enforcement without changing the checker or
contributor command.

Diagnostics use:

```text
path:line:column: rule: message
```

Exit status `0` means clean, `1` means findings were reported, and `2`
means the scan could not complete.

## Remediate findings

### `no-dead-code-allow`

Remove the unused item, connect it to its intended production path, or place a
test-only helper behind a precise test configuration. This also applies to
conditional `cfg_attr` suppression. A deliberately retained compatibility
surface may use a narrowly scoped compiler-checked expectation with a concrete
reason:

```rust
#[expect(dead_code, reason = "public compatibility surface")]
```

Do not replace a broad allowance with an underscore name or another lint escape
hatch.

### `require-safety-comment-for-unsafe`

Keep the unsafe operation only when necessary. State why its actual
preconditions hold:

```rust
// SAFETY: the descriptor is owned by this process and remains live for the call.
unsafe { invoke_descriptor(fd) }
```

### `require-invariant-comment-for-panics`

Prefer returning or propagating an error. When a panic is genuinely impossible,
state the checked invariant:

```rust
// INVARIANT: validation above guarantees at least one route.
let route = routes.first().unwrap();
```

A nonempty literal `expect("...")` message also documents the invariant.
Production `panic!`, `todo!`, `unimplemented!`, and `unreachable!`
require the same justification. Test-only panic paths are excluded.

## Full-tree accounting

To inspect the repository baseline without printing every diagnostic:

```sh
cargo run --locked -p zeroclaw-anti-slop -- --summary .
```

The pull-request gate uses changed-line mode. Full-tree scans are for cleanup
accounting and release of the legacy baseline.

## Dependency boundary

- `syn` parses Rust on the pinned stable toolchain.
- `proc-macro2` provides line and column spans for actionable diagnostics.
- `tempfile` is test-only and creates an isolated real Git repository for
  merge-base, worktree, and untracked-file coverage.

All three dependencies and resolved versions already exist in ZeroClaw's
workspace graph; this tool adds no new third-party package versions.

## Known limits

- The checker validates the presence and placement of comments, not whether a
  justification is true; review still owns the semantic claim.
- Arbitrary macro token grammars are opaque to `syn`. Named panic macros are
  checked, but an `unwrap()` embedded inside another macro invocation may rely
  on Clippy or review.
- This is a source-policy gate, not compiler type analysis. It does not replace
  formatting, Clippy, tests, or architecture checks.
