# Built-In Tool Inventory

Use this page when deciding whether an agent-callable tool should stay in the
core binary, become feature-gated, ship as a skill
package, or use an MCP or CLI-backed integration.

This is a classification map, not a removal plan. Do not remove or externalize a
tool until the replacement preserves the operator contract: config, security
policy, tool receipts, audit visibility, compatibility, and rollback.

The runtime registry source of truth is
`crates/zeroclaw-runtime/src/tools/mod.rs`, especially `default_tools`,
`all_tools_with_runtime`, and `register_skill_tools_with_context_and_runtime`.
The shared tool implementations live primarily under `crates/zeroclaw-tools/`.

## Classification Buckets

| Bucket | Meaning | Next action |
|---|---|---|
| Keep built-in | Part of the baseline agent contract or tightly coupled to runtime policy, receipts, memory, sessions, or delegation. | Keep in core unless the agent contract changes through an RFC. |
| Feature-gate candidate | First-party behavior still belongs in ZeroClaw, but the dependency, platform, binary-size, or operator-risk cost should not affect minimal builds. | Add or tighten a feature/config gate before considering removal. |
| Externalize later | Useful capability, but the long-term owner should be a plugin, skill package, MCP server, or external CLI because the behavior mostly wraps a product, vendor API, or optional workflow. | Keep compatibility until the external surface is real and documented. |
| No action yet | Current evidence is not enough to choose a different home. | Leave in place and revisit with source, usage, and replacement evidence. |

## Keep Built-In

These tools form the minimum local agent work surface. They are registered by
`default_tools` and again by the full registry.

| Tool(s) | Why they stay |
|---|---|
| `shell` | Executes local commands under ZeroClaw's shell policy, sandbox, runtime adapter, path guard, and receipts. |
| `file_read`, `file_write`, `file_edit` | Own the workspace file contract, persistence behavior, path guard, and audit surface. |
| `glob_search`, `content_search` | Provide local discovery without requiring shell-specific command syntax. |

These full-registry tools should also stay built in because they are runtime,
memory, coordination, or operator-control primitives rather than optional
product integrations.

| Tool(s) | Why they stay |
|---|---|
| `memory_store`, `memory_recall`, `memory_forget`, `memory_export`, `memory_purge` | Long-term memory is a first-party runtime contract and uses shared memory ownership rules. |
| `cron_add`, `cron_list`, `cron_remove`, `cron_update`, `cron_run`, `cron_runs`, `schedule` | Scheduling affects autonomous execution, ownership, and run history; keep it policy-visible in core. |
| `send_message_to_peer`, `reasoning_subagent` | Agent-child execution and peer messaging are part of the agent execution model and must share risk profiles, tools, memory, and parent/child constraints. (The legacy `spawn_subagent` and `delegate` tools are retired; see the delegation retirement record.) |
| `ask_user`, `escalate_to_human`, `reaction`, `poll`, `channel_room` | These are channel-bridging operator interaction primitives with late-bound channel handles and receipts. |
| `sessions_current`, `sessions_list`, `sessions_history`, `sessions_send` | Session visibility and message sending must share the daemon/gateway session backend and agent ownership boundaries. |
| `model_routing_config`, `model_switch`, `proxy_config` | Retired from the model surface. Routing and proxy configuration belong to the operator config API (`PUT/DELETE /api/config...`), runtime model switching to the channel `/model` command, and proxy application to startup + daemon reload. The tool implementations stay compiled but no assembly path registers them. See "Retired Operator Tools" below. |
| `TodoWrite` | Maintains the agent's structured task list inside the runtime tool surface; keep its stable tool name and lifecycle behavior in core. |
| `read_skill` and skill-defined tools with `kind = "shell"`, `kind = "http"`, or `kind = "builtin"` | Skills are an intended extension surface, but the runtime bridge that turns installed skills into tools is core. |

## Feature-Gate Candidates

These tools are first-party today, but they deserve explicit feature/config
boundaries because they add platform, dependency, network, or UI surface area.

| Tool(s) | Boundary | Classification |
|---|---|---|
| `browser`, `browser_open`, `text_browser` | Config-gated and runtime-dependent. | Keep first-party, but continue tightening feature/config gates because browser automation is a large trusted surface. |
| `http_request`, `web_fetch`, `web_search_tool` | Config-gated network access. | Keep first-party while SSRF, allowlist, provider routing, and receipt behavior remain ZeroClaw-owned. Revisit only after MCP/plugin replacements can express the same network policy. |
| SOP tools (`sop_list`, `sop_execute`, `sop_advance`, `sop_approve`, `sop_status`, `sop_workshop`) | Removed with the SOP run side. | Retired: no agent-facing SOP run tools remain; runs are Tachi-side ProcedureRuns through the procedure_v1 seam. |
| WASM plugin tools | Removed with the WASM plugin host. | Retired: extensions go through MCP servers. |
| `calculator`, `weather` | Removed from the model surface; implementations deleted. | Retired outright. The names stay reserved in `RETIRED_OPERATOR_TOOL_NAMES` so no assembly path can re-register them. A weather lookup, if ever wanted again, belongs to an HTTP skill or MCP wrapper. |
| `execute_pipeline` | Config-gated tool chaining. | Keep gated until tool chaining policy, per-step receipts, and caller allowlists are stable enough to judge whether it is core. |
| `knowledge` | Removed from the model surface; tool, graph store and config retired. | Retired: its private `knowledge.db` graph was a second store next to the Tachi memory kernel's own edges. `[knowledge]` / `[knowledge_bundles]` sections warn via the retired-surface tombstone. |
| `file_upload`, `file_upload_bundle`, `file_download` | Config-gated data movement. | Keep gated; these are policy-sensitive data movement tools and need an explicit replacement before externalization. |
| `backup`, `data_management` | Retired from the model surface. | Operator-only via the gateway operator API (`/api/agents/<alias>/backup*`, `/api/agents/<alias>/data-retention*`), which the `[backup]` / `[data_retention]` sections configure. See "Retired Operator Tools" below. |
| `screenshot`, `image_info` | Visual/UI tool surface. | Keep for now; classify with the visual/UI tool surface once plugin and dashboard boundaries settle. |
| `llm_task` | Provider-dependent subtask execution. | Keep until provider-scoped subtask execution has a separate contract from delegation. |
| `security_ops` | Retired from the model surface. | Default-off diagnostics with no operator surface; the module stays compiled but unreachable, and enabling the section only emits a startup/reload notice. See "Retired Operator Tools" below. |
| `verifiable_intent` | Config-gated trust policy. **The `vi_verify` tool is temporarily withheld from the model-visible registry.** | Keep gated and first-party; intent issuance and verification affect trust policy and should stay first-party until the credential boundary is stable. No chain verifier exists yet, so `vi_verify` is not registered even when `verifiable_intent.enabled = true`; enabling the section now only emits a warning naming that gap, at process startup and again on each daemon reload. The issuance and verification library paths are unchanged. Restore registration only behind a verify-and-evaluate path that consumes a verified chain result. |

## Retired Operator Tools

These first-party tools are no longer part of the model-visible registry on any
assembly path (ordinary, SubAgent, skill elevation, minimal or full
composition). Their implementations stay compiled; the capability moved to a
trusted surface or was deleted.

| Tool(s) | Trusted surface today |
|---|---|
| `model_routing_config` | `GET/PUT/DELETE /api/config...` (gateway config API, settings UI). Mutation actions additionally reject raw `api_key` values. |
| `model_switch` | Channel `/model` and `/models` commands (per-sender runtime switch; the parent agent cannot switch its own model through a tool). |
| `proxy_config` | `PUT /api/config/proxy.*` persists the canonical `[proxy]` config; the daemon applies it at process startup (runtime global plus, for `scope = "environment"`, process env) and refreshes the runtime global on daemon reload. The `apply_env` / `clear_env` actions were deleted outright: live process-env manipulation had no consumer and restart application replaces it. |
| `security_ops` | None (default-off diagnostics, unreachable by design). `security_ops.enabled` stays parseable; enabling it emits a startup/reload notice. |
| `backup` | `POST/GET /api/agents/<alias>/backup`, `.../backup/{name}/verify`, `.../backup/{name}/restore` (operator bearer gated; restore keeps its `confirm` guard). |
| `data_management` | `GET /api/agents/<alias>/data-retention/status`, `.../stats`, `POST .../purge` (operator bearer gated; purge keeps its `dry_run` default). |

Config sections that used to enable these as model tools still parse. Sections
that keep configuring an operator surface (`[backup]`, `[data_retention]`)
log a startup/reload notice naming that surface when enabled; nothing widens
the model-visible registry. The retired names are reserved against external tool
registration so no extension can claim one and ride it back onto the provider
wire.

## Externalize Later

These are the strongest candidates for moving out of the core binary once the
replacement surface exists. Until then, keep them compatible and policy-visible.

| Tool(s) | Likely long-term home | Why |
|---|---|---|
| `composio` | MCP server or CLI-backed integration. | These mostly wrap third-party products and authentication models that can evolve independently from the core runtime. |
| `claude_code`, `claude_code_runner`, `codex_cli`, `gemini_cli`, `opencode_cli` | Removed; retired raw launchers. | These Parent-visible raw CLI launchers were removed from the registry (#197 wall 2); harness execution goes through the typed subagent / Tachi paths instead. |
| `email_search`, `email_read` | Channel companion plugin or MCP server. | Email search/read is useful but tied to external account auth and channel setup rather than the baseline agent contract. |
| `discord_search` | Channel companion plugin or archive-query skill. | It depends on a Discord archive database produced by the channel; keep it close to that channel until the archive API is explicit. |
| `image_gen` | Skill package or MCP server. | These are optional workflows or vendor/data-service wrappers rather than core execution primitives. |
| `git_operations` | CLI-backed integration or narrowly scoped plugin. | It has local and remote repository side effects, so any external replacement must preserve policy checks, receipts, and explicit operator visibility. |

## No Action Yet

Leave these surfaces in place until another design slice produces better
evidence:

- `tool_search` and deferred MCP activation: part of the current MCP discovery
  flow, but the exact long-term boundary depends on the v0.8.2 plugin/MCP work.
- Session reset/delete tools: implementations exist, but the agent registry does
  not register the destructive unscoped variants by default. Keep that boundary
  unless an operator/admin surface explicitly needs them.

## Migration Rules

Before moving any tool out of core, the replacement must answer:

1. Which config remains first-party, and which config moves to the plugin,
   skill, MCP server, or CLI?
2. How does the replacement preserve autonomy checks, allow/deny lists, tool
   receipts, audit logs, and attribution?
3. How do existing configs fail or migrate when the built-in tool disappears?
4. Can operators see that the capability is installed, enabled, disabled,
   blocked, or missing?
5. What is the rollback path if the external package breaks?

If code proof is needed for a future slice, choose one low-blast-radius
candidate from the Externalize later table and prove the replacement path
without deleting the built-in tool in the same PR.

## Tachi delegated work

`tachi_start`, `tachi_status`, `tachi_result`, `tachi_cancel` and `tachi_watch`
are built-in L2 control tools. They use Tachi profiles and the existing session
request-binding store, with no local worker launch. They remain closed under
`[tachi]` defaults and are excluded from L1 profiles. See
[external work through Tachi](../agents/delegation.md#external-work-through-tachi)
for their request, polling and recovery contract.
