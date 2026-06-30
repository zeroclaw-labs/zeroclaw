# RPC Socket Transport

The daemon exposes a JSON-RPC 2.0 interface over a local IPC stream, a Unix
domain socket on Unix and a named pipe on Windows. This is the primary
transport for local clients like zerocode. The HTTP/WS gateway remains for
webhooks, the web dashboard, and remote REST consumers.

## Endpoint resolution

Each data directory gets its own endpoint, so multiple daemon instances on the
same machine do not collide. The data dir is derived from the config dir
(`--config-dir` / `ZEROCLAW_CONFIG_DIR`, or `ZEROCLAW_DATA_DIR`).

| OS | Default endpoint |
|---|---|
| Linux | `<data_dir>/daemon.sock` (Unix domain socket) |
| macOS | `<data_dir>/daemon.sock` (Unix domain socket) |
| Windows | `\\.\pipe\zeroclaw-<hash>` where `<hash>` is derived from `data_dir` |

Override with the `ZEROCLAW_SOCKET` environment variable on either platform:

<div class="os-tabs-src">

#### sh

```sh
export ZEROCLAW_SOCKET=/tmp/my-zeroclaw.sock
zeroclaw daemon
```

#### PowerShell

```powershell
$env:ZEROCLAW_SOCKET = '\\.\pipe\my-zeroclaw'
zeroclaw daemon
```

</div>

## Wire protocol

NDJSON (newline-delimited JSON). Each line is a complete JSON-RPC 2.0 message.
No HTTP framing, no length prefix. The framing is identical across platforms;
named pipes carry the same byte stream as Unix sockets.

```
{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":1},"id":1}\n
{"jsonrpc":"2.0","result":{"protocolVersion":1,"serverVersion":"0.8.2"},"id":1}\n
```

## Handshake

The first RPC call must be `initialize`. The daemon rejects all other methods
until `initialize` succeeds. Protocol version mismatch produces a structured
error with code `-32011`.

```json
{
  "jsonrpc": "2.0",
  "method": "initialize",
  "params": {
    "protocolVersion": 1
  },
  "id": 1
}
```

The endpoint does not require a pairing token. Access control is handled by
the operating system:

- Unix: socket is `0o600`, parent directory is `0o700`.
- Windows: named pipe ACL defaults to the creating user and `SYSTEM`.

## Methods

| Method | Direction | Description |
|---|---|---|
| `initialize` | client -> daemon | Authenticate and negotiate protocol version |
| `session/new` | client -> daemon | Create an agent session (requires `agentAlias`, optional `cwd`, `sessionId`) |
| `session/close` | client -> daemon | Close and clean up a session |
| `session/prompt` | client -> daemon | Run a turn (streamed via `session/update` notifications) |
| `session/cancel` | client -> daemon | Cancel an in-flight turn |
| `status` | client -> daemon | Server version, protocol version, active session list |
| `session/update` | daemon -> client | Streaming notification during a turn (text chunks, tool calls, approvals) |

### Turn streaming

`session/prompt` returns the final result when the turn completes. During
execution, the daemon sends `session/update` notifications with incremental
events:

```json
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"...","type":"agent_message_chunk","text":"Hello"}}
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"...","type":"tool_call","toolCallId":"tc_1","name":"bash","rawInput":{...}}}
{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"...","type":"tool_result","toolCallId":"tc_1","name":"bash","rawOutput":"..."}}
```

Event types: `agent_message_chunk`, `agent_thought_chunk`, `tool_call`,
`tool_result`, `approval_request`.

## Ephemeral mode

`zeroclaw daemon --ephemeral` tracks connected clients and self-terminates
when the last one disconnects (after a 1-second grace period). A reconnect
during the grace period cancels the shutdown. The daemon will not exit until
at least one client has connected.

Daemons started without `--ephemeral` ignore client count and run until
explicitly stopped.

## Security

- Unix socket directory: `0o700` (owner only)
- Unix socket file: `0o600` (owner only)
- Windows named pipe: default ACL grants the creating user and `SYSTEM`
- `SO_PEERCRED` on Linux provides the connecting process PID and UID for
  audit logging; Windows logs `pipe:local` as the peer label

## Quick test

Start the daemon in one terminal:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon
```

</div>

In a second terminal on Unix, connect with `socat`:

<div class="os-tabs-src">

#### sh

```sh
socat READLINE UNIX-CONNECT:~/.zeroclaw/data/daemon.sock
```

</div>

Paste lines one at a time:

```
{"jsonrpc":"2.0","method":"initialize","params":{"protocolVersion":1},"id":1}
{"jsonrpc":"2.0","method":"status","params":{},"id":2}
```

On Windows, use any named-pipe client (PowerShell `[System.IO.Pipes.NamedPipeClientStream]`,
`nc` via WSL, or just run `zerocode`).

## Internals

The dispatch layer lives in `crates/zeroclaw-runtime/src/rpc/`:

| File | Role |
|---|---|
| `transport.rs` | `RpcTransport` trait |
| `turn.rs` | `execute_turn()` shared turn executor |
| `session.rs` | `RpcSession`, `SessionStore` |
| `dispatch.rs` | `RpcDispatcher` method routing |
| `local.rs` | `LocalTransport` + listener (Unix socket / Windows named pipe) |
| `wss.rs` | WSS (WebSocket Secure) transport + TLS acceptor |
| `attachments.rs` | File upload processing, dedup, marker generation |

The `RpcTransport` trait is designed so that additional transports (vsock,
custom IPC) slot in without touching the dispatch or session logic. The
`local.rs` module wraps the Unix and Windows primitives behind a single
`LocalTransport` struct using `tokio::io::split`, so the read/write loop is
shared across both platforms.
