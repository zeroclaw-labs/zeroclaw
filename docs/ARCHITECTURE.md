# ClawSuite Architecture

## Stack

- **Framework:** TanStack Start + React
- **Styling:** Tailwind CSS + CSS custom properties (accent colors)
- **State:** Zustand (chat store) + React Query (history) + React state (UI)
- **Backend:** OpenClaw Gateway (WebSocket + HTTP on port 18789)
- **Browser:** Playwright + stealth plugin (headed Chromium)
- **Persistence:** Local filesystem (`data/tasks.json`, browser profile)

## Key Architecture Patterns

### Chat Message Flow

1. User sends message → POST `/api/send`
2. Gateway processes → SSE events via `/api/chat-events`
3. Single `done` event is authoritative end-of-response signal
4. 2-second failsafe timer as backup
5. History refetch 500ms after done for persistence sync

### Three-Layer Chat Sync

1. **Done event payload** — instant display from SSE
2. **History refetch** — 500ms after done event
3. **Periodic sync** — every 30s (skipped during streaming)

### Message Deduplication

- Content-based via `extractTextFromContent()` in gateway-chat-store
- Applied in: done handler, message handler, `mergeHistoryMessages`

### Two-Pass Display Filter

1. `use-chat-history.ts` — narration pass (filters tool-call-only messages)
2. `chat-screen.tsx` — `finalDisplayMessages` (system message filter)

- Messages with substantial text (>20 chars) are never hidden

### Accent Color System

- CSS custom properties: `--color-accent-50` through `--color-accent-900`
- All UI references use `accent-*` classes (no hardcoded colors)
- User-configurable in Settings → Appearance

## Known Limitations

- Gateway WebSocket does NOT support streaming deltas to operator connections
- Terminal uses `child_process` (no native PTY) — no resize, no interactive CLIs
- Typewriter animation disabled — needs state machine rewrite
- 4 competing state layers in chat (React state, Zustand, React Query, SSE)
