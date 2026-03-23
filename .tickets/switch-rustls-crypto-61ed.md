---
id: switch-rustls-crypto-61ed
stage: triage
deps: []
links: []
created: 2026-03-23T12:28:13Z
type: task
priority: 2
assignee: Dustin Reynolds
version: 1
---
# switch rustls crypto backend from aws-lc-rs to ring for cross-compilation


cargo zigbuild for aarch64-apple-darwin fails because aws-lc-sys requires compiling C code with platform-specific macOS headers that zig's bundled Darwin libc doesn't cover. Switching rustls to use the ring crypto provider instead of aws-lc-rs unblocks Darwin cross-compilation from Linux. Current state: rustls 0.23 defaults to aws-lc-rs. ring = 0.17 is already a direct dependency. Change: configure rustls CryptoProvider to use ring, or use feature flags to select ring backend. Must verify no regressions in TLS behavior across reqwest, matrix-sdk, tokio-tungstenite, lettre.
