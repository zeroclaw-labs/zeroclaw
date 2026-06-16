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

Keep the exposure and approval controls separate:

| Control | Scope | Use it for |
|---|---|---|
| `tool_filter_groups` | Prompt/context exposure | Decide which MCP tool schemas are visible to the model for a turn. |
| `auto_approve` / `always_ask` | Approval policy | Decide whether a selected MCP tool call requires operator approval. |
| `allowed_tools` / `excluded_tools` | Capability policy | Decide which prefixed tool names the risk profile may use at all. |

For a hard allowlist, include the MCP tool's prefixed name in
`allowed_tools` as well as any approval rule:

```toml
[risk_profiles.assistant]
allowed_tools = [
  "file_read",
  "filesystem__read_file",
]
auto_approve = [
  "filesystem__read_file",
]
```

`auto_approve` alone does not hide a tool from the model; it only answers the
approval question after the model selects that tool. Use `tool_filter_groups`
to reduce prompt noise and `allowed_tools` / `excluded_tools` to enforce a
capability boundary.

See [Autonomy levels](../security/autonomy.md) for the full per-profile field surface, and the [Config reference](../reference/config.md#mcp) for every MCP field and default.
