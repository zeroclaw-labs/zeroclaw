# Runtime state and persistence

ZeroClaw has one install root, but not one monolithic "workspace database".
Different state surfaces have different owners, reload behavior, and durability.
Use this map when a change adds state, moves state, caches config, touches reload,
or changes session/memory/log/cost behavior.

The single-source-of-truth rule still applies: if a fact already lives in one
surface, do not copy it into another stored field. Store new state only when this
table identifies the owning surface, or resolve it from the canonical owner at
use time.

## Install layout

For a normal install, `<install>` is the resolved config directory
(`~/.zeroclaw/` by default, Homebrew and explicit `--config-dir` installs can
move it). The current layout is:

```text
<install>/
├── config.toml                 # canonical user config
├── .secret_key                 # key for encrypted secrets
├── data/                       # instance-wide runtime data
│   ├── sessions/
│   │   ├── sessions.db         # default chat/session backend
│   │   └── acp-sessions.db     # ACP protocol sessions
│   ├── cron/jobs.db            # scheduled job state
│   ├── sop/runs.db             # legacy run store; removed with the run side, left in place
│   ├── control_plane.db        # legacy task ledger; removed with the control plane, left in place
│   ├── state/
│   │   ├── runtime-trace.jsonl # persisted logs
│   │   └── costs.jsonl         # cost ledger
│   ├── devices.db              # paired-device metadata
│   └── memory/                 # shared instance memory stores
├── shared/                     # shared resources, such as skill bundles
└── agents/<alias>/workspace/   # per-agent filesystem sandbox and identity
```

The legacy `<install>/workspace/` name is still accepted during migration, but
new runtime state should be described in terms of `<install>/data/`,
`<install>/shared/`, and per-agent workspaces.

## State map

| Surface | Canonical source | Durable path | In-memory owner | Reload / concurrency boundary | Notes |
| --- | --- | --- | --- | --- | --- |
| Config values | `zeroclaw-config::Config` loaded from `config.toml` | `<install>/config.toml` | daemon `Arc<RwLock<Config>>` plus per-subsystem resolved views | `/admin/reload` re-reads config and re-instantiates daemon subsystems; direct config writes use schema validation and dirty-path checks. Gateway HTTP config mutations additionally serialize their whole read-mutate-swap section on `AppState::config_write_lock` (tokio mutex first, `parking_lot` `RwLock` second, and never a `parking_lot` guard held across an `.await` or `lock().await`) | Do not cache config-derived facts in long-lived structs unless the cache is explicitly rebuilt on reload. |
| Encrypted secrets | Config secret fields plus `.secret_key` | `<install>/config.toml`, `<install>/.secret_key` | secret-store helpers in `zeroclaw-config` | Reload observes changed config; losing `.secret_key` makes encrypted config secrets unrecoverable | Never copy decrypted values into logs, docs, PR bodies, or runtime metadata. |
| Agent filesystem identity | Per-agent workspace files | `<install>/agents/<alias>/workspace/` | effective `SecurityPolicy` and agent prompt construction | Created lazily when the agent starts; workspace access is evaluated from config | This is the filesystem sandbox, not the config source of truth for providers/channels/tools. |
| Shared skill bundles | Configured skill bundle entries and resolved bundle dirs | `<install>/shared/skills/<bundle>/` by default | skill loading / prompt enrichment | Reload and new agent starts observe config and filesystem changes | Bundle aliases and directory resolution come from config; the files are the bundle content. |
| Conversation memory | `zeroclaw-memory` backend selected per agent | SQLite/Lucid/Markdown backend locations; SQLite shared store lives under `data/memory/` | `Arc<dyn Memory>` wrapped in agent-scoping adapters | Backend choice is locked once an agent has written data; same-backend cross-agent recall is opt-in | Memory rows are agent-scoped. Do not replace memory ownership with copied prompt/session caches. |
| Chat and channel sessions | `[channels].session_backend` plus `SessionBackend` | Default `data/sessions/sessions.db`; legacy/explicit JSONL uses `data/sessions/*.jsonl` | `zeroclaw-infra` session backend shared by channels and gateway | SQLite backend uses WAL; `SessionActorQueue` serializes active turns per session | Chat/Code sessions use this unified backend. ACP protocol sessions use a separate store. |
| ACP sessions | ACP protocol session store | `data/sessions/acp-sessions.db` | `AcpSessionStore` opened by `zeroclaw acp` and gateway agent-state handlers | WAL-backed SQLite store, separate from chat sessions | ACP `session/load` and `session/resume` operate on this protocol store, not the chat session backend. |
| Cron jobs | Declarative config membership plus cron SQLite store | `data/cron/jobs.db` | `zeroclaw-runtime::cron` scheduler/store | Read paths do not create `jobs.db`; scheduler owns due/lock state | Declarative jobs are reconciled from config, while run metadata and locks live in the cron DB. |
| SOP runs | Removed with the run side | `data/sop/runs.db` only as a leftover from an older install | none | The legacy `SopEngine`/`SopRunStore` pair was demolished; leftover stores are left in place and reported by a boot-time warning | Run truth lives Tachi-side as ProcedureRuns through the procedure_v1 seam. |
| Background task supervision | Removed with the control plane | `data/control_plane.db` only as a leftover from an older install | none | The coordinator child host and the task ledger were deleted; the daemon reports a leftover file once per boot and leaves it in place | Rows came from the detached arms of the retired `spawn_subagent`/`delegate` tools; after the spawn wall no writer minted new child rows. Durable task/attempt truth lives in Tachi through the task-intent bridge. |
| Background child results (the retired background `delegate` was the first consumer) | Removed with the control plane | `data/control_plane.db` only as a leftover from an older install | none | Same disposition as background task supervision | The workspace `delegate_results/` file store was retired with the delegate tool; leftover JSON files are ignored. |
| Runtime logs | `zeroclaw-log` event schema and subscriber layer | `data/state/runtime-trace.jsonl` when persistence is enabled | broadcast hook, JSONL writer, `/api/logs` reader, `Observer` bridge | Rolling/full/none persistence is config-controlled; dashboard SSE receives events even when JSONL is disabled | Logs are evidence and observability, not the source of user config or session state. |
| Cost ledger | `zeroclaw-cost::CostTracker` plus rate config | `data/state/costs.jsonl` | process-global `CostTracker` | Reload hot-swaps `CostConfig`; the tracker is constructed on demand if cost tracking becomes enabled | Shared results live in `zeroclaw-api::cost`. Existing records keep their recorded price; rate edits affect future requests after reload. |
| Gateway pairing tokens | Runtime `PairingGuard` from `gateway.paired_tokens` | token hashes in config | runtime pairing guard | Reload reconstructs the guard from config | Valid bearer tokens are config state, not `devices.db` rows. |
| Paired device metadata | Device registry rows keyed by token hash | `data/devices.db` | `DeviceRegistry` cache plus SQLite | Registry reconciles metadata against the canonical paired-token set | This DB makes paired devices visible/manageable; it does not invent valid tokens. |
| Health and component status | running subsystems report component state | none | gateway health/status state | Process-local; reset/rebuilt on daemon restart or reload | `/health`, `/api/health`, and `/api/status` are current observations, not durable configuration. |
| Queues, debouncers, watchdogs | `zeroclaw-infra` process utilities | none unless a caller stores results elsewhere | in-memory queues/debouncers/watchdogs | Process-local; used to serialize, coalesce, or detect stalls | Treat these as coordination state. Persist only the domain data they protect, not the queue itself. |

## Reload and restart

`POST /admin/reload` sends an in-process reload signal to the daemon. The outer
daemon loop re-reads config from disk and re-runs the daemon, creating fresh
gateway, channel, heartbeat, scheduler, MQTT, session, memory, and cost wiring
from the new config. The PID stays the same, but listeners briefly rebind.

A full process restart also rotates process-local state such as health
snapshots, actor queues, and any ephemeral tool-receipt key.
Durable stores survive restart according to the table above.

### Gateway ownership and interrupted turns

Every production gateway takes an exclusive operating-system lock on
`data/sessions/gateway.lock` before opening its runtime. A second gateway using
the same data directory is refused even on another port. The conversation hub
retains the lock for detached turns: closing the listener or last client does
not make still-executing work abandoned. Keep the empty lock file in place;
deleting it can allow two independent locks.

After acquiring ownership, startup settles `gw_` sessions left `running` in the
existing SQLite store as `error`, preserving the turn ID, history and request
receipts. This reports an interrupted turn, not that its tools had no effects.
No tool or accepted request is automatically replayed. Non-gateway sessions
and terminal states are untouched. Recovery neither creates a SQLite store nor
migrates an older schema; a store error refuses startup. JSONL has no durable
running-state guarantee.

This cooperative lock cannot fence older binaries or external database writers.
Stop old gateways before upgrading; never run an older binary against the same
data directory concurrently. Reload can acquire a new hub only after all owners
of the preceding hub release it. Outstanding work blocks replacement rather than
being misreported as abandoned.

Shutdown retires chat sockets, including incomplete handshakes, and bridge
control sockets. They cannot keep admitting work or hold an otherwise idle hub
through reload. Bridge cleanup leaves unacknowledged delivery attempts unknown;
it does not put them back into the automatic-send queue.

### Client detach and approvals

Ordinary tool approvals belong to the running conversation, not its sockets.
Within the same gateway lifetime, closing the last client leaves the prompt
pending only until its original deadline. Reconnect projects that same request
ID and its remaining time; it neither extends the deadline nor grants authority.
The existing operation/identity checks still govern responses, and only the
first timely decision can resolve the request. Timeout, turn cancellation,
session deletion or losing the owning process revokes the prompt. Approval
arguments and pending decisions are not persisted for execution after restart.
Correlated questions retain their separate offline behavior.

Desktop clients reuse the existing service and authenticated gateway. Closing
the app only detaches it; reconnect must not invoke `/admin/shutdown`, cancel a
turn or launch another backend. Reconnect negotiates the turn-control epoch.
A changed epoch requires reading durable history and session state, never
resending an accepted effectful request. Service installation, pairing and
operator migration remain explicit setup steps; health alone proves neither
identity nor operator authority.

## Backup and restore

For a normal single-instance install, back up the whole `<install>` directory.
At minimum, include:

- `config.toml`
- `.secret_key` if encrypted secrets are used
- `data/memory/`
- `data/sessions/`
- `data/cron/jobs.db` if cron jobs are configured through runtime surfaces
- `data/sop/runs.db` - legacy: the SOP run store was removed with the run side; existing files are left in place and reported by a boot-time warning
- `data/control_plane.db` - legacy: the task control plane was removed with the control-plane migration wall; existing files are left in place and reported by a boot-time warning
- `data/state/costs.jsonl` if cost history matters
- `data/state/runtime-trace.jsonl` if logs are needed for incident review
- `data/devices.db` for paired-device metadata

Do not run two daemons against the same install root. Several stores use SQLite
with a single-writer model, and the process-local caches assume one daemon owns
the instance.

## Source pointers

- Config, install-root, and data-dir resolution: `crates/zeroclaw-config/src/schema.rs`
- Session backends: `crates/zeroclaw-infra/src/session_sqlite.rs`, `crates/zeroclaw-infra/src/session_store.rs`
- ACP session store: `crates/zeroclaw-infra/src/acp_session_store.rs`
- Cron persistence: `crates/zeroclaw-runtime/src/cron/store.rs`
- SOP persistence: removed with the SOP run side ([Background work lifecycle](background-work-lifecycle.md)); a legacy `data/sop/runs.db` on an old install is reported by a boot-time warning
- Durable task supervision: removed with the control-plane migration wall; durable work runs through the Tachi bridge (`crates/zeroclaw-runtime/src/tachi_bridge/`)
- Logs: `crates/zeroclaw-log/`
- Cost ledger: `crates/zeroclaw-cost/src/tracker.rs`; shared results: `crates/zeroclaw-api/src/cost.rs`
- Pairing guard: `crates/zeroclaw-runtime/src/security/pairing.rs`
- Stateless token encoding/comparison: `crates/zeroclaw-config/src/tokens.rs`
- Device registry: `crates/zeroclaw-gateway/src/api_pairing.rs`
- Reload endpoint: `crates/zeroclaw-gateway/src/lib.rs`
