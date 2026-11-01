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
│   │   ├── session-events.db  # native SQLite session events
│   │   ├── session-events-jsonl/ # native JSONL segments and manifest
│   │   ├── session-format.json # installation backend, generation and upgrade phase
│   │   └── *.backup           # archived legacy SQLite, JSONL and ACP sources
│   ├── cron/jobs.db            # scheduled job state
│   ├── sop/runs.db             # optional durable SOP run state
│   ├── control_plane.db        # task supervision records
│   ├── state/
│   │   ├── runtime-trace.jsonl # persisted logs
│   │   └── costs.jsonl         # cost ledger
│   ├── devices.db              # paired-device metadata
│   ├── plugin-state.db          # encrypted exact-instance plugin state
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
| Config values | `zeroclaw-config::Config` loaded from `config.toml` | `<install>/config.toml` | daemon `Arc<RwLock<Config>>` plus per-subsystem resolved views | `/admin/reload` re-reads config and re-instantiates daemon subsystems; direct config writes use schema validation and dirty-path checks. RPC-side config mutations additionally serialize their whole read-mutate-flush section on `RpcContext::config_write_lock` (tokio mutex first, `parking_lot` `RwLock` second, and never a `parking_lot` guard held across an `.await` or `lock().await`); gateway HTTP config mutations likewise serialize their whole read-mutate-swap section on `AppState::config_write_lock` (same lock order) | Do not cache config-derived facts in long-lived structs unless the cache is explicitly rebuilt on reload. |
| Config save durability | `save()` / `save_dirty()` atomic-write path in `zeroclaw-config` | `<install>/config.toml` plus retained `config.toml.bak` | same as config values | Writes go through a temp file, pre-replacement directory sync, atomic rename, then a post-replacement directory sync | `Ok(())` means the replacement is **visible**, not that rename durability is proven: a pre-replacement failure aborts with disk and live config unchanged, but a post-rename directory-sync failure still returns `Ok(())` with a warning logged and `config.toml.bak` retained. After a crash immediately following such a save, the directory entry may replay to the prior file; callers must not treat `Ok` as a stronger durability guarantee, and recovery can consult the retained `.bak`. |
| Encrypted secrets | Config secret fields plus `.secret_key` | `<install>/config.toml`, `<install>/.secret_key` | secret-store helpers in `zeroclaw-config` | Reload observes changed config; losing `.secret_key` makes encrypted config secrets unrecoverable | Never copy decrypted values into logs, docs, PR bodies, or runtime metadata. |
| Plugin durable state | Authenticated state envelopes keyed by the host-issued plugin instance identity and portable logical key | `data/plugin-state.db`, encrypted by `<install>/.secret_key` | runtime `PluginStateStore` injected through `PluginHostServices` | SQLite WAL and compare-and-swap revisions serialize writes; tool/channel service frames recheck effective grants | The DB exposes only keyed blind indexes and `enc2:` ciphertext. Back it up with `.secret_key`; a missing or replaced key fails closed. |
| Agent filesystem identity | Per-agent workspace files | `<install>/agents/<alias>/workspace/` | effective `SecurityPolicy` and agent prompt construction | Created lazily when the agent starts; workspace access is evaluated from config | This is the filesystem sandbox, not the config source of truth for providers/channels/tools. |
| Shared skill bundles | Configured skill bundle entries and resolved bundle dirs | `<install>/shared/skills/<bundle>/` by default | skill loading / prompt enrichment | Reload and new agent starts observe config and filesystem changes | Bundle aliases and directory resolution come from config; the files are the bundle content. |
| Conversation memory | `zeroclaw-memory` backend selected per agent | SQLite/Postgres/Lucid/Qdrant/Markdown backend locations; SQLite shared store lives under `data/memory/` | `Arc<dyn Memory>` wrapped in agent-scoping adapters | Backend choice is locked once an agent has written data; same-backend cross-agent recall is opt-in | Memory rows are agent-scoped. Do not replace memory ownership with copied prompt/session caches. |
| Native session history | Committed session events; installation manifest selects the backend | `data/sessions/session-events.db` or `data/sessions/session-events-jsonl/`; `session-format.json` fences upgrades and backend changes | kernel `SessionCoordinator`, injected through `NativeSessionHub` | Foreground execution lane, short stream commit gate, atomic backend append; current authority is resolved again at effect and read boundaries | Message/context/model-selection/search views derive from events. A disconnected viewer does not own or terminate execution. See [Append-only session events](./session-events.md). |
| Legacy transcripts | Upgrade sources, retained as backups after verification | Old SQLite, JSONL and ACP files are renamed with `.backup` | installation importer | The installation fence rejects old writers throughout upgrade and after Ready | No per-session locator or dual writing remains after migration. |
| ACP sessions | Standard session events and the same `Session` projection as RPC and CLI | Configured native session store; `acp_<id>` is only a family namespace | `SessionRepository` and the shared native execution source | Execution metadata uses the coordinator commit lock; queries resolve the fixed event prefix | Ownership, workspace, plans, usage, lifecycle and context have no separate ACP database or metadata table. |
| Live RPC/TUI sessions | Runtime execution handles; durable model selection derives from native events | none for handles | `crates/zeroclaw-runtime/src/rpc/session.rs` in-memory map | Session incarnation guards stale work; uploads, cancel tokens and connected-client state are ephemeral | The live agent/provider box implements the committed selection; its map is not a second durable history owner. |
| Cron jobs | Declarative config membership plus cron SQLite store | `data/cron/jobs.db` | `zeroclaw-runtime::cron` scheduler/store | Read paths do not create `jobs.db`; scheduler owns due/lock state | Declarative jobs are reconciled from config, while run metadata and locks live in the cron DB. |
| SOP runs | `SopEngine` plus `SopRunStore` | None by default; `data/sop/runs.db` when durable SQLite initialization succeeds | SOP engine active/finished run caches | The durable store owns admission claims and persisted revisions; the engine restores active and terminal state on startup | Store initialization failure logs a warning and falls back to memory. Memory-backed audit records are not the run-lifecycle source of truth. |
| Background task supervision | Durable task control plane | `data/control_plane.db` | observer/producer handles plus the daemon-only recovery owner and reaper | Owner PID plus process-start identity prevents recovery from reclaiming live work; heartbeat timeout applies only to producers that emit heartbeats | Background delegates require a task row and atomically settle status, output reference, and error there. A process-local supervisor retries failed terminal writes with capped backoff. Their originating agent alias scopes new-row result and cancellation access; terminal pre-route rows retain exact-ID read compatibility only. Subagent registration remains best-effort and leaves route absent. Background delegate rows record the launching tool-loop session key as principal and refresh a heartbeat while running, leaving only the parent field absent; subagent rows leave heartbeat, parent, and principal fields absent. Goal APIs exist, but end-to-end goal execution is not yet wired. |
| Background delegate output | Delegate output artifact | `<workspace>/delegate_results/<task-id>.json` | delegate tool cancellation registry and running future | Output files survive restart; live cancellation handles do not | Current artifacts contain task identity and output but no lifecycle status, error, or timestamps. Reads take lifecycle state from the task row; legacy status-bearing files are fallback-only when no row exists. |
| Runtime logs | `zeroclaw-log` event schema and subscriber layer | `data/state/runtime-trace.jsonl` when persistence is enabled | broadcast hook, JSONL writer, `/api/logs` reader, `Observer` bridge | Rolling/full/none persistence is config-controlled; dashboard SSE receives events even when JSONL is disabled | Logs are evidence and observability, not the source of user config or session state. |
| Cost ledger | `CostTracker` plus rate config | `data/state/costs.jsonl` | process-global `CostTracker` | Reload hot-swaps `CostConfig`; the tracker is constructed on demand if cost tracking becomes enabled | Existing records keep their recorded price; rate edits affect future requests after reload. |
| Gateway pairing tokens | `PairingGuard` from `gateway.paired_tokens` | token hashes in config | pairing guard | Reload reconstructs the guard from config | Valid bearer tokens are config state, not `devices.db` rows. |
| Paired device metadata | Device registry rows keyed by token hash | `data/devices.db` | `DeviceRegistry` cache plus SQLite | Registry reconciles metadata against the canonical paired-token set | This DB makes paired devices visible/manageable; it does not invent valid tokens. |
| Health and component status | running subsystems report component state | none | gateway health/status state | Process-local; reset/rebuilt on daemon restart or reload | `/health`, `/api/health`, and `/api/status` are current observations, not durable configuration. |
| Queues, debouncers, watchdogs | `zeroclaw-infra` process utilities | none unless a caller stores results elsewhere | in-memory queues/debouncers/watchdogs | Process-local; used to serialize, coalesce, or detect stalls | Treat these as coordination state. Persist only the domain data they protect, not the queue itself. |

## Reload and restart

A gateway WebSocket is a viewer, not the owner of its active turn. Disconnecting
detaches the viewer; explicit session abort cancels the turn and its agentic
delegate children and parallel children running tool loops. A parallel child
making a plain model call ends with that call's own timeout. Background delegate
tasks outlive the turn and stop through `cancel_task`.

`POST /admin/reload` sends an in-process reload signal to the daemon. The outer
daemon loop re-reads config from disk and re-runs the daemon, creating fresh
gateway, channel, heartbeat, scheduler, MQTT, session, memory, and cost wiring
from the new config. The PID stays the same, but listeners briefly rebind.

A full process restart also rotates process-local state such as live RPC
sessions, health snapshots, actor queues, and any ephemeral tool-receipt key.
Durable stores survive restart according to the table above.

A session's run state (`idle`, `running`, `error`) is one of those durable
values, and a process that is killed mid-turn cannot clear it. The next
process to take ownership of the data directory therefore settles it at
startup, before it admits any turn: every session still marked `running` is
recorded as `error`, with its turn id and its messages kept. The turn is not
restarted. This happens once per process. A reload stays in the same process,
so it leaves the run state of turns still in progress alone, and merely
opening the session store never changes it. If the store cannot be written at
that moment, the attempt is logged and not repeated until the next process
start. Only the SQLite backend tracks run state, and its store is settled
whenever it exists, whichever backend is configured.

## Session backend migration

Session startup upgrades the whole installation into the configured SQLite or
JSONL event format before opening execution. The installation manifest records
the backend, generation and upgrade phase; session identity comes from events.
All legacy SQLite, JSONL and ACP sessions are imported with resumable receipts,
then their source files are renamed with `.backup` before Ready is published.
Conflicting or unreadable sources fail closed. Opening a repository handle does
not independently create an ACP database or trigger an implicit migration.

Backend changes drain executors, transfer the verified event image, payloads,
receipts and erasure relationships, and reject stale writer generations.
Restoring an old transcript writer is not a rollback. See
[Append-only session events](./session-events.md) for recovery and cutover details.

## Backup and restore

For a normal single-instance install, back up the whole `<install>` directory.
At minimum, include:

- `config.toml`
- `.secret_key` if encrypted secrets are used
- `data/memory/`
- `data/sessions/`
- `data/cron/jobs.db` if cron jobs are configured through runtime surfaces
- `data/sop/runs.db` if durable SOP runs are enabled
- `data/control_plane.db` if supervised task history matters
- `data/state/costs.jsonl` if cost history matters
- `data/state/runtime-trace.jsonl` if logs are needed for incident review
- `data/devices.db` for paired-device metadata
- `data/plugin-state.db` together with `.secret_key` for plugin-owned durable state

Do not run two daemons against the same install root. Several stores use SQLite
with a single-writer model, and the process-local caches assume one daemon owns
the instance.

## Source pointers

- Config, install-root, and data-dir resolution: `crates/zeroclaw-config/src/schema.rs`
- Session backends: `crates/zeroclaw-infra/src/session_sqlite.rs`, `crates/zeroclaw-infra/src/session_store.rs`
- Standard session repository: `crates/zeroclaw-infra/src/session_repository.rs`
- Installation upgrades: `crates/zeroclaw-infra/src/session_events/upgrade.rs`
- RPC live sessions: `crates/zeroclaw-runtime/src/rpc/session.rs`
- Cron persistence: `crates/zeroclaw-runtime/src/cron/store.rs`
- SOP persistence: `crates/zeroclaw-runtime/src/sop/store/`
- Background task and goal supervision: `crates/zeroclaw-runtime/src/control_plane/`
- Background delegation results: `crates/zeroclaw-runtime/src/tools/delegate.rs`
- Logs: `crates/zeroclaw-log/`
- Cost ledger: `crates/zeroclaw-config/src/cost/tracker.rs`
- Pairing guard: `crates/zeroclaw-config/src/pairing.rs`
- Device registry: `crates/zeroclaw-gateway/src/api_pairing.rs`
- Plugin durable state: `crates/zeroclaw-runtime/src/plugin_state.rs`
- Reload endpoint: `crates/zeroclaw-gateway/src/lib.rs`
