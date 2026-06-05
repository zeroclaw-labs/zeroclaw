# MCP

ZeroClaw supports the **Model Context Protocol (MCP)**, allowing you to extend the agent's capabilities with external tools and context providers. This guide explains how to register and configure MCP servers.

## Overview

MCP servers can be connected via three transport types:
- **stdio**: Long-running local processes (e.g., Node.js or Python scripts).
- **sse**: Remote servers via Server-Sent Events.
- **http**: Simple HTTP POST-based servers.

## Configuration

MCP servers are configured under `[mcp]` and `[[mcp.servers]]` in `config.toml`. The display `name` (used as the tool prefix `name__tool_name`) is required, plus `transport` (`stdio` | `sse` | `http`) and the transport-specific fields. See the [Config reference](../reference/config.md) for the surrounding section index.

Keep `deferred_loading = true` (the default) to load tool schemas on demand — this minimizes initial token overhead.

### `[mcp]` keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `enabled` | bool | `false` | Master switch for MCP tool loading. |
| `deferred_loading` | bool | `true` | When `true`, only tool *names* go into the system prompt; full schemas are fetched on demand via `tool_search`. |
| `servers` | `[[mcp.servers]]` array | `[]` | One entry per server. Also accepts `mcpServers` as an alias. |

### `[[mcp.servers]]` keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `name` | string | `""` | Display name; used as the tool prefix `<server>__<tool>`. Auto-filled when added via the dashboard. |
| `transport` | `"stdio"` \| `"http"` \| `"sse"` | `"stdio"` | Lowercase. |
| `url` | string | unset | **Required** for `http`/`sse`. Must parse as a URL and use scheme `http` or `https`. |
| `command` | string | `""` | **Required** (non-empty) for `stdio`; ignored for HTTP/SSE. |
| `args` | string[] | `[]` | Command arguments for `stdio` transport. |
| `env` | table&lt;string, string&gt; | `{}` | Environment variables for `stdio` transport. Values are stored as secrets. |
| `headers` | table&lt;string, string&gt; | `{}` | Request headers for `http`/`sse`. Values are stored as secrets (commonly carry Bearer tokens). |
| `tool_timeout_secs` | integer | unset | Optional per-call timeout in seconds. Hard-capped at `600`. |

Validation enforces transport-specific requirements: `stdio` rejects an empty `command`; `http` and `sse` reject a missing `url` or a non-`http(s)` scheme; any `tool_timeout_secs` above `600` is rejected at startup.

### HTTP transport example

```toml
[mcp]
enabled = true
deferred_loading = true   # optional; this is the default

[[mcp.servers]]
name = "github"
transport = "http"
url = "https://api.example.com/mcp"
tool_timeout_secs = 60

[mcp.servers.headers]
Authorization = "Bearer sk-…"
X-Custom-Header = "value"
```

Tools from this server appear to the LLM as `github__<toolname>` — the `name` field is the prefix. Swap `transport = "http"` for `transport = "sse"` to connect to the same URL over Server-Sent Events.

### stdio transport example

```toml
[[mcp.servers]]
name = "fs"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/srv/data"]

[mcp.servers.env]
LOG_LEVEL = "info"
```

## Security and Auto-Approval

By default, any tool execution from an MCP server requires manual approval unless the agent's risk-profile level is set to `full`.

To automatically approve specific tools from an MCP server, add them to `auto_approve` on the agent's risk profile (`[risk_profiles.<alias>]`):

```toml
[risk_profiles.assistant]
auto_approve = [
  "my_local_tool__read_file",   # tool from `my_local_tool` MCP server
  "my_remote_tool__get_weather" # tool from `my_remote_tool` MCP server
]
```

See [Autonomy levels](../security/autonomy.md) for the full surface of per-profile fields.

## Tips

- **Tool Filtering**: You can limit which MCP tools are exposed to the LLM using `tool_filter_groups` in your project configuration.
- **Deferred Loading**: Keeping `deferred_loading = true` reduces the initial token overhead by only sending tool names to the LLM. The agent will fetch the full schema only when it decides to use the tool.
