# MCP Server Configuration

Connect external [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) servers to ZeroClaw so the agent can use tools provided by those servers.

## Prerequisites

- ZeroClaw installed and working (`zeroclaw status` passes)
- One or more MCP-compatible tool servers (stdio, HTTP, or SSE)

## Quick Start

Add the following to your `config.toml`:

```toml
[mcp]
enabled = true

[[mcp.servers]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/documents"]
```

Restart ZeroClaw. Verify with:

```bash
zeroclaw status
```

The agent will discover tools advertised by connected MCP servers and make them available during conversations.

## Configuration Reference

### `[mcp]` section

| Key                | Type     | Default | Description |
|--------------------|----------|---------|-------------|
| `enabled`          | bool     | `false` | Master switch. Must be `true` for MCP servers to connect. |
| `deferred_loading` | bool     | `true`  | When `true`, only tool names are listed in the system prompt. The LLM calls `tool_search` to fetch full schemas on demand, reducing context window usage. When `false`, all MCP tool schemas are loaded eagerly. |

### `[[mcp.servers]]` entries

Each entry configures one MCP server connection.

| Key                 | Type              | Default  | Description |
|---------------------|-------------------|----------|-------------|
| `name`              | string (required) | —        | Unique display name. Used as tool prefix: `<name>__<tool>`. |
| `transport`         | string            | `"stdio"`| Transport type: `"stdio"`, `"http"`, or `"sse"`. |
| `command`           | string            | `""`     | Executable to spawn (stdio transport only). |
| `args`              | string array      | `[]`     | Arguments passed to the command (stdio transport only). |
| `env`               | key-value map     | `{}`     | Environment variables set for the spawned process (stdio only). |
| `url`               | string            | —        | Server URL (required for `http` and `sse` transports). |
| `headers`           | key-value map     | `{}`     | HTTP headers sent with requests (`http`/`sse` only). |
| `tool_timeout_secs` | integer           | `180`    | Per-call timeout in seconds. Hard cap: 600. |

## Transport Types

### Stdio (default)

Spawns a local process and communicates over stdin/stdout using JSON-RPC 2.0.

```toml
[[mcp.servers]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/docs"]
```

### HTTP

Connects to a remote MCP server via HTTP POST.

```toml
[[mcp.servers]]
name = "remote-tools"
transport = "http"
url = "https://mcp.example.com/rpc"
headers = { Authorization = "Bearer sk-..." }
```

### SSE (Server-Sent Events)

Connects via HTTP with Server-Sent Events for streaming responses.

```toml
[[mcp.servers]]
name = "streaming-tools"
transport = "sse"
url = "https://mcp.example.com/events"
```

## Examples

### Multiple servers

```toml
[mcp]
enabled = true

[[mcp.servers]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"]

[[mcp.servers]]
name = "github"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "ghp_..." }

[[mcp.servers]]
name = "postgres"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres", "postgresql://localhost/mydb"]
```

### Eager loading (disable deferred)

By default, MCP tool schemas are loaded on demand via `tool_search`. To load all schemas upfront (useful when you have few MCP tools):

```toml
[mcp]
enabled = true
deferred_loading = false
```

### Custom timeout

For long-running tools (e.g., database queries):

```toml
[[mcp.servers]]
name = "slow-db"
transport = "stdio"
command = "/usr/local/bin/mcp-db-server"
tool_timeout_secs = 300
```

## How Tools Appear to the Agent

Each MCP tool is prefixed with the server name to prevent collisions:

- Server `filesystem` advertising tool `read_file` becomes `filesystem__read_file`
- Server `github` advertising tool `list_repos` becomes `github__list_repos`

When **deferred loading** is enabled (the default), the agent sees tool names in the system prompt and calls `tool_search` to activate them before first use. When disabled, all tool schemas are included in the LLM context window from the start.

## Troubleshooting

### "MCP servers are configured but mcp.enabled is false"

You have `[[mcp.servers]]` entries but forgot the master switch:

```toml
[mcp]
enabled = true   # <-- add this
```

### "All configured MCP server(s) failed to connect"

Check that:

1. The `command` exists and is executable (`which npx`, `which node`, etc.)
2. The MCP server package is installed or the `npx -y` auto-install works
3. For HTTP/SSE: the `url` is reachable and correct
4. Run with `RUST_LOG=debug` for detailed error output:
   ```bash
   RUST_LOG=debug zeroclaw run
   ```

### "mcp.enabled is true but no servers are configured"

Add at least one `[[mcp.servers]]` entry. See the examples above.

### Tools not appearing in conversations

- Verify `mcp.enabled = true` in your config
- Check that `deferred_loading` matches your expectation:
  - If `true` (default): the agent must call `tool_search` to activate tools
  - If `false`: tools should appear immediately in the tool list
- Look for connection errors in logs: `zeroclaw run 2>&1 | grep -i mcp`

### Server name conflicts

Each server `name` must be unique (case-insensitive). Duplicate names cause a validation error at startup.

## Further Reading

- [MCP specification](https://modelcontextprotocol.io/)
- [Config reference](../reference/config-reference.md)
- [CLI commands](../reference/cli/commands-reference.md)
