# ACP — Agent Client Protocol

**ACP** is a JSON-RPC 2.0 protocol over stdio that lets editors and IDEs talk to a running ZeroClaw agent as a session host. Newline-delimited JSON — lightweight, streamable, easy to wire to a subprocess.

Think of it as "LSP for agents": the editor launches `zeroclaw acp`, sends prompts over stdin, receives session updates on stdout.

## What you'd use it for

- An editor extension that offers an "ask the agent about this file" command
- A terminal multiplexer integration that opens a side pane with an agent session
- A CI runner that drives the agent programmatically without a full gateway setup
- Anything that wants agent sessions without HTTP and without binding a port

## Protocol shape

All messages are JSON-RPC 2.0. The server supports these methods:

### `initialize`

Handshake. Returns server capabilities.

```json
→ {"jsonrpc":"2.0","id":1,"method":"initialize"}
← {"jsonrpc":"2.0","id":1,"result":{
    "streaming": true,
    "maxSessions": 10,
    "sessionTimeoutSecs": 3600,
    "defaultModel": "claude-haiku-4-5-20251001"   // only if configured
  }}
```

### `session/new`

Open an isolated agent session.

```json
→ {"jsonrpc":"2.0","id":2,"method":"session/new","params":{
    "cwd": "/path/to/project",
    "systemPrompt": "You are a careful code reviewer."
  }}
← {"jsonrpc":"2.0","id":2,"result":{"sessionId":"s-ab12cd"}}
```

### `session/prompt`

Send a prompt. The response is a sequence of `session/update` notifications streaming back, terminated by the response to the prompt call.

```json
→ {"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{
    "sessionId": "s-ab12cd",
    "prompt": "Summarise the changes in the last commit."
  }}
← {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-ab12cd","kind":"text","delta":"The last commit..."}}
← {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-ab12cd","kind":"text","delta":" introduces..."}}
← {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-ab12cd","kind":"tool_call","name":"shell","args":{...}}}
← {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-ab12cd","kind":"tool_result","...": "..."}}
← {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s-ab12cd","kind":"text","delta":"..."}}
← {"jsonrpc":"2.0","id":3,"result":{"finished":true,"usage":{...}}}
```

### `session/stop`

Cleanly end a session.

```json
→ {"jsonrpc":"2.0","id":4,"method":"session/stop","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","id":4,"result":{"stopped":true}}
```

### `session/update` (notification from server)

Streaming events. `kind` is one of `text`, `reasoning`, `tool_call`, `tool_result`, `final`.

## Configuration

```toml
[acp]
enabled = true                    # must be on for `zeroclaw acp` to serve
max_sessions = 10
session_timeout_secs = 3600       # kill idle sessions after 1 hour
default_model = "haiku"           # referenced in `initialize` response; omit to leave client-controlled
```

If `default_model` is unset, the `initialize` capabilities omit the field entirely — ACP clients are expected to handle the absence by prompting the user.

## Running

Two modes:

**As a subprocess (typical IDE integration):**

```bash
zeroclaw acp
```

The binary reads stdin, writes stdout, exits on EOF.

**As a background companion to the service:**

The running `zeroclaw service` process exposes ACP on the gateway's WebSocket. Point your ACP client at `ws://localhost:42617/acp`.

## Client libraries

- **Reference implementation** — `examples/acp-client/` in the repo (Rust)
- **TypeScript** — planned; see the tracking RFC

## Security

ACP inherits the running config's autonomy level. If the binary is configured `Supervised`, session prompts still trigger operator approval for medium-risk tool calls — delivered to the ACP client as a `session/update` with `kind: "approval_request"` that the client must acknowledge.

In `Full` (or YOLO) mode, tool calls execute without approval.

## Code reference

- Server: `crates/zeroclaw-channels/src/orchestrator/acp_server.rs`
- Capability declaration: `capabilities` object in the `initialize` handler
- Session handling: `session_manager` in the same module

## See also

- [Channels → Overview](./overview.md)
- [Tools → MCP](../tools/mcp.md) — the "tool provider" direction (clients providing tools to the agent); ACP is the inverse (clients driving the agent)
- [Security → Overview](../security/overview.md)
