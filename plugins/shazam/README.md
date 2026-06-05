# shazam plugin

A ZeroClaw WASM plugin (Extism) that looks up tracks in the Shazam catalogue
via a third-party **RapidAPI** Shazam service. It mirrors the native
`ShazamTool` but runs sandboxed as a plugin.

> **Unofficial wrapper.** Shazam has no free public API; this calls a RapidAPI
> service (default host `shazam.p.rapidapi.com`) that may rate-limit or change
> response shapes without notice. Best-effort.

## Actions

- `search_track` — text search by title/artist (`query`, optional `limit` 1–25,
  default 5).
- `get_track_details` — full metadata for a Shazam `track_key` (returned by
  `search_track`).

Optional args: `locale` (BCP-47, default `en-US`), `host` (RapidAPI host
override).

## Configuration

The RapidAPI key is read from the **`SHAZAM_RAPIDAPI_KEY`** environment
variable (via the `env_read` host permission). There is no config-file surface;
credentials come from the environment only.

Manifest permissions: `http_client`, `env_read`.

## Build & install

```bash
rustup target add wasm32-wasip1
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/shazam.wasm shazam.wasm
# install into the plugins dir ZeroClaw scans:
cp -r . ~/.zeroclaw/plugins/shazam/
```

Then enable plugins in your ZeroClaw config (`[plugins] enabled = true`) and set
`SHAZAM_RAPIDAPI_KEY`. The tool registers as `shazam`.

The built `shazam.wasm` and `target/` are git-ignored — commit source +
`manifest.toml` only; rebuild the `.wasm` from source.

## Differences vs the native tool

- Config comes from the environment, not `config.toml`.
- No `SecurityPolicy` allowlist gating (acceptable for a read-only public API).
- Synchronous execution; HTTP runs through the host's `zc_http_request` (fixed
  120s ceiling).
- Successful output carries a fidelity footer naming the data source and the
  fields actually present in the response.
