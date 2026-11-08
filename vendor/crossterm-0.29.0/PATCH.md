# Temporary Crossterm terminal-disconnection backport

This directory contains the released crates.io Crossterm 0.29.0 package, retaining its MIT license and original attribution. Source, examples, documentation, and manifests are copied from that package. Its generated Cargo.lock is omitted; the ZeroClaw workspace lockfile owns dependency resolution.

The correction is backported from [crossterm-rs/crossterm PR #1067](https://github.com/crossterm-rs/crossterm/pull/1067), inspected at revision `c006ee6efbd7bed45f1286ec9545d401f3ecb1fe`. The only production behavior delta from released 0.29.0 is in `src/event/source/unix/mio.rs`: return `UnexpectedEof` when terminal input returns zero bytes, and propagate unexpected read errors instead of retrying indefinitely. `WouldBlock` and `Interrupted` retain their existing behavior. This applies the upstream repair to the default Unix backend used by ZeroCode; alternate terminal backends and the async EventStream are unchanged.

ZeroCode already propagates synchronous poll errors to its shutdown cleanup. [Issue #11481](https://github.com/zeroclaw-labs/zeroclaw/issues/11481) tracks the terminal-disconnection spin. `apps/zerocode/tests/terminal_disconnect.rs` exercises the production dependency through a disposable PTY, partial ANSI input and disconnect without SIGHUP, with bounded child cleanup even on failure.

The workspace override covers repository builds and binaries built from them. Cargo does not apply this workspace patch for downstream crates.io installations; those require a released upstream correction. Replace this local copy and remove the override when an upstream release includes the fix and the same PTY regression passes against it.

Two package-integration corrections are also retained. Redundant parentheses in `src/terminal/sys/unix.rs` are removed so the local path dependency compiles under the repository's `-D warnings` CI setting; this does not change behavior. The historical upgrade-manual link in `CHANGELOG.md` points to the upstream upgrade wiki, and the examples README resolves its license links to `../LICENSE`, because the released package omits the original relative targets.

Do not reformat or modernize unrelated upstream source. Keep future changes small and documented here so the local maintenance delta remains reviewable.
