# zeroclaw-infra

Channel infrastructure: session backends, debouncing, and stall watchdog.

These are cross-cutting utilities used by multiple channel implementations in
the ZeroClaw workspace.

## Modules

| Module | Purpose |
|--------|---------|
| `session_backend` | Trait definition for session persistence backends |
| `session_sqlite` | SQLite-backed session store (default) |
| `session_store` | JSONL-backed session store (legacy) |
| `session_queue` | Ordered message queue for session delivery |
| `acp_session_store` | ACP (Agent Communication Protocol) session storage |
| `debounce` | Message deduplication and rate-limiting |
| `stall_watchdog` | Detects and recovers stalled agent sessions |

## Session Backends

The crate provides two session-persistence backends via [`make_session_backend`]:

- **`sqlite`** (default) — opens `{workspace}/sessions/sessions.db`. On first
  open, automatically imports any pre-existing JSONL session files and renames
  them to `*.jsonl.migrated` for rollback safety.
- **`jsonl`** (legacy) — opens `{workspace}/sessions/*.jsonl`, one file per
  session key.

Unknown backend values fall back to SQLite with a warning so a typo in config
never silently disables persistence.

## Dependencies

- `zeroclaw-api` — trait definitions
- `zeroclaw-log` — structured logging via `record!`
- `zeroclaw-spawn` — attribution-propagating task spawning
- `rusqlite` (bundled) — SQLite session backend

## Stability

Beta — breaking changes permitted in MINOR with changelog notes.
