# Built-In Tool Inventory

Use this page with [First-party extensions](./first-party-extensions.md) when
deciding whether an agent-callable tool should stay in the core binary, become
feature-gated, move to a WASM plugin, ship as a skill package, or use an MCP or
CLI-backed integration.

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
| `spawn_subagent`, `delegate`, `send_message_to_peer` | Delegation is part of the agent execution model and must share risk profiles, tools, memory, and parent/child constraints. |
| `ask_user`, `escalate_to_human`, `reaction`, `poll`, `channel_room` | These are channel-bridging operator interaction primitives with late-bound channel handles and receipts. |
| `sessions_current`, `sessions_list`, `sessions_history`, `sessions_send` | Session visibility and message sending must share the daemon/gateway session backend and agent ownership boundaries. |
| `model_routing_config`, `model_switch`, `proxy_config` | These expose the current model/proxy routing control plane and should not drift from config-source behavior. |
| `read_skill` and skill-defined tools with `kind = "shell"`, `kind = "http"`, or `kind = "builtin"` | Skills are an intended extension surface, but the runtime bridge that turns installed skills into tools is core. |

## Feature-Gate Candidates

These tools are first-party today, but they deserve explicit feature/config
boundaries because they add platform, dependency, network, or UI surface area.

| Tool(s) | Boundary | Classification |
|---|---|---|
| `browser`, `browser_open`, `browser_delegate`, `text_browser` | Config-gated and runtime-dependent. | Keep first-party, but continue tightening feature/config gates because browser automation is a large trusted surface. |
| `http_request`, `web_fetch`, `web_search_tool` | Config-gated network access. | Keep first-party while SSRF, allowlist, provider routing, and receipt behavior remain ZeroClaw-owned. Revisit only after MCP/plugin replacements can express the same network policy. |
| `pdf_read` | Compile-feature gated. | Keep feature-gated; do not move until generated docs, file extraction, and path policy have an equivalent external contract. |
| SOP tools (`sop_list`, `sop_execute`, `sop_advance`, `sop_approve`, `sop_status`) | Runtime-handle gated. | Keep first-party; SOP lifecycle, approvals, and audit records are runtime state, not a generic external integration. |
| WASM plugin tools | Compile-feature and config-gated host bridge. | Keep the host bridge first-party; individual plugin capabilities should live outside core. |
| `execute_pipeline` | Config-gated tool chaining. | Keep gated until tool chaining policy, per-step receipts, and caller allowlists are stable enough to judge whether it is core. |
| `knowledge` | Config-gated knowledge surface. | Keep gated while relationship memory and graph workflows are still being promoted into user-facing docs and skills. |
| `file_upload`, `file_upload_bundle`, `file_download` | Config-gated data movement. | Keep gated; these are policy-sensitive data movement tools and need an explicit replacement before externalization. |
| `backup`, `data_management` | Local-state mutation surface. | Consider a clearer feature/config boundary because both mutate local state outside ordinary file edit flows. |
| `screenshot`, `image_info`, `canvas` | Visual/UI tool surface. | Keep for now; classify with the visual/UI tool surface once plugin and dashboard boundaries settle. |
| `llm_task` | Provider-dependent subtask execution. | Keep until provider-scoped subtask execution has a separate contract from delegation. |
| `security_ops` | Config-gated security operations. | Keep gated; security operations need first-party policy visibility until a plugin can advertise equivalent permissions, receipts, and rollback. |
| `verifiable_intent` | Config-gated trust policy. | Keep gated; intent issuance and verification affect trust policy and should stay first-party until the credential boundary is stable. |
| Hardware probes (`hardware_board_info`, `hardware_memory_map`, `hardware_memory_read`) | Peripheral-gated hardware access. | Keep first-party while hardware tools are added through the peripheral registry path and touch physical devices under ZeroClaw permission rules. |

## Externalize Later

These are the strongest candidates for moving out of the core binary once the
replacement surface exists. Until then, keep them compatible and policy-visible.

| Tool(s) | Likely long-term home | Why |
|---|---|---|
| `notion`, `jira`, `microsoft365`, `google_workspace`, `linkedin`, `composio` | Plugin, MCP server, or CLI-backed integration. | These mostly wrap third-party products and authentication models that can evolve independently from the core runtime. |
| `claude_code`, `claude_code_runner`, `codex_cli`, `gemini_cli`, `opencode_cli` | CLI-backed integration or skill package. | The external CLI already owns authentication, command behavior, and release cadence; ZeroClaw should preserve receipts and policy if it invokes them. |
| `email_search`, `email_read` | Channel companion plugin or MCP server. | Email search/read is useful but tied to external account auth and channel setup rather than the baseline agent contract. |
| `discord_search` | Channel companion plugin or archive-query skill. | It depends on a Discord archive database produced by the channel; keep it close to that channel until the archive API is explicit. |
| `image_gen`, `cloud_ops`, `cloud_patterns`, `project_intel`, `report_template` | Skill package, plugin, or MCP server. | These are optional workflows or vendor/data-service wrappers rather than core execution primitives. |
| `weather` | Skill package or HTTP-backed skill; later plugin or MCP server if parity needs custom formatting or policy. | The current built-in is a no-key `wttr.in` wrapper. A minimal lookup fits the HTTP skill shape, but full externalization still needs parity for formatted output, the `tool.weather` proxy policy, and the built-in tool name / auto-approve behavior. |
| `pushover` | Common notification path through `system.notify`, plus a narrowly scoped service plugin. | Its core shape is device notification, which overlaps the standard node capability; Pushover-specific authentication, delivery, failure modes, and adapter compatibility still need proof before it moves outside the core runtime. |
| `git_operations` | CLI-backed integration or narrowly scoped plugin. | It has local and remote repository side effects, so any external replacement must preserve policy checks, receipts, and explicit operator visibility. |

## No Action Yet

Leave these surfaces in place until another design slice produces better
evidence:

- `calculator`: tiny, dependency-light, and harmless enough that moving it out
  may cost more complexity than it saves.
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
