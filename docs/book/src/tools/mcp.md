# MCP

ZeroClaw supports the **Model Context Protocol (MCP)**, allowing you to extend the agent's capabilities with external tools and context providers. This guide explains how to register and configure MCP servers.

## Overview

MCP servers can be connected via three transport types:
- **stdio**: Long-running local processes (e.g., Node.js or Python scripts).
- **sse**: Remote servers via Server-Sent Events.
- **http**: Simple HTTP POST-based servers.

## Configuration

MCP servers are configured in the `[mcp]` section of your `config.toml`.

```toml
[mcp]
enabled = true
deferred_loading = true # Recommended: only load tool schemas when needed

[[mcp.servers]]
name = "my_local_tool"
transport = "stdio"
command = "node"
args = ["/path/to/server.js"]
env = { "API_KEY" = "secret_value" }

[[mcp.servers]]
name = "my_remote_tool"
transport = "sse"
url = "https://mcp.example.com/sse"
```

### Server Configuration Fields

| Field | Type | Description |
|-------|------|-------------|
| `name` | String | **Required**. Display name used as a tool prefix (`name__tool_name`). |
| `transport` | String | `stdio`, `sse`, or `http`. Default: `stdio`. |
| `command` | String | (stdio only) Executable to run. |
| `args` | List | (stdio only) Command line arguments. |
| `env` | Map | (stdio only) Environment variables. |
| `url` | String | (sse/http only) Server endpoint URL. |
| `headers` | Map | (sse/http only) Custom HTTP headers (e.g., for auth). |
| `tool_timeout_secs` | Integer | Per-call timeout for tools from this server. |

## Security and Auto-Approval

By default, any tool execution from an MCP server requires manual approval unless your autonomy level is set to `full`.

To automatically approve tools from a specific MCP server, add its prefix to the `auto_approve` list in the `[autonomy]` section:

```toml
[autonomy]
auto_approve = [
  "my_local_tool__read_file", # Allow specific tool from 'my_local_tool'
  "my_remote_tool__get_weather" # Allow specific tool from 'my_remote_tool'
]
```

## Tips

- **Tool Filtering**: You can limit which MCP tools are exposed to the LLM using `tool_filter_groups` in your project configuration.
- **Deferred Loading**: Keeping `deferred_loading = true` reduces the initial token overhead by only sending tool names to the LLM. The agent will fetch the full schema only when it decides to use the tool.
