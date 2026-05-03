# Building the web dashboard

The web dashboard at `web/` is a Vite + React + TypeScript app. Its TypeScript API client is generated from the gateway's runtime OpenAPI spec, not hand-written. Both the spec snapshot and the generated client are derived artifacts — neither is committed.

## Quickstart

```bash
cargo web build         # production bundle into web/dist/
cargo web dev           # vite dev server with HMR
cargo web check         # typecheck only (gen-api + tsc -b)
cargo web gen-api       # regenerate web/src/lib/api-generated.ts
cargo web install       # npm install in web/
```

`cargo web` is an alias for `cargo run -p xtask --bin web --` (defined in `.cargo/config.toml`). Every subcommand auto-runs `npm install` if `web/node_modules/` is missing.

## What gets generated

| Path                            | Generator                | Tracked?   |
| ------------------------------- | ------------------------ | ---------- |
| `web/src/lib/api-generated.ts`  | `cargo web gen-api`      | gitignored |
| `target/openapi.json`           | `cargo web gen-api`      | gitignored |
| `web/dist/`                     | `cargo web build`        | gitignored |

`cargo web gen-api` renders the OpenAPI spec in-process from `zeroclaw_gateway::openapi::build_spec()`, writes it to `target/openapi.json`, and feeds that file to `openapi-typescript`. The same `build_spec()` serves `/api/openapi.json` at runtime, so the spec on disk is never the source of truth — it is a transient handoff between Rust and the TS codegen.

## Why nothing is committed

The OpenAPI spec is ~10K lines of JSON. The generated TypeScript client is ~7800 lines. Both regenerate deterministically from the gateway's `schemars`-derived types. Committing them would mean:

- ~17K lines of churn on every PR that touches a gateway handler or request/response type
- A CI staleness check that catches drift but does not catch downstream type errors
- A second source of truth that can desync from the runtime spec

Generating on demand keeps the runtime `build_spec()` as the single contract source.

## Editing flow

1. Change a gateway handler or schema in `crates/zeroclaw-gateway/`.
2. Run `cargo web check` — `gen-api` regenerates `api-generated.ts` from the new spec, then `tsc -b` typechecks the dashboard against it. Any consumer that relies on a now-removed field fails to compile.
3. Update consumers in `web/src/` to match.
4. `cargo web build` for the final bundle.

## CI and release builds

CI does not run `cargo web build` — the lint/build/test jobs use a `web/dist/.gitkeep` placeholder so the gateway crate compiles without the bundle. Producing a release artifact that includes the dashboard is a separate step:

```bash
cargo web build
cargo build --release --features gateway
```

The gateway loads `web/dist/` from the filesystem at runtime via `static_files.rs`, so the Rust compile and the web build are decoupled. Ship the populated `web/dist/` alongside the binary for installs that should serve the dashboard.

## Required tools

| Tool   | Install                                |
| ------ | -------------------------------------- |
| `npm`  | <https://nodejs.org/> or `nvm install --lts` |
| `cargo`| <https://rustup.rs>                    |

`cargo web` fails fast with an install hint if `npm` is missing.
