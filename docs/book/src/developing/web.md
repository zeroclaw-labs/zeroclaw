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

## Dashboard i18n

The dashboard keeps its locale registry in `web/src/lib/i18n/` instead of one
large translation file:

| Path | Purpose |
| ---- | ------- |
| `web/src/lib/i18n.ts` | Runtime locale state and translation helpers |
| `web/src/lib/i18n/types.ts` | `Locale` and `LocaleMessages` types |
| `web/src/lib/i18n/supportedLocales.ts` | Language picker metadata |
| `web/src/lib/i18n/translations.ts` | Per-locale module registry |
| `web/src/lib/i18n/locales/*.ts` | Locale message maps |

React components should not hardcode visible dashboard copy. Use `t()` for normal
strings and `tWithFallback()` when a backend-supplied English fallback is still
valid. Schema-driven config editor copy has dedicated helpers in
`web/src/lib/i18n.ts`:

- `tConfigLabel(path, fallback)` for field labels.
- `tConfigDescription(path, fallback)` for schema doc-comment descriptions.
- `tConfigPlaceholder(path, fallback)` for input placeholders.
- `tConfigSectionLabel()` / `tConfigGroupLabel()` / picker helpers for Config
  navigation and picker rows.

Config paths may arrive as kebab-case (`api-key`) or snake_case (`api_key`),
especially for nested object-array fields derived from JSON Schema. The helper
layer normalizes both forms, tries wildcard candidates for dynamic map sections
such as `providers.models.*.<field>` and `channels.*.<field>`, then falls back to
English and finally to the schema-provided text. Prefer wildcard translation keys
for user-defined map keys so contributors do not need one translation per local
provider or channel name.

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
