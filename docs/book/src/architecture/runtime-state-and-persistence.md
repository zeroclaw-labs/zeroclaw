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
│   ├── sop/runs.db             # optional durable SOP run state
│   ├── control_plane.db        # task supervision records
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
| Config values | `zeroclaw-config::Config` loaded from `config.toml` | `<install>/config.toml` | daemon `Arc<RwLock<Config>>` plus per-subsystem resolved views | `/admin/reload` re-reads config and re-instantiates daemon subsystems; direct config writes use schema validation and dirty-path checks. RPC-side config mutations additionally serialize their whole read-mutate-flush section on `RpcContext::config_write_lock` (tokio mutex first, `parking_lot` `RwLock` second, and never a `parking_lot` guard held across an `.await` or `lock().await`) | Do not cache config-derived facts in long-lived structs unless the cache is explicitly rebuilt on reload. |
| Encrypted secrets | Config secret fields plus `.secret_key` | `<install>/config.toml`, `<install>/.secret_key` | secret-store helpers in `zeroclaw-config` | Reload observes changed config; losing `.secret_key` makes encrypted config secrets unrecoverable | Never copy decrypted values into logs, docs, PR bodies, or runtime metadata. |
| Agent filesystem identity | Per-agent workspace files | `<install>/agents/<alias>/workspace/` | effective `SecurityPolicy` and agent prompt construction | Created lazily when the agent starts; workspace access is evaluated from config | This is the filesystem sandbox, not the config source of truth for providers/channels/tools. |
| Shared skill bundles | Configured skill bundle entries and resolved bundle dirs | `<install>/shared/skills/<bundle>/` by default | skill loading / prompt enrichment | Reload and new agent starts observe config and filesystem changes | Bundle aliases and directory resolution come from config; the files are the bundle content. |
| Conversation memory | `zeroclaw-memory` backend selected per agent | SQLite/Postgres/Lucid/Qdrant/Markdown backend locations; SQLite shared store lives under `data/memory/` | `Arc<dyn Memory>` wrapped in agent-scoping adapters | Backend choice is locked once an agent has written data; same-backend cross-agent recall is opt-in | Memory rows are agent-scoped. Do not replace memory ownership with copied prompt/session caches. |
| Chat and channel sessions | `[channels].session_backend` plus `SessionBackend` | Default `data/sessions/sessions.db`; legacy/explicit JSONL uses `data/sessions/*.jsonl` | `zeroclaw-infra` session backend shared by channels, gateway, RPC tools | SQLite backend uses WAL; `SessionActorQueue` serializes active turns per session | Chat/Code sessions use this unified backend. ACP protocol sessions use a separate store. |
| ACP sessions | ACP protocol session store | `data/sessions/acp-sessions.db` | `AcpSessionStore` opened at daemon boot and in RPC context | WAL-backed SQLite store, separate from chat sessions | ACP `session/load` and `session/resume` operate on this protocol store, not the chat session backend. |
| Live RPC/TUI sessions | RPC `SessionStore` | none by itself | `crates/zeroclaw-runtime/src/rpc/session.rs` in-memory map | Process-local; session history persists only through the chat or ACP backend | Live session handles, uploads, cancel tokens, owners, and overrides are runtime state. |
| Cron jobs | Declarative config membership plus cron SQLite store | `data/cron/jobs.db` | `zeroclaw-runtime::cron` scheduler/store | Read paths do not create `jobs.db`; scheduler owns due/lock state | Declarative jobs are reconciled from config, while run metadata and locks live in the cron DB. |
| SOP runs | `SopEngine` plus `SopRunStore` | None by default; `data/sop/runs.db` when durable SQLite initialization succeeds | SOP engine active/finished run caches | The durable store owns admission claims and persisted revisions; the engine restores active and terminal state on startup | Store initialization failure logs a warning and falls back to memory. Memory-backed audit records are not the run-lifecycle source of truth. |
| Background task supervision | Durable task control plane | `data/control_plane.db` | control-plane handle, task producers, and reaper | Owner PID/boot ID identifies prior-boot orphans; heartbeat timeout applies only to producers that emit heartbeats | Current delegate/subagent producers register best-effort rows but leave heartbeat, parent, route, and principal fields absent. Goal APIs exist, but end-to-end goal execution is not yet wired. |
| Background delegate results | Delegate result record | `<workspace>/delegate_results/<task-id>.json` | delegate tool cancellation registry and running future | Result files survive restart; live cancellation handles do not | Reads are file-first and overlay only `lost` or `timed_out` supervision when the file still says `running`; result and control-plane writes are independent and can diverge. |
| Runtime logs | `zeroclaw-log` event schema and subscriber layer | `data/state/runtime-trace.jsonl` when persistence is enabled | broadcast hook, JSONL writer, `/api/logs` reader, `Observer` bridge | Rolling/full/none persistence is config-controlled; dashboard SSE receives events even when JSONL is disabled | Logs are evidence and observability, not the source of user config or session state. |
| Cost ledger | `CostTracker` plus rate config | `data/state/costs.jsonl` | process-global `CostTracker` | Reload hot-swaps `CostConfig`; the tracker is constructed on demand if cost tracking becomes enabled | Existing records keep their recorded price; rate edits affect future requests after reload. |
| Gateway pairing tokens | `PairingGuard` from `gateway.paired_tokens` | token hashes in config | pairing guard | Reload reconstructs the guard from config | Valid bearer tokens are config state, not `devices.db` rows. |
| Paired device metadata | Device registry rows keyed by token hash | `data/devices.db` | `DeviceRegistry` cache plus SQLite | Registry reconciles metadata against the canonical paired-token set | This DB makes paired devices visible/manageable; it does not invent valid tokens. |
| Health and component status | running subsystems report component state | none | gateway health/status state | Process-local; reset/rebuilt on daemon restart or reload | `/health`, `/api/health`, and `/api/status` are current observations, not durable configuration. |
| Queues, debouncers, watchdogs | `zeroclaw-infra` process utilities | none unless a caller stores results elsewhere | in-memory queues/debouncers/watchdogs | Process-local; used to serialize, coalesce, or detect stalls | Treat these as coordination state. Persist only the domain data they protect, not the queue itself. |

## Reload and restart

`POST /admin/reload` sends an in-process reload signal to the daemon. The outer
daemon loop re-reads config from disk and re-runs the daemon, creating fresh
gateway, channel, heartbeat, scheduler, MQTT, session, memory, and cost wiring
from the new config. The PID stays the same, but listeners briefly rebind.

A full process restart also rotates process-local state such as live RPC
sessions, health snapshots, actor queues, and any ephemeral tool-receipt key.
Durable stores survive restart according to the table above.

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

Do not run two daemons against the same install root. Several stores use SQLite
with a single-writer model, and the process-local caches assume one daemon owns
the instance.

## Source pointers

- Config, install-root, and data-dir resolution: `crates/zeroclaw-config/src/schema.rs`
- Session backends: `crates/zeroclaw-infra/src/session_sqlite.rs`, `crates/zeroclaw-infra/src/session_store.rs`
- ACP session store: `crates/zeroclaw-infra/src/acp_session_store.rs`
- RPC live sessions: `crates/zeroclaw-runtime/src/rpc/session.rs`
- Cron persistence: `crates/zeroclaw-runtime/src/cron/store.rs`
- SOP persistence: `crates/zeroclaw-runtime/src/sop/store/`
- Background task and goal supervision: `crates/zeroclaw-runtime/src/control_plane/`
- Background delegation results: `crates/zeroclaw-runtime/src/tools/delegate.rs`
- Logs: `crates/zeroclaw-log/`
- Cost ledger: `crates/zeroclaw-config/src/cost/tracker.rs`
- Pairing guard: `crates/zeroclaw-config/src/pairing.rs`
- Device registry: `crates/zeroclaw-gateway/src/api_pairing.rs`
- Reload endpoint: `crates/zeroclaw-gateway/src/lib.rs`
