# Building the web dashboard

The web dashboard at `web/` is a Vite + React + TypeScript app. Its TypeScript API client is generated from the gateway's runtime OpenAPI spec, not hand-written.

## Quickstart

<div class="os-tabs-src">

#### sh

```sh
cargo web build         # production bundle into web/dist/
cargo web dev           # vite dev server with HMR
cargo web check         # typecheck only (gen-api + tsc -b)
cargo web gen-api       # regenerate web/src/lib/api-generated.ts
cargo web install       # npm install in web/
```

</div>

`cargo web` is an alias for `cargo run -p xtask --bin web --` (defined in the cargo config). Every subcommand auto-runs `npm install` if `web/node_modules/` is missing.

## What gets generated

| Path                            | Generator                | Tracked?   |
| ------------------------------- | ------------------------ | ---------- |
| `web/src/lib/api-generated.ts`  | `cargo web gen-api`      | gitignored |
| `target/openapi.json`           | `cargo web gen-api`      | gitignored |
| `web/dist/`                     | `cargo web build`        | gitignored |

`cargo web gen-api` renders the OpenAPI spec in-process from `zeroclaw_gateway::openapi::build_spec()`, writes it to `target/openapi.json`, and feeds that file to `openapi-typescript`. The same `build_spec()` serves `/api/openapi.json` at runtime, so `build_spec()` is the single contract source and the generated files are rebuilt on demand.

## Editing flow

1. Change a gateway handler or schema in `crates/zeroclaw-gateway/`.
2. Run `cargo web check`: `gen-api` regenerates `api-generated.ts` from the new spec, then `tsc -b` typechecks the dashboard against it. Any consumer that relies on a now-removed field fails to compile.
3. Update consumers in `web/src/` to match.
4. `cargo web build` for the final bundle.

## CI and release builds

The required CI gate runs `cargo web check` when the dashboard, its toolchain, the Rust crates that own the exported schemas (`zeroclaw-config`, `zeroclaw-gateway`, `zeroclaw-runtime`, and `zeroclaw-sop-graph`), the `xtask` generator, workspace manifests, or this workflow changes. This regenerates the ignored TypeScript client and typechecks the dashboard without producing a bundle. The Rust lint/build/test jobs still use a `web/dist/.gitkeep` placeholder so the gateway crate can compile without the bundle. Producing a release artifact that includes the dashboard is a separate step:

<div class="os-tabs-src">

#### sh

```sh
cargo web build
cargo build --release --features gateway
```

</div>

The gateway loads `web/dist/` from the filesystem at runtime via `static_files.rs`, so the Rust compile and the web build are decoupled. Ship the populated `web/dist/` alongside the binary for installs that should serve the dashboard.

## Nix builds

`nix/web.nix` exposes `packages.zeroclaw-web` (the `dist/` bundle at
`$out/share/zeroclaw-web/`) and `packages.zeroclaw-openapi-spec` (the
hermetic spec dump). The chain is split so each sandbox stays single-language:

1. `zeroclaw-openapi-spec` builds `cargo xtask web` and runs
   `web spec-dump --out $out`: pure Rust, no npm. `api-generated.ts` is
   deliberately *not* produced here.
2. `zeroclaw-web` (`buildNpmPackage`, Node 24 per `.nvmrc`) copies the dumped
   helpers into `src/lib/`, derives `api-generated.ts` from `openapi.json`
   with the vendored `openapi-typescript`, then runs the standard
   `npm run build`.

```sh
nix build .#zeroclaw-web
nix build .#zeroclaw-openapi-spec
```

The NixOS module (`nix/module.nix`, `webUiPackage` option) points
`gateway.web_dist_dir` at the bundle; see `nix/README.md`.

## Required tools

| Tool   | Install                                |
| ------ | -------------------------------------- |
| `npm`  | <https://nodejs.org/> or `nvm install && nvm use` from the repo root |
| `cargo`| <https://rustup.rs>                    |

The repo root `.nvmrc` pins the Node major version used by release web builds.
Use it for local dashboard work so `npm install`, `cargo web check`, and
manual release builds all run against the same Node line.

`cargo web` fails fast with an install hint if `npm` is missing.

## Supported browsers (minimum)

The dashboard targets evergreen browsers with support for both `color-mix()`
and `structuredClone()`.

- Chrome 111+
- Edge 111+
- Firefox 113+
- Safari 16.2+

## Browser voice controls

Paired browsers can open **Config → Voice and audio** to enable microphone and
speaker controls in chat. Preferences live in the gateway's `devices.db`, on the
existing paired-device record, and survive page reloads and gateway restarts.
`GET` and `PUT /api/devices/me/voice` resolve that record from the authenticated
paired bearer token; the own-device route cannot choose a different device ID. The device selector
uses `GET/PUT /api/devices/{id}/voice` under the same operator authorization as
listing/revoking devices. The selected record is distinct from the calling
browser: chat controls always load their own paired record. Revoking
or rotating a device removes its preferences along with the existing device
record. Cookie-only OIDC sessions and unpaired access do not have a paired-device
identity and cannot use the own-device preference API. Administrator sessions
may manage registered devices through the operator routes; non-administrators
are refused. The selector shows each record’s saved voice; choosing/sampling
available OS voices is performed in that device’s own browser.

Dictation defaults to on-device recognition. It requires HTTPS (or localhost),
a compatible browser, microphone permission, and an available language pack.
Downloading a pack requires an explicit action. An unavailable local recognizer
never silently falls back to a network service. Users can explicitly choose the
browser-service mode, which may send microphone audio to the browser provider.
The gateway does not receive microphone audio; final transcriptions enter the
message draft. By default, 15 seconds of silence after recognized speech sends
that draft automatically. The shared timeout is adjustable in Browser voice and
audio settings; set it to `0` for manual-only sending. Click the microphone again
to stop dictation and review the draft, or use Send to submit it manually.
Stopping dictation manually cancels its pending automatic send. Empty capture is
never submitted, and hiding or leaving the conversation cancels capture.

Reading uses a voice reported by the browser. The selector identifies local and
online voices; availability can change with the OS or browser. No particular
voice or gender is guaranteed. Click the speaker to read the latest assistant
reply or stop active reading. Hold it to enable continuous reading; click again
to disable it. Shift+Enter on the focused speaker toggles continuous reading.
This chat-scoped override does not alter the saved automatic-reading default.
Automatic reading excludes historical hydration, tool output, and reasoning.
Capture and playback stop when their chat closes, changes session, or becomes
hidden. Permissions Policy allows microphone and on-device speech recognition
only for the gateway origin; user microphone consent remains required.

History materialized into chat bubbles is marked at the hydration boundary, so
reconnection cannot make regenerated message IDs look like new live answers.
Returning to a browser tab/window reloads voice preferences from the gateway;
controls remain inactive until that read succeeds. Other tabs also receive an
invalidation-only BroadcastChannel notification and fetch their own record.
No browser preference store
or cross-tab copy becomes an additional source of truth. Device-registry schema
and I/O failures are returned to gateway startup with context instead of panicking.
