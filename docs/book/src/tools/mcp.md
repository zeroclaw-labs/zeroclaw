# MCP

ZeroClaw is an MCP client: it connects to external [Model Context Protocol](https://modelcontextprotocol.io) servers and exposes their tools to the agent. Each MCP tool is namespaced as `<server>__<tool>` (for example `filesystem__read_file`), so tools from different servers never collide.

## Configure MCP

MCP support is enabled by default, but no external MCP tools are exposed until at least one server is configured under `mcp.servers`. Configure through the gateway, zerocode, or `zeroclaw config set`:

```sh
zeroclaw config set mcp.servers.filesystem.command npx
```

Set `mcp.enabled = false` to disable MCP tool loading without removing server definitions.

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

For runtime-discovered MCP tools the authorization contract has an MCP-specific exception:

- If the risk profile's `allowed_tools` is empty or omitted, no authorization constraint applies — every discovered tool (MCP or built-in) is reachable.
- If `allowed_tools` is non-empty, any MCP tool whose name contains `__` (the `<server>__<tool>` convention) is auto-admitted into the effective allow-list without being listed there individually. Non-MCP built-ins still need an exact entry.
- If `allowed_tools = []` (explicit empty list), everything is denied, MCP included. This is the "default-deny" escape hatch.
- `excluded_tools` always subtracts, including from the auto-admitted MCP set. To block a single MCP tool like `filesystem__write_file` while keeping the rest of the `filesystem` server reachable, put it in `excluded_tools`.

The rationale: before this exception, every agent that pinned an `allowed_tools` list to lock down its built-in surface would silently lose every MCP tool, even ones the operator explicitly configured. The cost is that the deny-list is now the operator's primary lever for blocking destructive MCP capabilities under an allow-list-pinned profile.

See [Autonomy levels](../security/autonomy.md) for the full per-profile field surface, and the [Config reference](../reference/config.md#mcp) for every MCP field and default.
