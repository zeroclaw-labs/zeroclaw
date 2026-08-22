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

The knowledge graph (the `[knowledge]` tool store) follows the same attribution model on one install-wide SQLite file. Every node and edge carries the writing agent's alias in an `owner_agent` column; the tool binds the calling agent's identity at registration and scopes every action to rows that agent owns plus rows owned by aliases in the agent's `workspace.read_knowledge_from` allowlist. That allowlist is validated like `read_memory_from` (no self-reference, no dangling alias) but has no backend-compatibility rule because the knowledge store is a single shared file rather than a per-agent backend.

Pre-attribution rows have no owner and fail closed. Startup assigns them automatically when exactly one agent is enabled. In a multi-agent install, the tool remains unavailable until `knowledge.legacy_owner_agent` names an enabled agent; after assignment, normal allowlists control any sharing.

## Rename and delete lifecycle

Use the gateway dashboard's agent controls or the dedicated `zeroclaw agents` CLI for rename and delete. In the standard build with `gateway` and `agent-runtime` enabled, both surfaces run the reference and owned-state cascades; directly removing or re-keying `agents.<alias>` in TOML or through a generic config setter does not. A reduced-feature CLI still updates config references but warns that owned state was not cascaded, so use a build with both features enabled for lifecycle operations.

Both operations make the config change durable before running owned-state side effects. Rename rewrites config references first, then moves the default per-alias workspace and re-points memory, knowledge graph, cron, ACP, and session state. Delete first refuses hard references and live ACP sessions, then removes the config entry and soft references before attempting workspace archival, memory and knowledge export and cleanup, and session-attribution clearing.

The post-persist side effects are best-effort and report surfaced failures, but archive-file write failures may appear only in gateway logs. Rename warnings call for retrying the same gateway API rename to converge residue left under the old alias. After deletion, verify the archive contents and logs before relying on the archive for recovery. Automated restore is not supported.

See [Multi-agent setup walkthrough](../contributing/multi-agent-setup.md#rename-an-agent) for the current controls, blockers, archive layout, and operator checks.

## Not supported today

1. Cross-backend cross-agent memory access (e.g. SQLite agent reading a Postgres agent's rows).
2. Automated restore from an agent deletion archive.
3. Per-agent secret namespacing: there is a single workspace-wide `SecretStore`.
4. Lucid wire-format extensions for cross-agent scoping.
