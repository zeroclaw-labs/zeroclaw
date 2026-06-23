# MCP

ZeroClaw is an MCP client: it connects to external [Model Context Protocol](https://modelcontextprotocol.io) servers and exposes their tools to the agent. Each MCP tool is namespaced as `<server>__<tool>` (for example `filesystem__read_file`), so tools from different servers never collide.

## Configure MCP

MCP support is enabled by default, but no external MCP tools are exposed until at least one server is configured under `mcp.servers` and an agent is granted that server through its `mcp_bundles` (see Per-agent server scoping below). Configure through the gateway, zerocode, or `zeroclaw config set`:

```sh
zeroclaw config set mcp.servers.filesystem.command npx
```

Set `mcp.enabled = false` to disable MCP tool loading without removing server definitions.

## Per-agent server scoping (`mcp_bundles`)

A `[[mcp.servers]]` entry only *defines* a server. Which servers an agent actually connects to is decided by that agent's `agents.<alias>.mcp_bundles`, and the model is secure by default: omission is not a grant.

- An agent with no `mcp_bundles` connects to **no** MCP servers, even when `mcp.servers` is non-empty.
- A bundle `[mcp_bundles.<alias>]` names the servers it grants by their `mcp.servers` `name`, with an optional `exclude` list. An agent's grant is the union of the servers across every bundle it references, minus any name excluded by any of those bundles (deny wins).
- An unknown bundle alias, or a bundle server name that matches no configured server, grants nothing. Both fail closed and are reported as non-fatal warnings by config validation, so a typo narrows an agent's access instead of widening it.

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"

[mcp_bundles.files]
servers = ["filesystem"]

[agents.assistant]
mcp_bundles = ["files"]   # connects to `filesystem`; an agent without this gets no MCP servers
```

This is the *connection* boundary (which servers an agent talks to at all). The `allowed_tools` / `excluded_tools` controls below are the per-tool *capability* boundary applied on top of whatever a granted server exposes.

## Transports

A server is reached over one of three transports (the `transport` field):

| Transport | When to use | Required fields |
|---|---|---|
| `stdio` (default) | A local process you spawn (a Node.js or Python MCP server) | `command`, optional `args`, `env` |
| `http` | A remote server speaking MCP over HTTP POST | `url`, optional `headers` |
| `sse` | A remote server speaking MCP over HTTP + Server-Sent Events | `url`, optional `headers` |

`env` (stdio) and `headers` (http/sse) are stored as secrets; `headers` commonly carries the `Authorization: Bearer …` token for the upstream server.

Add a server through the gateway, zerocode, or `zeroclaw config set` (for example `zeroclaw config set mcp.servers.filesystem.command npx`). A stdio server needs `command` plus optional `args`/`env`; an http/sse server needs `url` plus optional `headers`. The per-field commands are in the field table below.

## Editing servers

Three surfaces edit the same `[[mcp.servers]]` table:

- **`config.toml`**: hand-edit the keys documented below. The full table is round-tripped on save.
- **zerocode TUI** (`/config` -> `mcp.servers`): first-class per-field editor. The section shows one row per server, labeled with the server's `name`; enter a row to edit `transport`, `command` / `url`, `headers`, `env`, and `tool_timeout_secs` as individual fields. `+ Add` creates a new entry seeded with the name you supply; deleting from the alias list removes the entry. The `name` field is not edited inline because renaming the natural key mid-edit would invalidate in-flight references; use the dashboard or hand-edit `config.toml` to rename for now.
- **Web dashboard**: currently renders `mcp.servers` through a JSON-array editor. A migration to the same per-field surface the TUI uses is planned; until then the dashboard remains a usable but coarser editor.

## Server fields

Per-server fields (`[[mcp.servers]]`), generated from the schema:

{{#config-fields mcp.servers}}

`tool_timeout_secs` is an optional per-call timeout; it must be greater than 0 and is capped at 600 seconds.

## Top-level fields

{{#config-fields mcp}}

## Deferred loading

`mcp.deferred_loading` is `false` by default, so configured MCP tools are included in the model context eagerly. Set it to `true` to place only MCP tool **names** in the system prompt; the LLM calls the built-in `tool_search` tool to fetch a tool's full schema before invoking it. This keeps the initial context window small when a server exposes many tools.

## Security and approval

MCP tool calls go through the same approval gate as every other tool, governed by the agent's risk profile (`risk_profiles.<alias>`). The `tool_search` discovery step is auto-approved so deferred MCP loading can work in non-interactive sessions, but tools discovered from MCP servers still follow the normal approval policy:

- At autonomy `level = full`, no tool call prompts (MCP tools included).
- Otherwise, an MCP tool call prompts for approval unless its **prefixed** name (`<server>__<tool>`) is in the profile's `auto_approve` list. `auto_approve = ["*"]` approves everything; an exact entry like `auto_approve = ["filesystem__read_file"]` approves just that tool.
- `always_ask` is the inverse: a name (or `"*"`) there always prompts, overriding `auto_approve`.

### Authorization: `allowed_tools` / `excluded_tools`

Approval gates *when* a tool call needs a human green-light. Authorization gates *whether* the agent can call a tool at all. The two are independent.

Keep the three MCP tool controls on their own axes:

| Control | Scope | Use it for |
|---|---|---|
| `tool_filter_groups` | Prompt/context exposure | Decide which MCP tool schemas are visible to the model for a turn. |
| `auto_approve` / `always_ask` | Approval policy | Decide whether a selected MCP tool call requires operator approval. |
| `allowed_tools` / `excluded_tools` | Capability policy | Decide which prefixed tool names the risk profile may use at all. |

For runtime-discovered MCP tools the capability contract has an MCP-specific exception:

- If the risk profile's `allowed_tools` is empty or omitted, no authorization constraint applies; every discovered tool (MCP or built-in) is reachable. The TOML config does not distinguish an omitted field from `allowed_tools = []`; both deserialize to the same "no authorization constraint" state at the risk-profile level. If you need an explicit deny-all gate, do it on the caller-supplied per-run `allowed_tools` (cron jobs and other narrowers pass that list in directly) or via `excluded_tools` covering the specific tools you want blocked.
- If `allowed_tools` is non-empty, any MCP tool whose name contains `__` (the `<server>__<tool>` convention) is auto-admitted into the effective allow-list without being listed there individually. Non-MCP built-ins still need an exact entry.
- `excluded_tools` always subtracts, including from the auto-admitted MCP set. To block a single MCP tool like `filesystem__write_file` while keeping the rest of the `filesystem` server reachable, put it in `excluded_tools`.

The rationale: before this exception, every agent that pinned an `allowed_tools` list to lock down its built-in surface would silently lose every MCP tool, even ones the operator explicitly configured. The cost is that the deny-list is now the operator's primary lever for blocking destructive MCP capabilities under an allow-list-pinned profile.

If you want the strict pattern from before this change, where you only admit MCP tools you list explicitly with no `__` auto-admit, combine an explicit `allowed_tools` entry with an `excluded_tools` entry per destructive sibling you need blocked:

```toml
[risk_profiles.assistant]
allowed_tools = [
  "file_read",
  "filesystem__read_file",
]
# Block the destructive sibling that would otherwise be auto-admitted via
# the `__` exception above.
excluded_tools = [
  "filesystem__write_file",
]
auto_approve = [
  "filesystem__read_file",
]
```

The MCP `__` auto-admit exception is scoped to the **risk profile**'s `allowed_tools` only. Caller-supplied per-run allow-lists, like a cron job `allowed_tools` or any other narrowed invocation that passes an explicit list into the runtime, are still treated as strict explicit-list intersections, with no `__` auto-admit on top. A cron job that narrows itself to `allowed_tools = ["cron_add"]` will not surface `filesystem__write_file` to the model even when the agent's risk profile would otherwise auto-admit it via the `__` convention; the per-run narrowing remains a reliable capability boundary regardless of how many MCP servers are configured.

`auto_approve` alone does not hide a tool from the model; it only answers the approval question after the model selects that tool. Use `tool_filter_groups` to reduce prompt noise and `allowed_tools` / `excluded_tools` to enforce a capability boundary.

See [Autonomy levels](../security/autonomy.md) for the full per-profile field surface, and the [Config reference](../reference/config.md#mcp) for every MCP field and default.
