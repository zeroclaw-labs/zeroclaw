# MCP

ZeroClaw is an MCP client: it connects to external [Model Context Protocol](https://modelcontextprotocol.io) servers and exposes their tools to the agent. Each MCP tool is namespaced as `<server>__<tool>` (for example `filesystem__read_file`), so tools from different servers never collide.

## Enable MCP

MCP is **off by default**. Set `mcp.enabled = true`, then add one entry per server under `mcp.servers`. Configure through the gateway, zerocode, or `zeroclaw config set`:

```sh
zeroclaw config set mcp.enabled true
```

## Transports

A server is reached over one of three transports (the `transport` field):

| Transport | When to use | Required fields |
|---|---|---|
| `stdio` (default) | A local process you spawn (a Node.js or Python MCP server) | `command`, optional `args`, `env` |
| `http` | A remote server speaking MCP over HTTP POST | `url`, optional `headers` |
| `sse` | A remote server speaking MCP over HTTP + Server-Sent Events | `url`, optional `headers` |

`env` (stdio) and `headers` (http/sse) are stored as secrets; `headers` commonly carries the `Authorization: Bearer …` token for the upstream server.

Add a server through the gateway, zerocode, or `zeroclaw config set` (for example `zeroclaw config set mcp.servers.filesystem.command npx`). A stdio server needs `command` plus optional `args`/`env`; an http/sse server needs `url` plus optional `headers`. The per-field commands are in the field table below.

## Server fields

Per-server fields (`[[mcp.servers]]`), generated from the schema:

{{#config-fields mcp.servers}}

`tool_timeout_secs` is an optional per-call timeout; it must be greater than 0 and is capped at 600 seconds.

## Top-level fields

{{#config-fields mcp}}

## Deferred loading

`mcp.deferred_loading` is `true` by default. With it on, only MCP tool **names** are placed in the system prompt; the LLM calls the built-in `tool_search` tool to fetch a tool's full schema before invoking it. This keeps the initial context window small when a server exposes many tools. Set it to `false` to eagerly include every MCP tool's full schema up front.

## Security and approval

MCP tool calls go through the same approval gate as every other tool, governed by the agent's risk profile (`risk_profiles.<alias>`):

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
