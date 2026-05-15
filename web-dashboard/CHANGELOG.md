# web-dashboard changelog

Fork-local dashboard for `Stealinglight/zeroclaw`. Versioning is
independent of upstream ZeroClaw — this changelog tracks the
multi-session-dashboard milestones in `.omc/plans/multi-session-dashboard.md`.

## M3 — Streaming chat + slot CRUD UI + theme switcher

The dashboard now ships a multi-session chat surface:

- Slot CRUD: `+ New`, hover-revealed rename / duplicate / delete on
  every row, inline rename input
- URL-driven active slot via `/chat/:slotId` — reload preserves
  selection
- Streaming chat backed by `POST /api/slots/:id/messages` (SSE), with
  optimistic message append, react-markdown rendering, and a Stop
  button that aborts the local reader and POSTs `/stop`
- Three themes (default / monochrome / contrast) × two modes (light /
  dark), persisted to `localStorage[zeroclaw.control.settings.v1]`
  with a FOUC-avoidance script that applies the saved values before
  React hydrates
- Service worker with network-first HTML / cache-first hashed assets
- Playwright smoke covering bootstrap load, slot creation, theme
  persistence, and the rename-Cancel-button race regression

### Attribution

UX patterns and module organization adapted from
[OpenClaw](https://github.com/openclaw/openclaw) (MIT, copyright Peter
Steinberger). The slot-sidebar information architecture, theme token
system (three themes × two modes), FOUC-avoidance script structure,
and `/api/control-ui/config` bootstrap pattern all originate from
OpenClaw's `ui/` workspace. Implementation rewritten in React/TS;
brand naming, color palettes, and per-slot agent model are
ZeroClaw's own.

See [`LICENSES/OpenClaw-LICENSE`](./LICENSES/OpenClaw-LICENSE) for
verbatim MIT license text.
