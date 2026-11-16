# Runtime internals

This page is the architecture-depth companion to the rest of the Agents
section: how the runtime enforces per-agent permissions, scopes memory, and
attributes logs. For configuring and running agents, start at
[Agents](./overview.md); for the schema-level field reference, see
[Config](../reference/config.md); for live setup steps, see
[Multi-agent setup](../contributing/multi-agent-setup.md).

## Repeated tool failure recovery

Configured `Agent::turn` and streamed Agent turns read root `[pacing]` at the
start of each API turn. Daemon-backed agents use their existing live config
handle, so a reload applies to the next turn. Configless builders keep loop
detection disabled. The guard spans steering and model-switch rounds within
that API call; a new call starts with fresh evidence.

With `loop_detection_enabled`, identical completed failures reach an advisory
at `loop_detection_max_repeats` (default 3). The model then receives one further
identical retry. If it fails identically and the batch has no other completed
progress, the turn stops visibly while retaining all completed tool results
and receipts. Parallel duplicates beyond that one recovery retry are skipped.
A changed result, changed approach or successful completed call clears the
pending failure warning. Successful identical calls receive at most one
advisory per turn and may continue polling, including Tachi status/watch calls.

Evidence uses the actual canonical executed tool name, canonical arguments
without runtime-injected `approved`, and the complete credential-scrubbed
result before display truncation. `loop_ignore_tools` applies to that canonical
name. Approval denials, hook short circuits, unavailable tools and cancelled
batches do not consume this recovery budget. The existing maximum-iteration,
shared-budget and cancellation controls still apply. Low-level/channel callers
retain the legacy detector; its window/no-progress/minimum-elapsed settings do
not delay this Agent failure-recovery contract.

## Permissions model

Each agent's effective `SecurityPolicy` is built by `SecurityPolicy::for_agent(config, alias)`:

1. Start from the agent's risk profile (`[risk_profiles.<profile>]`).
2. Set the boundary to the per-agent workspace dir (`<install>/agents/<alias>/workspace/`).
3. Walk `[agents.<alias>.workspace.access]`:
   - `Read` → sibling's workspace lands in the read-only allowlist.
   - `Write` / `ReadWrite` → sibling's workspace lands in the read-write allowlist.
4. If `[agents.<alias>.workspace.unrestricted_filesystem]` is `true`, flip `workspace_only` off.

The read-only allowlist is honored by `file_read` (and other read-side tools); the read-write allowlist gates `file_write`, `file_edit`, `git_operations`, and the shell tool's path-touching invocations. POSIX device files (`/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`) are always readable so shell idioms keep working without per-agent config.

SubAgent spawns enforce the rule that a child cannot escalate beyond its parent. The validator's full axis list and the budget-sharing behavior live in `crates/zeroclaw-runtime/src/subagent/mod.rs` (`SecurityPolicy::ensure_no_escalation_beyond` in `crates/zeroclaw-policy/src/lib.rs`); see [Delegation & SubAgents](./delegation.md) for the retirement record of the tools that exercised it.

## Memory model

Each agent has its own `Arc<dyn Memory>` instance. The factory (`zeroclaw_memory::create_memory_for_agent`) dispatches by backend kind:

- **Tachi**: default shared install-wide store (`<data_dir>/memory/tachi.db`). Each agent alias resolves to a UUID via `ensure_agent_uuid`. The factory wraps the inner backend in `AgentScopedMemory`, which stamps the bound agent's UUID on every store via `store_with_agent` and filters every recall via `recall_for_agents` with the resolved allowlist.
- **Markdown**: per-agent dir. Each agent's `MarkdownMemory` writes to `<install>/agents/<alias>/workspace/MEMORY.md` and `memory/YYYY-MM-DD.md`. Cross-agent recall is composed by `AgentScopedMarkdownMemory`, which holds the bound agent's `MarkdownMemory` plus a peer set of `(alias, MarkdownMemory)` pairs and unions their results with `[<alias>]` attribution prefixes on each row.
- **SQLite / Lucid / Postgres / Qdrant**: removed. Selecting any of them (install-wide `memory.backend` or per-agent `memory.backend`) fails memory construction with an explicit error steering operators to `zeroclaw memory migrate-native`; the config still loads and reports the `memory_backend_removed` warning.
- **None**: no-op stub. The wrapper still exists so the runtime path is uniform.

Cross-backend cross-agent memory is not supported: the schema validator at config load rejects `read_memory_from` entries that point at a sibling on a different backend.

## Rename and delete lifecycle

Use the gateway dashboard's agent controls or the dedicated `zeroclaw agents` CLI for rename and delete. In the standard build with `gateway` and `agent-runtime` enabled, both surfaces run the reference and owned-state cascades; directly removing or re-keying `agents.<alias>` in TOML or through a generic config setter does not. A reduced-feature CLI still updates config references but warns that owned state was not cascaded, so use a build with both features enabled for lifecycle operations.

Both operations make the config change durable before running owned-state side effects. Rename rewrites config references first, then moves the default per-alias workspace and re-points memory, cron, ACP, and session state. Delete first refuses hard references and live ACP sessions, then removes the config entry and soft references before attempting workspace archival, owned-state export and cleanup, and session-attribution clearing.

The post-persist side effects are best-effort and report surfaced failures, but archive-file write failures may appear only in gateway logs. Rename warnings call for retrying the same gateway API rename to converge residue left under the old alias. After deletion, verify the archive contents and logs before relying on the archive for recovery. Automated restore is not supported.

See [Multi-agent setup walkthrough](../contributing/multi-agent-setup.md#rename-an-agent) for the current controls, blockers, archive layout, and operator checks.

## Not supported today

1. Cross-backend cross-agent memory access (e.g. Tachi agent reading a Markdown agent's rows).
2. Automated restore from an agent deletion archive.
3. Per-agent secret namespacing: there is a single workspace-wide `SecretStore`.
