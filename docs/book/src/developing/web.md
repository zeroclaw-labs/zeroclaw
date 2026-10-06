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

## Workspace navigation

Home (`/`) summarizes ready agents, active work, recorded daily model spend,
and system health, followed by recent sessions and SOP activity. The main
workspaces have short descriptions, and an expandable feature directory keeps
supporting tools discoverable. These read-only views use existing gateway and
runtime APIs; unavailable data is shown as unknown rather than zero.

**Agent**, **Code**, **S.O.P**, and **Admin** form a centered switcher in the persistent
header, including on Home. The page scrolls below it. There is no global
navigation rail. Search, appearance, language, and sign-out remain available
from the header. **Operator Dark/Light** use the same default palette and cyan
accent behavior as `master`. They apply when no appearance preference is saved.
The softer graphite/teal and ivory/green palettes remain available as
**Calm Dark/Light** under **Appearance > Themes**. Switching appearance modes
keeps these dark/light pairs together. Calm uses contrasting primary-button text;
other palettes retain the button text and accent behavior from `master`.

Admin (`/admin`) collects dashboards for system health, spending, channels,
memory, sessions, workflow runs, logs, and diagnostics, plus agent/resource
management and advanced tools. Its searchable directory uses the shared
navigation catalogue. Quick settings resolve available sections and labels from
the gateway's configuration catalogue and open the existing floating editor.
Searching the directory includes all available settings sections; Cmd/Ctrl+K on
Admin searches configuration fields. Admin remains selected on management and
operational routes, while workflow run details belong to S.O.P. This is a
navigation grouping, not a new permission role or authentication boundary.

Agent resumes the browser's last active agent conversation. Its sidebar switches
between configured agents while open conversations stay connected. The active
agent's settings button opens its existing schema-driven editor in a modal.
Code restores the last selected code session, falling back to the newest eligible
session when that selection no longer exists. Its sidebar lists previous code
sessions, and a collapsible workspace file preview sits beneath the conversation.

**Cmd+K / Ctrl+K** opens search scoped to the current feature. The persistent
header shows a **Search settings** button with both shortcuts at every screen
width; on narrower screens it occupies a second row below the workspace switcher.
Agent and Code search include the active agent's schema-declared provider and
profile references;
"Search everything" expands to all features, history, and settings. Selecting a
field opens a focused editor without changing the workspace URL or connection.
"All related settings" opens the owning form. Edits use the existing config draft
store, gateway validation, save operations, and reload indicators. Shared profile
and provider forms identify their owner because changes affect all agents using
them. Search never indexes secret values.

The SOP workspace centers manual authoring on a full-height node canvas, with
zoom, fit, drag, and wiring controls. The searchable library on the left can be
hidden. One toggleable panel on the right contains **Runs**, **Node**, and
**Agent** tabs. Selecting a node opens its inspector; SOP settings and triggers
use the same panel. The palette creates agent tasks, planned tool actions,
conditional branches, and approval checkpoints. Deterministic workflows also
offer a bounded wait capability. These are presets for existing runtime step
fields, not new execution kinds.

New SOPs open the helper with a **Start blank** option. Each sent helper message
includes the current draft as inspectable context. A complete JSON proposal can
be validated by the gateway and applied to the draft; saving remains explicit.
The helper uses an ordinary configured Code agent and its existing permissions.
Opening it alone does not call a model. Editors and opened helpers remain
mounted while switching SOPs or workspaces, including when a new SOP is saved.
Pending assistant approvals link back to the owning SOP.

Fields, graph edits, applied proposals, and **Advanced / Source** share one
local draft. Per-SOP session-storage recovery keeps unsaved edits in the browser
tab; it is not a shared definition store. Source changes must be applied or
reset before other edits or saving. Saving reloads the backend-normalized
step numbers and bindings. **Reset to saved** restores the persisted definition.

**Run** displays the current saved graph with the existing live overlay. The
Runs tab switches between active runs across SOPs and recent runs for the
selected SOP, and can start a saved workflow with a manual trigger. The Node
tab inspects captured tool arguments, results, and errors. Existing approval,
deny, and safe-stop controls use the gateway's decision APIs; an accepted stop
remains visible while the current step drains. Run selection preserves the
editor draft and rejects late updates from a previously selected run.

The current backend does not bind a run to an immutable definition revision,
so the canvas labels its use of the current saved definition. Deferred work
beyond v1 has no milestone: [persistent SOP managers and conversations][sop-manager],
[run-definition revisions][sop-revisions], [helper authority and live adaptation][sop-authority],
[reviewable gate artifacts and supported decisions][sop-approval],
[shared named groups][sop-groups], and [child-SOP composition][sop-nested].

[sop-manager]: https://github.com/zeroclaw-labs/zeroclaw/issues/11546
[sop-revisions]: https://github.com/zeroclaw-labs/zeroclaw/issues/11547
[sop-authority]: https://github.com/zeroclaw-labs/zeroclaw/issues/11548
[sop-approval]: https://github.com/zeroclaw-labs/zeroclaw/issues/11549
[sop-groups]: https://github.com/zeroclaw-labs/zeroclaw/issues/11550
[sop-nested]: https://github.com/zeroclaw-labs/zeroclaw/issues/11551

The Code workspace uses the daemon's zerocode RPC dispatcher through the paired
`/ws/code` bridge. It supports session history, streaming output, approvals,
choice questions, cancellation, and read-only file previews. Agents edit files
through their runtime tools and existing permission checks. **Cmd+Enter /
Ctrl+Enter** sends a prompt. Refreshing or closing the browser still disconnects
clients; active code turns and unsaved SOP edits request browser confirmation.

Availability comes from `GET /api/workspace`. Empty or unavailable workspaces
provide setup or reconnect paths. Operational metrics remain at `/system`, and
existing `/?tab=...` bookmarks still work. The Tauri wrapper loads this same web
application; this change adds no native commands or filesystem permissions.

### Browser verification

`web/scripts/workspace-smoke.cjs` exercises the real web renderer with synthetic
HTTP and RPC fixtures. It covers the workspace entries, modal settings search
and focus, approvals across page changes, stale turn events, resumed task
recovery, unavailable capabilities, mobile layout, and light mode. It writes
screenshots to `/tmp/zeroclaw-workspace-evidence` by default.

Start Vite on port 5178, then run the script with an existing Playwright module
and Chrome installation. `PLAYWRIGHT_MODULE` can be an absolute path to that
module; when omitted, Node resolves `playwright` normally. `WEB_SMOKE_URL` and
`WEB_SMOKE_OUTPUT` override the server URL and screenshot directory.

```sh
node web/scripts/workspace-smoke.cjs
```

`web/scripts/sop-workspace-smoke.cjs` exercises SOP draft recovery, explicit save
and reset, gateway validation failures, zoomed node movement and undo, quick run
switching, approvals, accepted cancellation during a refresh outage, helper
proposals, and helper-session retention on first save. It checks keyboard tabs
and 390px/320px layouts with synthetic HTTP/RPC fixtures and writes screenshots
to `/tmp/zeroclaw-sop-evidence`. Start Vite on port 5179 and use the same environment
overrides as the workspace script:

```sh
node web/scripts/sop-workspace-smoke.cjs
```

These fixtures verify browser behavior without model calls. The gateway's
`code::tests` separately exercise real authentication, HTTP/WebSocket upgrade,
local IPC, the runtime dispatcher, and session history in an isolated install.

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
