# wikipedia-summary

A reference **WIT component** plugin for ZeroClaw. Given a topic title it returns
a short factual summary from the public Wikipedia REST API. It's the canonical
example of the wasmtime component-model authoring path: it implements the
`tool` + `plugin-info` exports and calls the host `http-request` import.

## Build

```bash
rustup target add wasm32-wasip2
cd plugins/wikipedia-summary
cargo build --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/wikipedia_summary.wasm wikipedia_summary.wasm
```

`cargo build --target wasm32-wasip2` produces a component directly (no
`wasm-tools component new` step needed).

## Install

Copy the plugin directory (the `.wasm` next to its `manifest.toml`) into your
configured plugins dir, then enable plugins:

```toml
[plugins]
enabled = true

[http_request]
# The host enforces its SSRF allowlist for plugin HTTP too.
allowed_domains = ["en.wikipedia.org"]
```

Run the agent with a build that includes a compiler backend, e.g.
`--features plugins-wasm,plugins-wasm-cranelift`.

## What it demonstrates

- Authoring a plugin with `wit-bindgen` against `wit/v0` (world `tool-plugin`).
- Declaring capabilities/permissions in `manifest.toml` (`http_client`).
- Calling the host `http-request` capability; the host applies the SSRF
  allowlist and (when configured) injects credentials — the guest never sees
  secret values.

See `docs/book/src/developing/plugin-protocol.md` for the full protocol.
