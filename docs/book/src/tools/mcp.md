# MCP

ZeroClaw supports the **Model Context Protocol (MCP)**, allowing you to extend the agent's capabilities with external tools and context providers. This guide explains how to register and configure MCP servers.

## Overview

MCP servers can be connected via three transport types:
- **stdio**: Long-running local processes (e.g., Node.js or Python scripts).
- **sse**: Remote servers via Server-Sent Events.
- **http**: Simple HTTP POST-based servers.

## Configuration

MCP servers are configured under `[mcp]` and `[[mcp.servers]]` in `config.toml`. The display `name` (used as the tool prefix `name__tool_name`) is required, plus `transport` (`stdio` | `sse` | `http`) and the transport-specific fields. See the [Config reference](../reference/config.md) for the full field index and defaults.

Keep `deferred_loading = true` (the default) to load tool schemas on demand — this minimizes initial token overhead.

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
