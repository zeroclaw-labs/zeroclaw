# Provenance
Vendored from the `gemini-live` crate (MIT) originally developed in the `kutsu` project.
Origin commit: f555bf4e8bd6a15ad29052c0def2a059478f2558.
Local changes tracked in this repo's history. Upstream license: see LICENSE (MIT).

## Intentional divergence from workspace conventions

This crate is dual-published (`metalmon/gemini-live` upstream, vendored here) and
intentionally keeps its own `edition = "2021"`, `version`, and `license` in
`Cargo.toml` rather than inheriting them from the ZeroClaw workspace — that's
what keeps it buildable and publishable as a standalone crate outside this
repo, in parity with the upstream `kutsu` original.

The `KUTSU_GEMINI_DIAG` environment-variable branches in `src/session.rs`
(gated diagnostic logging around inbound server messages and outbound audio
send latency) are verbatim-preserved from the upstream `kutsu` code, not
ZeroClaw additions — kept as-is rather than rewritten to ZeroClaw's own
logging conventions so the crate stays a faithful, diffable vendor of the
origin commit above.
