# WASM PoC — Skills page (RFC #8132 Phase 1)

Bounded prototype per the maintainer contract on zeroclaw-labs/zeroclaw#8132
(Aug 24): one representative page, no production replacement, no default-build
change, no framework commitment. This directory is a standalone cargo workspace
and is not referenced by the zeroclaw workspace build.

- Page: `web/src/pages/Skills.tsx` (241 lines) + `web/src/components/SkillCard.tsx` (177 lines)
- Framework: Dioxus `=0.7.10` (stable, Jul 30 2026), CSR, features `minimal, web`
- Fallback under the same harness: Leptos 0.8.21 (documented, not built yet)
- Toolchain: cargo + `wasm32-unknown-unknown` + wasm-bindgen-cli. No Node, no
  npm, no Tailwind build step.

## Build

```
rustup target add wasm32-unknown-unknown
./build.sh
python3 -m http.server 8080   # from web-wasm-poc root
```

Set `window.__ZC_API_ORIGIN__` in `index.html` to the gateway origin (e.g.
`http://127.0.0.1:42617`) or leave empty when serving same-origin with the
gateway pointed at `pkg/` via `gateway.web_dist_dir` (operator opt-in; the
default `web_dist_dir` is untouched).

## Evidence contract (measured 2026-09-30, same host)

| Row | React/Vite baseline | Dioxus PoC | Notes |
|---|---|---|---|
| Bundle size | whole SPA: 3,676,214 B raw / 804,033 B gzip (63 chunks); Skills route chunk 7,592 B / 2,521 B gz on top of shared runtime (index 368,214 B / 104,608 B gz, lucide 611,527 B / 142,147 B gz) | wasm 660,147 B raw / ~208,000 B gz + JS shim 42,904 B | PoC is ONE page incl. framework; React numbers include all pages. A migration amortizes dioxus framework cost across pages; the honest per-page React cost = shared index+lucide chunks + route chunk |
| Build time (release) | npm ci + vite build ≈ several min (node:24-slim container) | 34.8 s cold release profile (`opt-level=s`, LTO, cg=1) | wasm cold build in rust:1.98-slim container, registry-cached |
| Dev loop | vite HMR | not exercised in PoC | `dioxus serve` not part of this build path |
| Feature parity | full page | see gaps below | search/expand/reload/dropped-banner/agent-switch verified against gateway endpoints |
| Accessibility | aria-labels, focus states | parity-level: search input and agent select keep aria-label/title; expand buttons focusable with visible focus ring | TSX lacks aria-expanded on expand button; PoC mirrors (parity target) |
| Test surface | vitest + browser tests | none shipped | PoC scope; migration needs a wasm test story |
| Docker impact | node build stage | cargo-only stage | no node/npm in the PoC chain; rust:1.98-slim + wasm-bindgen-cli |

## Honest gaps (known, deliberate)

- i18n: English strings only, inline dict. The dashboard's i18n mechanism is
  out of scope for one page.
- The Edit link renders as a plain anchor (no client router in the PoC).
- Search, expand/detail, reload, dropped-skills warning, agent switching are
  implemented against the real gateway endpoints (`/api/config/agent-options`,
  `/api/agents/{alias}/skills`, `/api/skills/bundles/{bundle}/skills/{name}`)
  with the same bearer-token auth (`zeroclaw_token` localStorage key).
- CSS tokens are copied from `web/src/index.css` into `static/poc.css` —
  duplication is intentional and temporary; a migration would import the real
  token file.
- Cross-origin: the gateway may reject CORS from a static server. Same-origin
  serving via `gateway.web_dist_dir` avoids this entirely.

## Versions pinned

- dioxus `=0.7.10` (exact)
- wasm-bindgen CLI must match the `wasm-bindgen` version resolved in
  `Cargo.lock` (build.sh verifies and prints the mismatch)
