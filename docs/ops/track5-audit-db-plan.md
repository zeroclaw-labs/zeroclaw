# Track 5: audit.db + Wire Audit Logger

Status: **Plan only — implement next session**

## AuditLogger Current State

- **Location**: `crates/daemonclaw-runtime/src/security/audit.rs` (1,279 lines, 35 tests)
- **Storage**: Append-only JSONL file at `{config_dir}/audit.log`
- **Status**: Fully built but **unwired** — never instantiated in daemon startup or channel orchestrator. No audit.log exists on disk.

### Event Shape

```
AuditEvent {
    timestamp, event_id, event_type,
    actor { channel, user_id, username },
    action { command, risk_level, approved, allowed },
    result { success, exit_code, duration_ms, error },
    security { policy_violation, rate_limit_remaining, sandbox_backend },
    sequence, prev_hash, entry_hash, signature?
}
```

Event types: `CommandExecution`, `FileAccess`, `ConfigChange`, `AuthSuccess`, `AuthFailure`, `PolicyViolation`, `SecurityEvent`

### Integrity Model

- Merkle-chained: `entry_hash = SHA-256(prev_hash || canonical_json)`
- Genesis seed: `0x00...00`
- Chain recovery reads last entry on startup
- `verify_chain()` validates entire chain (tested: tamper detection, sequence gap, insertion)
- Optional HMAC signing via `DAEMONCLAW_AUDIT_SIGNING_KEY` env var (32-byte hex)

## Why audit.db (separate from state.db)

1. **Trust boundary**: audit is a security mechanism; co-locating with mutable state gives the agent implicit ability to tamper with its own audit trail
2. **Write volume**: tool execution logging is high-frequency (every tool call), vs cold state's infrequent writes
3. **Integrity**: hash chain requires sequential ordered writes — WAL contention with other writers could reorder

## Schema

```sql
CREATE TABLE audit_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp   TEXT NOT NULL,
    event_id    TEXT NOT NULL UNIQUE,
    event_type  TEXT NOT NULL,
    actor       TEXT,          -- JSON
    action      TEXT,          -- JSON
    result      TEXT,          -- JSON
    security    TEXT NOT NULL,  -- JSON
    sequence    INTEGER NOT NULL,
    prev_hash   TEXT NOT NULL,
    entry_hash  TEXT NOT NULL,
    signature   TEXT
);
CREATE INDEX idx_audit_timestamp ON audit_events(timestamp);
CREATE INDEX idx_audit_sequence ON audit_events(sequence);
CREATE INDEX idx_audit_type ON audit_events(event_type);
```

## Wiring Points

The orchestrator calls `run_tool_call_loop` — this is where tool/command events occur. The audit logger needs to be:

1. Instantiated during daemon startup (alongside the existing hooks runner)
2. Added to `ChannelRuntimeContext` as a new field
3. Threaded through `run_tool_call_loop` (which already accepts an `Observer`)

`run_tool_call_loop` in `agent/loop_.rs` has access to: tool name, args, result, duration, channel, and approval state — exactly the `CommandExecutionLog` shape the AuditLogger already expects.

## Implementation Sequence

1. Replace file I/O in `AuditLogger` with `rusqlite` — `INSERT INTO audit_events`
2. Chain recovery: `SELECT prev_hash, sequence FROM audit_events ORDER BY id DESC LIMIT 1`
3. `verify_chain()`: `SELECT * FROM audit_events ORDER BY sequence ASC`
4. Retention by row count or date (`DELETE` older than N days), replacing file rotation
5. Add `audit_logger: Option<Arc<AuditLogger>>` to `ChannelRuntimeContext`
6. Instantiate in daemon startup (alongside hooks)
7. Wire into `run_tool_call_loop` — log `CommandExecution` events for every tool execution
8. Wire into approval/policy path — log `PolicyViolation` and `AuthFailure` events
9. All 35 existing tests must pass against SQLite backend + new wiring tests

## JSONL Disposition

| File | Decision | Justification |
|------|----------|---------------|
| `events.jsonl` | Keep as rolling file | High-frequency observer events (every LLM call, tool dispatch). Trait-based observer interface (`dyn Observer`). |
| `runtime-trace.jsonl` | Keep as rolling file | Per-turn diagnostic trace (token counts, timing). Fire-and-forget write path. |

Both are hot observability streams where file-append is the correct pattern. They are not cold state. The audit.db is the structured, queryable, integrity-chained counterpart for security-relevant events specifically.

## Blast Radius

| File | Change | Lines |
|------|--------|-------|
| `security/audit.rs` | Storage rewrite (file → SQLite) | ~150 |
| `daemon/mod.rs` | Instantiate logger | ~10 |
| `orchestrator/mod.rs` | Add field + thread through | ~20 |
| `agent/loop_.rs` | Emit audit events from tool execution | ~30 |
| Tests | Adapt existing 35 + new wiring tests | ~200 |

**Total**: ~400 lines across 4 files. Low risk — the audit logger is currently unwired.
