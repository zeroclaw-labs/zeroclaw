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

## Provisioning OpenBao secrets from the dashboard

With a server-side OpenBao identity configured, the operator selects **OpenBao**,
enters the secret value, and selects **Add credential**. The editable reference
is suggested from the mount, prefix, alias and field. The gateway handles vault
authentication, so the operator can complete the task without issuing or
copying a vault token. **Link existing secret** saves the reference without
writing a value to OpenBao.

Set `BAO_ADDR` to the operator-configured HTTPS origin of an OpenBao server
and `ZEROCLAW_OPENBAO_WRITE_PREFIX` to an allowed KV v2 mount and prefix,
for example `secret/agents`. OpenBao then offers **Add** to create a
record and **Link existing secret** to register an existing reference.
The editable reference defaults to `<prefix>/<alias>/<field>`.
Existing read-side OpenBao identity and ACLs still govern later use.
Set `BAO_CACERT` to a PEM CA bundle if the vault uses a private CA.

For managed writes, set `ZEROCLAW_OPENBAO_WRITE_TOKEN_FILE` to the absolute
path of a dedicated, short-lived write token managed by OpenBao Agent or an
equivalent deployment service. If omitted, the gateway can use the configured
OpenBao Agent token file from BAO_TOKEN_PATH. The dashboard asks the authenticated
operator for the value only. The gateway reads the token file on each
write, so lease renewal and replacement do not require a gateway restart.
Protect the file and the writer identity from agent tools and untrusted
processes. An explicitly shared OpenBao identity needs scoped permissions for both reads and writes. Use a separate writer identity where the gateway has its own execution boundary. Without a server-side writer, the WebUI can still link an existing secret without issuing an OpenBao write. Give the writer token `create` for new records or `patch` plus `read`
on the metadata endpoint for rotations, restricted to the configured
prefix. The gateway sends the value and token over verified HTTPS and
persists only the encrypted reference. New records use KV v2 CAS zero.
Replace with a value patches the field on the same reference against
its current version. A blank replacement preserves the external value.
Delete removes the catalog entry. Use the vault for remote destruction.

If OpenBao accepts a new write but persisting the local reference fails,
use **Link existing secret** with the same reference to recover. A
retry of **Add** cannot overwrite it. A gateway timeout may also leave
the result uncertain. Verify in OpenBao before retrying.

### Other external vaults

Each writer is opt-in. Set the matching prefix on the gateway host. The
browser suggests a reference inside that scope, and editing the reference
remains subject to the scope and the provider ACL. The operator enters
write authorization per operation. The existing CLI-based read flows
retain their own credentials.

| Provider | Gateway setting and example | Operator write authority |
| --- | --- | --- |
| 1Password | `ZEROCLAW_1PASSWORD_WRITE_PREFIX=op://<vault-id>` and `ZEROCLAW_1PASSWORD_CONNECT_URL=https://connect.example.com` | A Connect write token scoped to the vault. Set `ZEROCLAW_1PASSWORD_CONNECT_CA` to a PEM bundle if the origin uses a private CA. |
| Azure Key Vault | `ZEROCLAW_AZURE_WRITE_PREFIX=vault-name/jarvis-` | A bearer token for Azure Key Vault with metadata-list and secret-set permissions in the configured scope. Public Azure endpoint. |
| AWS Secrets Manager | `ZEROCLAW_AWS_WRITE_PREFIX=us-east-1/jarvis` | Temporary AWS STS access key, secret access key and session token with CreateSecret and PutSecretValue permissions for the scope. Commercial AWS endpoints. |
| Google Secret Manager | `ZEROCLAW_GOOGLE_WRITE_PREFIX=project-id/jarvis-` | An OAuth access token with the cloud-platform scope and permissions to create secrets and add versions. Global secrets, using `latest` references. |

The 1Password writer checks for an existing item title and stores the
item and field IDs returned by Connect. Rotation patches the field by
ID. Azure checks version metadata before setting a value. Azure Set
Secret and 1Password item creation lack a conditional create by name.
Use a dedicated vault or name prefix with one writer to avoid races with
external writers. AWS and Google reject creation of an existing name
at the provider.

Google creates the resource and its first version in separate requests.
If the version write fails, verify the resource in Google Cloud, then
link the existing identifier and use **Replace** to add the version.
Retried adds cannot replace an existing AWS or Google resource.

## Browser voice controls

Paired browsers can open **Config → Other → Browser Voice and Audio** to enable microphone and
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

Within one dashboard page, split chat panes and the settings preview share
browser-audio ownership. Manual microphone, speaker and preview clicks show a busy message while
another pane owns audio; click again after it ends. Only continuous narration
keeps replies pending and retries after capture or playback ends. Capture end
or cancellation releases that ownership. Other browser tabs and applications
are outside this page-local coordination.

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
