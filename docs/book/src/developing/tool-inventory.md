# Built-In Tool Inventory

Use this page when deciding whether an agent-callable tool should stay in the
core binary, become feature-gated, move to a WASM plugin, ship as a skill
package, or use an MCP or CLI-backed integration.

For the proposed tier of each tool the runtime registry builds, the core set
the runtime retains, and why the runtime keeps more than the FND-001 baseline,
see [Tiers and the retained core set](#tiers-and-the-retained-core-set).

This is a classification map, not a removal plan. Do not remove or externalize a
tool until the replacement preserves the operator contract: config, security
policy, tool receipts, audit visibility, compatibility, and rollback. That rule
is the accepted lighter-core policy from
[RFC #6165](https://github.com/zeroclaw-labs/zeroclaw/issues/6165); see
[Replacement-First Policy](#replacement-first-policy).

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

These tools form the minimum local agent work surface. `default_tools`
registers them together with `deliver_file`, a [tier 2](#tier-2-host-coupled)
tool that only ACP turns admit, and the full registry registers all seven
again.

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
| `TodoWrite` | Maintains the agent's structured task list inside the runtime tool surface; keep its stable tool name and lifecycle behavior in core. |
| `read_skill` and skill-defined tools with `kind = "shell"`, `kind = "http"`, or `kind = "builtin"` | Skills are an intended extension surface, but the runtime bridge that turns installed skills into tools is core. |

## Feature-Gate Candidates

These tools are first-party today, but they deserve explicit feature/config
boundaries because they add platform, dependency, network, or UI surface area.

| Tool(s) | Boundary | Classification |
|---|---|---|
| `browser`, `browser_open`, `browser_delegate`, `text_browser` | Config-gated and runtime-dependent. | Keep first-party, but continue tightening feature/config gates because browser automation is a large trusted surface. |
| `http_request`, `web_fetch`, `web_search_tool` | Config-gated network access. | Keep first-party while SSRF, allowlist, provider routing, and receipt behavior remain ZeroClaw-owned. Revisit only after MCP/plugin replacements can express the same network policy. |
| SOP tools (`sop_list`, `sop_execute`, `sop_advance`, `sop_approve`, `sop_status`, and conditional `sop_workshop`) | Runtime-handle gated; `sop_workshop` also requires procedural memory. | Keep first-party; SOP lifecycle, approvals, procedural memory, and audit records are runtime state, not a generic external integration. |
| WASM plugin tools | Compile-feature and config-gated host bridge. | Keep the host bridge first-party; individual plugin capabilities should live outside core. |
| `execute_pipeline` | Config-gated tool chaining. | Keep gated until tool chaining policy, per-step receipts, and caller allowlists are stable enough to judge whether it is core. |
| `knowledge` | Config-gated knowledge surface. | Keep gated while relationship memory and graph workflows are still being promoted into user-facing docs and skills. |
| `file_upload`, `file_upload_bundle`, `file_download` | Config-gated data movement. | Keep gated; these are policy-sensitive data movement tools and need an explicit replacement before externalization. |
| `backup`, `data_management` | Backup mutates local state; data management currently exposes retention preview and storage statistics only. | Keep explicit config boundaries. Re-enable confirmed data-management purge only after its owned categories, confirmation/audit behavior, and rollback contract are defined. |
| `screenshot`, `image_info`, `canvas` | Visual/UI tool surface. | Keep for now; classify with the visual/UI tool surface once plugin and dashboard boundaries settle. |
| `llm_task` | Provider-dependent subtask execution. | Keep until provider-scoped subtask execution has a separate contract from delegation. |
| `security_ops` | Config-gated security operations. | Keep gated; security operations need first-party policy visibility until a plugin can advertise equivalent permissions, receipts, and rollback. |
| `verifiable_intent` | Config-gated trust policy. **The `vi_verify` tool is temporarily withheld from the model-visible registry.** | Keep gated and first-party; intent issuance and verification affect trust policy and should stay first-party until the credential boundary is stable. No chain verifier exists yet, so `vi_verify` is not registered even when `verifiable_intent.enabled = true`; enabling the section now only emits a warning naming that gap, traced at process startup, again on each daemon reload, and once more when a `zeroclaw config patch` turns the section from disabled to enabled, and reported by `zeroclaw doctor` and the config API as the `verifiable_intent_tool_withheld` validation warning so it survives `observability.log_persistence = "none"`. Withholding the tool does not remove the issuance and verification library paths, which remain available to embedders. Restore registration only behind a verify-and-evaluate path that consumes a verified chain result, retiring both channels in that same change. |
| Hardware probes (`hardware_board_info`, `hardware_memory_map`, `hardware_memory_read`) | Peripheral-gated hardware access. | Keep first-party while hardware tools are added through the peripheral registry path and touch physical devices under ZeroClaw permission rules. |

`web_fetch` is [tier 1](#tier-1-core) because FND-001 D5 lists it as core, and
its `[web_fetch].enabled` config gate stays.

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
| `git_operations` | CLI-backed integration or narrowly scoped plugin. | It has local and remote repository side effects, so any external replacement must preserve policy checks, receipts, and explicit operator visibility. Worktree operations validate every requested target and linked-worktree metadata path against the applicable read/write grant; listing returns only entries whose worktree path resolves and whose metadata can be validated through the applicable read grant. Metadata symlinks are rejected rather than followed, including when their target would be otherwise allowed. Read-only commands must not execute repository-configured content filters, signature verifiers, or nested submodule diff commands, must not resolve mailmaps, and must not inspect submodule worktree state; they retain changed superproject gitlink commit IDs. They prohibit Git transport and disable implicit promisor-object fetches, so unavailable partial-clone objects fail rather than starting repository-selected transport. Write-classified commands preserve Git hooks and filters and apply the configured per-agent sandbox boundary. With `[runtime] kind = "docker"`, write-classified commands are rejected before any write-classified Git command is launched because Git does not enter the runtime container; read-classified commands are unchanged, and the shell tool remains the in-container command path. In native runtime mode, a no-op sandbox therefore does not confine Git. Only Landlock extends that sandbox to configured sibling roots; other sandbox backends can reject writes outside the agent workspace, mount the workspace read-only, omit the workspace, or hide global Git configuration. |

`git_operations` is [tier 1](#tier-1-core) because FND-001 D5 lists it as core.
The row above records a possible later home, which would first need D5 amended.

`claude_code`, `codex_cli`, `gemini_cli`, and `opencode_cli` are
[tier 2](#tier-2-host-coupled) because they run through the runtime's shared
sandbox; externalization remains a later candidate.

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

## Tiers and the retained core set

A tier says where a built-in agent tool belongs under the runtime composition
contract proposed in
[PR #11090](https://github.com/zeroclaw-labs/zeroclaw/pull/11090): constructed
by the runtime, or a candidate for an application-owned tool source. The tier
test is whether the tool can be built from that contract's `ToolRequest` alone,
that is from `config`, `agent_alias`, the resolved `SecurityPolicy`, the
selected `RuntimeAdapter`, and the agent's memory handle. A tool is
host-coupled if it needs any other runtime handle, or if the runtime changes
how it executes on the tool's name: event emission, turn admission, memory
rebinding, argument injection, loop guards, or prompt guidance selection.
Name-keyed display and description tables, such as the ACP tool-kind map, the
Matrix argument-disclosure list, and the agent loop's tool descriptions, do not
count. Tools that pass the test but stay in the runtime are recorded as
judgment calls. If the contract changes before it merges, update the tier test
here to match.

This classification is a proposal for
[#10998](https://github.com/zeroclaw-labs/zeroclaw/issues/10998), which
requires the approved core set, and the rationale for any deviation, to be
recorded on the runtime and gateway delivery tracker,
[#7432](https://github.com/zeroclaw-labs/zeroclaw/issues/7432).

Tiers record who constructs a tool under the composition contract; the buckets
above record its long-term home, so a tool can be tier 2 and an
externalization candidate.

The tables cover the tools that `all_tools_with_runtime` constructs, plus the
four that the scoped registry assembly mints. They leave out peripheral tools
from the hardware crate, which depend on the configured boards; WASM plugin
tools, skill-defined tools, and MCP wrappers, whose names are decided at
runtime; `vi_verify`, which is deliberately withheld; the session reset and
delete tools, which the agent registry does not register; and `skills_list`,
`skill_view`, and `skill_manage`, which only the opt-in background skill review
registers.

A test checks the three tier tables below against
`crates/zeroclaw-tools/src/inventory.rs`, so edit both together.

### Tier 1: core

Tier 1 is the retained core set: the baseline from
[FND-001 D5](../foundations/fnd-001-intentional-architecture.md#d5-reduce-all_tools_with_runtime-to-core-tools-only),
unchanged, which names the tools a useful agent needs with no plugins
installed.

| Tool | Why it is core |
|---|---|
| `shell` | The general way to act on the host, under the shell policy and the runtime adapter. The runtime builds it on the per-agent sandbox that it shares with `git_operations` and the coding CLIs. |
| `file_read` | Reading the workspace is the minimum for any task. It runs behind the path guard. |
| `file_write` | Writes results into the workspace behind the path guard and the runtime's persistence rules. |
| `file_edit` | Makes exact edits without rewriting whole files, behind the same guard as writes. |
| `glob_search` | Finds workspace files without depending on shell syntax. |
| `content_search` | Searches workspace contents without depending on shell syntax. |
| `git_operations` | Repository work through the runtime's Git command boundary, which wraps the same shared sandbox. |
| `memory_store` | The write path of long-term memory, a first-party runtime contract. |
| `memory_recall` | The read path of long-term memory. |
| `memory_forget` | The delete path, so wrong or unwanted memories can be removed. |
| `web_fetch` | The minimal network capability: one page, returned as plain text. |

`web_fetch` stays gated by `[web_fetch].enabled`, which defaults to true.

### Tier 2: host-coupled

Tier 2 tools are retained in the runtime beyond the core set, and the runtime
keeps constructing them.

| Family | Tools | Why the runtime keeps constructing it |
|---|---|---|
| Scheduling | `cron_add`, `cron_list`, `cron_remove`, `cron_update`, `cron_run`, `cron_runs`, `schedule` | The runtime cron store and scheduler. The runtime also keys behavior on five of the names: shell-dialect guidance on `cron_add`, `cron_update`, and `schedule`; runtime-approved argument injection on those three and `cron_run`; delivery defaults on `cron_add`; and cron agent turns without their own allowlist exclude `cron_add`, `cron_update`, `cron_remove`, `cron_run`, and `schedule`. |
| Memory plane | `memory_export`, `memory_purge` | The sealed registry rebinds all five memory tools together when a session is pinned to its owner's private memory plane, and the shared memory tool name list treats them as one unit. |
| Agent execution | `spawn_subagent`, `delegate`, `send_message_to_peer` | They start child or peer turns through the runtime's agent loop and carry the caller into them: its security policy and SOP step scope for `spawn_subagent`, its delegation policy and, for bounded targets, its ceilings for `delegate`, and its identity as the peer turn's principal for `send_message_to_peer`. `delegate` owns the parent-tools snapshot, and the tool loop exempts `spawn_subagent` and `delegate` from duplicate-call collapsing by name. |
| Control plane | `model_switch`, `model_routing_config`, `proxy_config` | The turn-scoped model-switch state that the runtime tool loop installs. The last two mutate config: both rewrite the config file, and `proxy_config` also sets the process-wide runtime proxy that the HTTP request and web fetch tools read. They can be built from the tier-test inputs, so keeping them runtime-owned is a judgment call recorded here. |
| Channel bridging | `ask_user`, `escalate_to_human`, `reaction`, `poll`, `channel_room`, `send_via`, `git_forge` | Late-bound channel maps that assembly returns to its caller, which fills them afterwards; `reaction` and `git_forge` share one map, `ask_user` and `send_via` share another. |
| Sessions | `sessions_current`, `sessions_list`, `sessions_history`, `sessions_send` | The ACP session read view, which only the ACP agent path passes; the session backend itself is opened from config. |
| SOP | `sop_list`, `sop_execute`, `sop_advance`, `sop_approve`, `sop_status`, `sop_workshop` | The SOP engine and audit logger. |
| Skills | `read_skill` | The skills loader, and the channel prompt path selects the skills prompt mode by whether `read_skill` is available. |
| Task list | `TodoWrite` | The runtime emits plan events only for this name, and that is the sole feed of the ZeroCode task tracker. |
| Canvas | `canvas` | The canvas store shared with the gateway. |
| ACP delivery | `deliver_file` | Admitted only on ACP turns; its helpers are used by the ACP server. |
| Provider-bound | `llm_task` | Builds a provider from the agent's credential outside the provider source. |
| Sandbox-bound coding CLIs | `claude_code`, `codex_cli`, `gemini_cli`, `opencode_cli` | The coding-CLI executor wraps the runtime adapter and the shared sandbox. Opt-in by config, but runtime-constructed. |
| Live config | `a2a_discover`, `a2a_send`, `a2a_get_task`, `a2a_cancel`, `file_download` | They hold the live config handle; the A2A client also uses a process-wide route cache. |
| Runtime-defined | `security_ops` | Lives in the runtime crate and uses its security playbook and vulnerability modules. |
| Built outside the factory | `execute_pipeline`, `tool_search`, `mcp_resources`, `mcp_prompts` | Minted by the scoped registry assembly from the registry itself or from the MCP registry. |

The shared memory tool name list is `MEMORY_TOOL_NAMES` in
`crates/zeroclaw-tools/src/lib.rs`.

### Tier 3: optional

Tier 3 tools can be built from `ToolRequest` alone, taking the workspace
directory from the resolved `SecurityPolicy`. They are candidates for the
application-owned tool source and for an optional-tools feature (proposed as
`tools-extra`).

| Family | Tools | Registered on a default config |
|---|---|---|
| Utilities | `calculator`, `weather`, `pushover`, `screenshot`, `image_info` | Always |
| Browser | `browser_open`, `browser`, `browser_delegate`, `text_browser` | `browser_open` yes, because the browser section is enabled by default; the others no |
| Network | `http_request`, `web_search_tool` | Yes, both default to enabled |
| SaaS integrations | `notion`, `jira`, `linkedin`, `composio`, `google_workspace`, `microsoft365` | No |
| Reporting | `project_intel`, `report_template` | No |
| Coding runner | `claude_code_runner` | No |
| Ops | `backup`, `data_management`, `cloud_ops`, `cloud_patterns` | `backup` yes; the others no |
| Media and transfer | `image_gen`, `file_upload`, `file_upload_bundle` | No |
| Channel companions | `discord_search`, `email_search`, `email_read` | No |
| Knowledge | `knowledge` | No |

Registration on a default config follows the config defaults:
`[browser].enabled`, `[http_request].enabled`, `[web_search].enabled`, and
`[backup].enabled` default to true, while `[browser].automation_enabled` and
every other gate in this table is off or unset. Nine tier 3 tools therefore
register on a default config: `calculator`, `weather`, `pushover`,
`screenshot`, `image_info`, `browser_open`, `http_request`, `web_search_tool`,
and `backup`. Existing configs receive these nine tools without opting in, so
any later gating must first migrate them.

### Deviation from the FND-001 baseline

FND-001 D5 names eleven kernel tools and says "Everything else is registered by
installed plugins." This classification deviates from that: 51 tools stay
runtime-constructed beyond the eleven. Most need a runtime handle that
`ToolRequest` does not carry, or have runtime behavior keyed on their names.
The rest pass the tier test and are kept by judgment: `cron_list` and
`cron_runs`, which share the cron store with the rest of scheduling;
`security_ops`, which uses the runtime's playbook and vulnerability modules;
and `model_routing_config` and `proxy_config`. `llm_task` builds its own
provider; under the contract it would need the provider source, which
`ToolRequest` does not carry. What would have to change before a family could
move:

- `ToolRequest` carries the shared per-agent sandbox. The coding CLIs could
  then move; `shell` and `git_operations` stay core either way.
- `ToolRequest` carries the live config handle. The A2A tools and
  `file_download` could then move.
- `ToolRequest` carries the generation's `ChannelSource`, and `ChannelSource`
  can also list the running channels, so channel-bridging tools look channels
  up instead of reading maps that the caller fills after assembly. Channel
  bridging could then move.
- `ToolRequest` carries the generation's provider source. `llm_task` could
  then move.
- The SOP engine and audit logger, the ACP session read view, and the canvas
  store reach tools through the contract. The SOP, sessions, and canvas
  families could then move.
- The runtime recognizes the plan, delivery, memory, scheduling, and
  skill-reading tools by a typed identity that it checks together with
  `ToolProvenance::Native`, not by the name string alone, including when it
  selects the skills prompt mode, and memory rebinding asks the tool source
  again with the routed memory handle. The task list, ACP delivery,
  memory-plane, and skills families could then move, and so could `cron_add`,
  `cron_update`, `cron_remove`, `cron_run`, and `schedule`.
- `security_ops`, `model_routing_config`, `proxy_config`, `cron_list`, and
  `cron_runs` need no contract change; moving them is a decision, not a
  prerequisite.
- No change above moves agent execution, `model_switch`, or the four tools
  built outside the factory: they depend on the turn loop or on the assembled
  registry.

None of these changes is proposed.
[PR #11090](https://github.com/zeroclaw-labs/zeroclaw/pull/11090) keeps these
internals out of `ToolRequest` so that they do not become public contract, and
a live config handle would cut across its rule that a request belongs to one
config generation.

### What this classification does not do

- It does not remove, gate, or move any tool. The registry registers the same
  tools under the same conditions as before.
- It does not relax the [Replacement-First Policy](#replacement-first-policy).
  Every later removal, gate, or move still needs its own issue and PR with
  replacement evidence, tier 3 tools included.

## Replacement-First Policy

[RFC #6165](https://github.com/zeroclaw-labs/zeroclaw/issues/6165) is the
accepted policy for moving working built-in integrations out of the core. Cite
it in every removal, feature-gate, or migration review, and hold the proposal
to these rules:

- A working built-in integration stays available until its replacement is
  real, documented, and independently reviewed.
- The replacement review must cover configuration migration, security policy,
  tool receipts or audit behavior where applicable, compatibility, and
  rollback.
- Each concrete removal, feature gate, or migration needs its own issue and PR
  that carries that replacement evidence.
- The RFC does not pre-approve any individual removal, feature gate, migration,
  or runtime behavior change.
- Schema V4 cleanup is a separate decision under
  [#8310](https://github.com/zeroclaw-labs/zeroclaw/issues/8310). It does not
  create a blanket exception for removing a working integration without a
  replacement path.

The [Migration Rules](#migration-rules) below are the questions that review
must answer before a built-in tool leaves the core.

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
