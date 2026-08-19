# ZeroClaw anti-slop for Rust

This is a stable-toolchain Rust source checker inspired by
[`dmmulroy/anti-slop`](https://github.com/dmmulroy/anti-slop). It translates the
original project's low-evidence policy into Rust concepts and adds the two
production-code rules already stated in ZeroClaw's contributor contract.

The CLI defaults to the `zeroclaw` policy profile. It preserves ZeroClaw's
canonical `serde_json::Value` wire/tool contracts, legitimate schema `Shape`
vocabulary, documented `expect(...)` invariants, and test-only typed-error
downcasts. Pass `--profile strict` for the upstream-style interpretation that
treats those patterns as suspect too.

The checker is intentionally a normal workspace binary built on `syn`, not a
`rustc_private` or Dylint plugin. That keeps it usable with ZeroClaw's pinned
stable toolchain. It is AST-aware, but it does not claim compiler type
resolution.

## Usage

Check only lines introduced on the current branch:

```sh
just anti-slop origin/master
```

The equivalent direct command is:

```sh
cargo run --locked -p zeroclaw-anti-slop -- \
  --changed-since origin/master
```

`--changed-since` diffs from the merge-base to the current working tree, so it
includes committed, staged, and unstaged changes. Untracked Rust files are
checked in full. This delta mode lets ZeroClaw adopt stronger policy without
requiring unrelated legacy cleanup. The checker's own `tools/anti-slop/` source
is excluded, matching the upstream project's treatment of its vendored plugin.

To inspect every Rust file under explicit roots:

```sh
cargo run --locked -p zeroclaw-anti-slop -- crates/zeroclaw-api src
```

For a repository-wide baseline without printing every diagnostic:

```sh
cargo run --locked -p zeroclaw-anti-slop -- \
  --summary src crates apps xtask tools tests benches firmware
```

Summary mode reports counts by rule and the 20 highest-volume files.

To list rule identifiers:

```sh
cargo run --locked -p zeroclaw-anti-slop -- --list-rules
```

Diagnostics use `path:line:column: rule: message` and the process exits `1` for
violations or `2` when a file/git operation cannot be checked.

## Rule mapping

| Rust rule | Upstream idea |
|---|---|
| `no-chained-casts` | `no-chained-type-assertions` |
| `no-known-value-widening` | `no-known-value-widening` |
| `no-mock-macros` | `no-module-mocking` |
| `no-shape-in-symbol-names` | same rule |
| `no-erased-parameter-types` | `no-unknown-parameters` + `no-object-parameters` |
| `no-erased-return-types` | `no-unknown-returns` |
| `no-erased-type-aliases` | `no-unknown-type-aliases` |
| `no-unsafe-dictionary-types` | same rule, using string-keyed Rust maps |
| `no-runtime-downcasting` | `no-runtime-typeof`, `no-reflect-get`, and widen/assert flows |
| `require-safety-comment-for-unsafe` | safety comments for type assertions |
| `require-invariant-comment-for-panics` | ZeroClaw's documented-panic contract |
| `no-dead-code-allow` | ZeroClaw's no-suppressed-production-code contract |

`no-conditional-empty-object-spread` has no Rust equivalent. The Effect-specific
service-constructor rule is also not ported. ZeroClaw's provider-dispatch gate in
`scripts/ci/rust_quality_gate.sh` already owns the comparable application rule,
so this tool does not duplicate it.

## Known limits

- Imported `serde_json::Value` aliases are followed within a file; arbitrary
  cross-module re-exports are not.
- Runtime downcast checks are syntax-based and intentionally opinionated.
- Macro-expanded code is outside `syn`'s view; macro invocations themselves are
  checked where a rule applies.
- Changed-line mode reports a diagnostic only when its primary span begins on a
  changed line.
