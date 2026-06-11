# Runtime internals

This page is the architecture-depth companion to the rest of the Agents
section: how the runtime enforces per-agent permissions, scopes memory, and
attributes logs. For configuring and running agents, start at
[Agents](./overview.md); for the schema-level field reference, see
[Config](../reference/config.md); for live setup steps, see
[Multi-agent setup](../contributing/multi-agent-setup.md).

## Permissions model

Each agent's effective `SecurityPolicy` is built by `SecurityPolicy::for_agent(config, alias)`:

1. Start from the agent's risk profile (`[risk_profiles.<profile>]`).
2. Set the boundary to the per-agent workspace dir (`<install>/agents/<alias>/workspace/`).
3. Walk `[agents.<alias>.workspace.access]`:
   - `Read` → sibling's workspace lands in the read-only allowlist.
   - `Write` / `ReadWrite` → sibling's workspace lands in the read-write allowlist.
4. If `[agents.<alias>.workspace.unrestricted_filesystem]` is `true`, flip `workspace_only` off.

The read-only allowlist is honored by `file_read` (and other read-side tools); the read-write allowlist gates `file_write`, `file_edit`, `git_operations`, and the shell tool's path-touching invocations. POSIX device files (`/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`) are always readable so shell idioms keep working without per-agent config.

SubAgent spawns enforce the rule that a child cannot escalate beyond its parent. The validator's full axis list and the budget-sharing behavior are documented at [Delegation → Permission inheritance](./delegation.md#permission-inheritance).

## Memory model

Each agent has its own `Arc<dyn Memory>` instance. The factory (`zeroclaw_memory::create_memory_for_agent`) dispatches by backend kind:

- **SQLite / Postgres / Lucid**: shared install-wide store. The `agents` table maps alias → UUID, and the `memories` table carries `agent_id` referencing that UUID. The factory wraps the inner backend in `AgentScopedMemory`, which stamps the bound agent's UUID on every store via `store_with_agent` and filters every recall via `recall_for_agents` with the resolved allowlist.
- **Markdown**: per-agent dir. Each agent's `MarkdownMemory` writes to `<install>/agents/<alias>/workspace/MEMORY.md` and `memory/YYYY-MM-DD.md`. Cross-agent recall is composed by `AgentScopedMarkdownMemory`, which holds the bound agent's `MarkdownMemory` plus a peer set of `(alias, MarkdownMemory)` pairs and unions their results with `[<alias>] ` attribution prefixes on each row.
- **Qdrant**: shared collection, payload-keyed. The `agent_id` payload field is the per-agent attribution; `recall_for_agents` over-fetches and post-filters by payload.
- **None**: no-op stub. The wrapper still exists so the runtime path is uniform.

Cross-backend cross-agent memory is not supported: the schema validator at config load rejects `read_memory_from` entries that point at a sibling on a different backend.

## Not supported today

1. Cross-backend cross-agent memory access (e.g. SQLite agent reading a Postgres agent's rows).
2. Agent rename (the `agents.id` UUID indirection is the rename-ready foundation, but no CLI/UI surface exists).
3. Pre-delete archive and restore.
4. Per-agent secret namespacing: there is a single workspace-wide `SecretStore`.
5. Lucid wire-format extensions for cross-agent scoping.
6. A dedicated `zeroclaw agents` management CLI for creating/deleting/listing agents.
