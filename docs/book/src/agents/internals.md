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
   - `Write` → sibling's workspace lands in the write-only allowlist.
   - `ReadWrite` → sibling's workspace lands in the read-write allowlist.
4. If `[agents.<alias>.workspace.unrestricted_filesystem]` is `true`, flip `workspace_only` off.

The read-only allowlist is honored by `file_read` and other read-side tools, including `git_operations` status, diff, log, branch, stash-list, and worktree-list commands. Worktree list returns only entries whose worktree path resolves and whose Git metadata can be validated through the applicable read grant. The read-write allowlist gates `file_write`, `file_edit`, write-classified `git_operations` commands (including worktree add/remove/prune), and the shell tool's path-touching invocations. A write-only sibling workspace is not available to `git_operations`: Git must read repository metadata before any operation. Worktree targets and linked-worktree `.git`/`gitdir`/`commondir` metadata must be within the applicable grant. `git_operations` rejects symlinks anywhere in that metadata instead of following them, even when their eventual target would otherwise be allowed. Git invocations discard inherited `GIT_*` variables; configure commit identity in repository or global Git configuration rather than environment variables. Read-only commands suppress repository-configured executable integrations and mailmap identity resolution; their fixed log format also uses raw author fields, so those commands never read `.mailmap` or `mailmap.file`. They prohibit Git transport and disable implicit promisor-object fetches, so a read that needs an unavailable partial-clone object fails instead of launching repository-selected transport. Write commands retain ordinary Git hooks and filters when the operator grants read-write access, and apply the configured per-agent sandbox boundary. With `[runtime] kind = "docker"`, write-classified `git_operations` commands are rejected before any write-classified Git command is launched because Git does not enter the runtime container; read-classified commands are unchanged, and the shell tool remains the in-container command path. In native runtime mode, a no-op sandbox therefore does not confine Git. Only the Landlock backend extends that sandbox to configured sibling roots; other sandbox backends can reject writes outside the agent workspace, mount the workspace read-only, omit the workspace, or hide global Git configuration. See [Sandboxing](../security/sandboxing.md) for backend behavior. POSIX device files (`/dev/null`, `/dev/zero`, `/dev/random`, `/dev/urandom`) are always readable so shell idioms keep working without per-agent config.

SubAgent spawns enforce the rule that a child cannot escalate beyond its parent. The validator's full axis list and the budget-sharing behavior are documented at [Delegation → Permission inheritance](./delegation.md#permission-inheritance).

## Memory model

Each agent has its own `Arc<dyn Memory>` instance. The factory (`zeroclaw_memory::create_memory_for_agent`) dispatches by backend kind:

- **SQLite / Postgres / Lucid**: shared install-wide store. The `agents` table maps alias → UUID, and the `memories` table carries `agent_id` referencing that UUID. The factory wraps the inner backend in `AgentScopedMemory`, which stamps the bound agent's UUID on every store via `store_with_agent` and filters every recall via `recall_for_agents` with the resolved allowlist. Structured grants can further filter each sibling to exact category names; the bound agent always sees all of its own categories.
- **Markdown**: per-agent dir. Each agent's `MarkdownMemory` writes to `<install>/agents/<alias>/workspace/MEMORY.md` and `memory/YYYY-MM-DD.md`. Because Markdown does not preserve per-row custom categories, the config validator and factory reject category-scoped grants for this backend; only unrestricted sibling grants are accepted. The wrapper still filters any direct peer construction defensively.
- **Qdrant**: shared collection, payload-keyed. The `agent_id` payload field is the per-agent attribution; `recall_for_agents` over-fetches and post-filters by payload.
- **None**: no-op stub. The wrapper still exists so the runtime path is uniform.

Cross-backend cross-agent memory is not supported: the schema validator at config load rejects `read_memory_from` entries that point at a sibling on a different backend. Category grants are exact-match and non-transitive; an unknown category matches no rows.

## Rename and delete lifecycle

Use the gateway dashboard's agent controls or the dedicated `zeroclaw agents` CLI for rename and delete. In the standard build with `gateway` and `agent-runtime` enabled, both surfaces run the reference and owned-state cascades; directly removing or re-keying `agents.<alias>` in TOML or through a generic config setter does not. A reduced-feature CLI still updates config references but warns that owned state was not cascaded, so use a build with both features enabled for lifecycle operations.

Rename runs the same recoverable sequence on every surface: the `zeroclaw agents rename` CLI, the gateway dashboard and API, and the daemon RPC that zerocode uses.

1. Validate both aliases and resolve the agent being renamed.
2. Write a durable recovery record.
3. Commit the config rename, which rewrites references from the old alias to the new one.
4. Move the owned state that follows the alias.
5. Re-check that state and confirm nothing is left under the old alias.
6. Clear the recovery record.

The owned state that follows a rename is:

- the default per-alias workspace, moved from `<install>/agents/<old>/workspace/` to `<install>/agents/<new>/workspace/` (a custom `workspace.path` never moves, even one that names this default location);
- memory attribution;
- cron jobs and their run-history ownership;
- ACP sessions: their owner alias, and their persisted working directory when it was the old default workspace; and
- session attribution.

The record lives at `<data_dir>/agent-lifecycle-recovery.json` with a sidecar `.lock` file, and agent file tools cannot modify it. Because it is cleared only in the last step, an interrupted or partial rename leaves it open. A store that exists but cannot be read counts as unfinished work, not as empty: the rename reports it and keeps the record. Removing leftover state by hand outside the rename does not clear the record; re-running the same rename does, once it verifies that nothing is left.

To finish a rename that reported unfinished work, re-run it with the same aliases: `zeroclaw agents rename <old> <new>`, or the same gateway API or daemon RPC request. The CLI exits non-zero until the rename has fully converged, and while a record is open it never reports the old alias as not configured. The gateway API and daemon RPC keep returning `renamed: true` with a `warnings` list while any owned state is unfinished; an empty `warnings` list means the rename has converged. A request whose old alias is not configured, has no open record, and has no leftover state is still reported as not configured.

Two conflicts need an operator and keep the record open: the destination workspace already exists and is not empty, or the destination alias already owns memory rows. Resolve them by moving or merging that state by hand, then re-run the same rename.

A rename never finishes while `<install>/agents/<old>/workspace/` still exists, whatever `<new>`'s workspace configuration is, because an agent re-created as `<old>` would adopt that directory. When `<new>` has a custom `workspace.path`, the rename removes the old directory if it is empty and otherwise reports it until its contents are moved by hand. If `<new>`'s `workspace.path` points at the old directory itself, point it elsewhere first, then re-run the rename.

If `[agents.<old>]` is back in the config while the record is open (a hand edit, say), re-running the rename is refused and moves nothing, since that entry would take over the state the rename still owes `<new>`. Remove `[agents.<old>]` by hand and re-run the rename, or abandon it.

If a rename cannot be finished, `zeroclaw agents rename <old> <new> --abandon` drops its recovery record without moving anything. `<old>` can then be created again, and the new agent adopts whatever state is still kept under that alias, so check the warnings the command prints first.

An alias retired by an unfinished rename cannot be reused until that rename converges, so a re-created agent never inherits the previous holder's workspace, memory, cron, ACP, or session state. While a record for `<old>` to `<new>` is open, creating an agent named `<old>` is refused by the CLI (including `zeroclaw config set agents.<old>.<field>` and `zeroclaw config patch`), the gateway and dashboard, the daemon RPC, `zeroclaw quickstart`, and the agent-facing config tool. An agent added through an environment override or a hand edit of `config.toml` is not refused; config load logs a warning for it instead. Renaming another agent onto `<old>` is refused too, and so is renaming or deleting `<new>` until the pending rename converges. Any other alias can still be created.

Delete makes the config change durable before running owned-state side effects. It first refuses hard references and live ACP sessions, then removes the config entry and soft references before attempting workspace archival, owned-state export and cleanup, and session-attribution clearing.

These post-persist side effects are best-effort and report surfaced failures, but archive-file write failures may appear only in gateway logs. After deletion, verify the archive contents and logs before relying on the archive for recovery. Automated restore is not supported.

See [Multi-agent setup walkthrough](../contributing/multi-agent-setup.md#rename-an-agent) for the current controls, rename recovery steps, blockers, archive layout, and operator checks.

## Not supported today

1. Cross-backend cross-agent memory access (e.g. SQLite agent reading a Postgres agent's rows).
2. Automated restore from an agent deletion archive.
3. Per-agent secret namespacing: there is a single workspace-wide `SecretStore`.
4. Lucid wire-format extensions for cross-agent scoping.
