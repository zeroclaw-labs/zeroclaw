# Provenance
Vendored from the `gemini-live` crate (MIT) originally developed in the `kutsu` project.
Origin commit: f555bf4e8bd6a15ad29052c0def2a059478f2558.
Local changes tracked in this repo's history. Upstream license: see LICENSE (MIT).

## Package name

The crate is published as `zeroclaw-gemini-live`, not `gemini-live`: the plain
name on crates.io belongs to an unrelated client
(`jacoblincool/gemini-live-rs`), so a published `zeroclaw-channels` asking for
`gemini-live` would resolve to an implementation that does not have this API.
The name is therefore ZeroClaw's, and the package takes the workspace version
so the coordinated release publishes it ahead of the channel that depends on
it. The upstream source lives on in `metalmon/gemini-live`; this directory
stays a diffable vendor of it.

## Intentional divergence from workspace conventions

The crate keeps its own `edition = "2021"` and `license = "MIT"` in
`Cargo.toml` rather than inheriting them from the ZeroClaw workspace — the
edition so the source stays a faithful, diffable vendor of the origin commit
above, and the license because the vendored code is MIT (see LICENSE).

The `KUTSU_GEMINI_DIAG` environment-variable branches in `src/session.rs`
(gated diagnostic logging around inbound server messages and outbound audio
send latency) are verbatim-preserved from the upstream `kutsu` code, not
ZeroClaw additions — kept as-is rather than rewritten to ZeroClaw's own
logging conventions so the crate stays a faithful, diffable vendor of the
origin commit above.
