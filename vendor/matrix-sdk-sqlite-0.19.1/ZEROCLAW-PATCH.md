# Temporary rusqlite compatibility patch

Source: matrix-sdk-sqlite 0.19.1, published crates.io archive.
Archive SHA-256: 8d4271e06d52a17bc6ec39b5a2985b65dcced737969fac3678967783656edb34.
Upstream commit: b18166c68bb958a21f0bca8b2d8320cb53583362.
License: Apache-2.0 (see LICENSE and source file headers).

The rusqlite requirement in Cargo.toml and Cargo.toml.orig changes from
0.40.2 to 0.41. Two TODO comments in src/crypto_store.rs are normalized for
the host repository comment-hygiene gate; their original upstream reference
is matrix-org/matrix-rust-sdk#2000. Executable Rust code and all migrations
are unchanged from the published archive. Cargo cache metadata and the crate's standalone Cargo.lock are omitted.
This lets the workspace resolve libsqlite3-sys 0.39.1 (SQLite 3.54.1), repairing
RUSTSEC-2026-0459 rather than suppressing the advisory. Related upstream issue:
https://github.com/zeroclaw-labs/zeroclaw/issues/12320.

Remove this directory, the workspace exclusion and crates.io patch once a
published matrix-sdk-sqlite release accepts rusqlite 0.41 or later. Keep the
security regression and the patched SQLite version when removing this bridge.
