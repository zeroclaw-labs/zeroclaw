# WASM PoC: Skills page (#8132 Phase 1)

Bounded prototype: one representative page, no production replacement, no
default-build change, no framework commitment. This standalone Cargo workspace
is not referenced by the ZeroClaw workspace build. React remains authoritative.

Dioxus is pinned to `=0.7.10`, CSR, features `minimal, web`. Building uses Cargo,
`wasm32-unknown-unknown`, and wasm-bindgen-cli. No Node/npm or Tailwind build step.

## Build and serve locally

Prerequisites: a gateway already listening on `127.0.0.1:42617`, its paired
bearer token, Rust with the WASM target, a matching wasm-bindgen CLI, and Python 3.11+.
The helper does not start or configure the gateway.

```sh
cd web-wasm-poc
rustup target add wasm32-unknown-unknown
# One-time setup, skip if this exact CLI version is already installed.
# This version matches the checked-in Cargo.lock.
cargo install --locked wasm-bindgen-cli --version 0.2.129 --bin wasm-bindgen
./build.sh
python3 serve.py
```

Open `http://127.0.0.1:8080/` (use this exact host, not `localhost`). Paste your
existing paired bearer token into **Gateway token** and click **Connect**.
The password field masks the token. The page validates it against the gateway
before saving it under the React dashboard's `zeroclaw_token` storage key.
Blank or rejected tokens, unavailable gateways and storage failures stay on
the connection screen with an error. A saved token is revalidated on reload.
Use **Disconnect / change token** to remove it and return to the form.

This is existing-token entry, not the React pairing-code exchange. The helper
remains read-only: no pairing, admin-code generation or write endpoints are added.
Tokens are stored in localStorage, not encrypted. Use only a trusted local origin.

Leave `window.__ZC_API_ORIGIN__` empty. `serve.py` serves only the required asset
URLs and forwards GET `/api/*` requests to the fixed loopback gateway, including
the browser's Authorization header. No CORS changes, token injection, or gateway
static-route changes are needed. It rejects foreign Host and Origin headers,
does not log request URLs or headers, and does not support writes. Missing or
invalid credentials retain the gateway's rejection status. An unavailable
gateway produces HTTP 502. This is a local helper, not a production server.
Stop it with Ctrl+C. Use **Disconnect / change token** before stopping it
to remove the token from this browser origin.

Do not use `python3 -m http.server` alone: it cannot forward API requests.
Do not point `gateway.web_dist_dir` at `pkg/`: it has no index document, and the
PoC asset URLs do not match the gateway's supported static mounts.

### Reproduce the interaction checks

1. Select an agent and record its alias, card count, and one visible skill name.
2. Select another agent and record its alias and resulting skill list.
3. Search for a visible skill name, record the filtered count, then clear search.
4. Click Reload and confirm the selected agent's list is restored.
5. Inspect the search input's accessible name and title (`Search skills…`).
6. If a bundle-editable skill exists, expand it and check the fetched SKILL.md.
   Otherwise record expansion as unverified, not passed.

Record the revision (including uncommitted changes), browser, viewport,
observations, and screenshots. Helper tests alone are not live-gateway or browser
proof.

### Repair verification (October 31, 2026)

Tested `94834af0c` plus the uncommitted review repairs, against an isolated real
ZeroClaw v0.8.7 gateway with pairing enforced and synthetic bundle skills.
Chromium 131.0.6778.33 and Firefox 132.0, headless at 1400×900, both passed:

- Agent `alpha` showed `alpha-guide` and `alpha-notes` (two cards).
- Switching to `beta` showed only `beta-guide` (one card), then switching back
  restored both alpha cards.
- Searching `alpha-guide` reduced the count to one. A nonmatching query showed
  the empty state. Clearing search restored both cards.
- Expanding `alpha-guide` fetched and displayed `# Alpha Guide` from SKILL.md.
- Reload returned HTTP 200 and retained two cards for the selected agent.
- The search field had the expected accessible name and title, received focus
  through Tab navigation, and had a computed 2px focus outline.
- Missing and invalid tokens returned HTTP 401. No JavaScript page errors occurred.

Screenshots and machine-readable results were captured locally. They must be
attached to the review response before treating them as published evidence.

A subsequent skipped-skills check in both browsers used a harmless malformed
manifest in the isolated alpha workspace. The gateway reported one dropped
skill with `reason_kind = manifest_parse_error`. The banner displayed its name
and diagnostic, while both valid alpha skills remained usable. Search and
reload preserved the warning. Switching to beta removed it, and switching back
restored it. The invalid skill never appeared as a usable card. This check reused
the existing WASM bundle without rebuilding; frontend runtime sources were
unchanged. These checks are not a full accessibility audit or proof of parity
across the whole dashboard.

The StageX toolchain lacked a preinstalled WASM standard library, so validation
compiled that library from its bundled sources with Cargo's `build-std` path.
The ordinary rustup-based `./build.sh` path was syntax/preflight-tested, not run
end-to-end in that environment. The resulting real WASM bundle and documented
`serve.py` path were exercised together in both browsers.

### Token-entry verification (October 31, 2026)

A subsequent cached standalone WASM build added the connection screen. Chromium
and Firefox passed valid token entry, whitespace trimming, Enter submission,
blank/invalid rejection, saved-token reload, disconnect, stale-token recovery,
gateway failure and blocked-storage checks. Skills switching, search, expansion
and reload also passed behind the new gate. The helper's read-only policy is
unchanged. This is not a final merged-gateway build or full React pairing parity.

A separate human-demo gateway uses unchanged repository documents: the runtime
`zeroclaw-docs` asset and contributor skills `changelog-generation` and
`wit-breaking-change-check`. Demo agents group these as docs (one skill) and
development (two skills), not the operator's personal agents. Both browsers
matched all three expanded bodies to the repository and real gateway responses,
with no dropped skills. Synthetic alpha/beta and malformed manifests remain a
separate automated-test fixture. Desktop 1400×900 and narrow 390×844 checks in
both browsers passed keyboard connect, search focus, expand/collapse and no
page-level horizontal overflow. Screenshots were captured; this is a focused
interaction/layout smoke, not a full visual or accessibility audit.

### Focused checks and fast iteration

```sh
python3 -m unittest -v test_build test_serve
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
bash -n build.sh
```

HTTP tests use an isolated test upstream, not a real gateway. They cover asset
MIME types, bearer forwarding, missing/invalid tokens, unavailable upstream,
foreign origins/hosts, traversal, private source paths, and rejected writes.

`browser_auth_check.py` is an optional Playwright smoke, separate from Python
unit-test discovery. It requires Playwright 1.49 with Chromium and Firefox,
a built PoC and a running isolated synthetic gateway. Its fixture contract is
agents alpha/beta, bundle skills alpha-guide/alpha-notes/beta-guide, alpha-guide
body starting with `# Alpha Guide`, and the public test-only bearer value
`poc-isolated-test-token-not-a-real-credential`. Never configure that value on a
real deployment. Run `python3 browser_auth_check.py --output token-ui-results.json`
in that prepared environment. Successful login and skills requests reach the
real gateway, while gateway-failure/403/malformed-response and blocked-storage
cases are deliberately injected by the browser harness.

Keep registry and target caches between container runs, cap jobs using
`CARGO_BUILD_JOBS=4`, and batch Rust changes before a release build. `build.sh`
checks the CLI version before compiling and uses `--locked`. It expects the
local `target/` directory. Mount a cache there rather than overriding
`CARGO_TARGET_DIR`. The wasm-bindgen CLI must exactly match the version resolved
in Cargo.lock. No root-workspace build is needed for this standalone PoC.

## Preliminary evidence

Historical numbers below were reported on September 30, 2026. They have not
been remeasured for this repair and do not establish a migration advantage.

| Area | React/Vite baseline | Dioxus PoC | Limitation |
|---|---|---|---|
| Bundle size | Whole SPA: 3,676,214 B raw / 804,033 B gzip (63 chunks). Skills route: 7,592 B / 2,521 B gzip, plus shared index 368,214 B / 104,608 B gzip and lucide 611,527 B / 142,147 B gzip. | WASM 660,147 B raw / approximately 208,000 B gzip, JS shim 42,904 B raw. | One page versus full SPA is not a like-for-like comparison. JS gzip size not recorded. |
| Release build | Comparable clean/incremental times unmeasured. | Previously reported 34.8 s, registry-cached, opt-level=s, LTO, one codegen unit. | Not a clean dependency-download measurement. No build-speed advantage established. |
| Feature parity | Existing page remains authoritative. | Historical evidence plus the repair checks above. | Current checks cover switching, search, empty state, reload, bundle expansion and the dropped-skills banner, not the entire dashboard. |
| Accessibility | Existing page is the comparison baseline. | Search and select declare aria-label/title. | Accessible name, title, Tab focus and search outline verified in the repair smoke above. No full conformance claim. |
| Tests | Existing frontend tests. | Native missing-window regression, HTTP/build-preflight tests and optional token-entry browser smoke. | Browser smoke requires its isolated gateway fixture and Playwright. It is not wired into CI. |
| Dev workflow | Existing Vite workflow. | Cargo and wasm-bindgen. | HMR and contributor workflow not exercised. |
| Container build | Existing Node build stage. | Rust/WASM toolchain required. | Removing npm transfers dependencies and tooling, not supply-chain risk elimination. |

## Known gaps and evaluation boundary

- English-only inline catalogue. Dashboard localization integration is out of scope.
- Edit is a plain link, not a client-side routed editor. This helper supports
  only the Skills page, so navigating to the editor is not supported.
- Search, expansion/detail, reload, dropped-skills warning and switching are
  implemented against the existing gateway endpoints with the same
  `zeroclaw_token` localStorage key. Implemented does not mean browser-verified.
- CSS tokens are temporarily duplicated in `static/poc.css`.
- Direct cross-origin gateway access is not the documented serving path.
- Comparable clean/incremental timings, load time, runtime memory, editor
  integration, API generation, contributor workflow and supply-chain evaluation
  remain unmeasured. Keep the broader report in #8132. No production migration
  recommendation follows from these preliminary measurements.

## Rollback

Stop the helper and remove the token from this browser origin. No gateway
configuration or production frontend changes are required. Reverting the PoC
repair restores the previous standalone files without affecting core builds.
