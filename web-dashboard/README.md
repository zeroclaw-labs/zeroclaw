# ZeroClaw Web Dashboard

Multi-session chat dashboard for ZeroClaw — Vite + React 19 + Tailwind 4 + React Query.

This is a **fork-local feature** living on `Stealinglight/zeroclaw`. Upstream
ZeroClaw's v0.8.0 dashboard (RFC #5890) takes a different shape. See
`.omc/plans/multi-session-dashboard.md` for the full plan.

## Quick start

```bash
# 1. Run the gateway (in a separate shell)
cargo run -p zeroclaw-gateway

# 2. Run the dashboard dev server
cd web-dashboard
npm install
npm run dev
# → http://localhost:5173/  (Vite proxies /api and /ws to the gateway)
```

## Scripts

| Command | What |
|---|---|
| `npm run dev` | Vite dev server with HMR |
| `npm run build` | Production build → `dist/` |
| `npm run typecheck` | `tsc -b --noEmit` |
| `npm run preview` | Serve `dist/` locally |
| `npm test` | Playwright smoke (route-mocked, no real gateway) |
| `npm run test:install` | One-time `playwright install --with-deps chromium` |

## Module layout

The structure mirrors OpenClaw's `ui/src/ui/` module taxonomy
(see plan §12 translation table):

```
src/
├── App.tsx                              # router + bootstrap provider
├── main.tsx                             # QueryClient + service worker registration
├── app/
│   └── ControlUiBootstrapProvider.tsx   # /api/control-ui/config snapshot context
├── chat/
│   ├── ChatPage.tsx                     # 3-pane layout: sidebar + chat
│   ├── ChatView.tsx                     # streaming chat UI (markdown + composer)
│   ├── SlotSidebar.tsx                  # slot list + CRUD UI
│   ├── slotMutations.ts                 # React Query hooks for /api/slots
│   ├── slotsQuery.ts                    # shared ["slots"] observer
│   └── useSlotStream.ts                 # SSE consumer for /api/slots/:id/messages
├── theme/
│   ├── useTheme.ts                      # localStorage-backed theme + mode
│   └── ThemeSwitcher.tsx                # 3-theme × 2-mode picker popover
└── lib/
    ├── apiFetch.ts                      # auth-aware fetch wrapper
    └── auth.ts                          # token storage
```

## Credits

UX patterns and module organization adapted from
[OpenClaw](https://github.com/openclaw/openclaw) (MIT, copyright Peter
Steinberger). The dashboard's three-theme × two-mode token system, FOUC-
avoidance approach, control-ui bootstrap pattern, and slot-sidebar
information architecture all originate from OpenClaw's `ui/` workspace.
ZeroClaw owns its own implementations (React/TS rather than Lit/TS),
brand naming, color palettes, and per-slot agent model.

See [`LICENSES/OpenClaw-LICENSE`](./LICENSES/OpenClaw-LICENSE) for the
verbatim MIT text.
