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
{"jsonrpc":"2.0","result":{"protocolVersion":1,"serverVersion":"0.8.5"},"id":1}\n
```

## Connection limits

The local endpoint bounds what one client can hold:

| Limit | Value | What the client sees |
|---|---|---|
| Frame size | 8 MiB per line | One `-32600` error with `id: null` and `data: {"reason": "frame_too_large", "limit_bytes": 8388608}`, then end of stream. The rest of the oversized line is never read, so the connection cannot continue. |
| Initialize | 30 s from connect | A connection that has not completed `initialize` 30 s after it connected is closed with no reply, so a client that connects and sends nothing cannot hold a connection slot. The deadline covers authentication too: an `initialize` sent in time whose authentication is still running when it passes, such as a slow OIDC check, is closed rather than accepted late. |
| Unfinished frame | 30 s from the frame's first byte | Once a frame starts, the whole line must arrive within 30 s of its first byte, however steadily the rest arrives. Past that the client receives one `-32600` error with `id: null` and `data: {"reason": "frame_timeout", "limit_ms": 30000}`, then end of stream. The wait between frames is not bounded, so an initialized client may stay idle. |
| Write stall | 30 s per frame | A client that stops reading for 30 s while the daemon has output queued for it is disconnected. Other connections are unaffected. Disconnecting ends the turns that connection started, as any other disconnect does. That includes a suspended client: a zerocode stopped with Ctrl-Z, or frozen with Ctrl-S, while a turn streams has that turn cancelled after 30 s, where before it finished once the client resumed. |
| Open connections | `rpc.max_local_connections`, default 512 | A connection past the ceiling receives one `-32004` error with `id: null` and `data: {"reason": "connection_limit", "limit": N}`, then end of stream. The daemon logs one warning when it starts refusing. The value is read when the listener starts. |

Payloads larger than a frame travel as a chunked upload rather than in one
line. See [Chunked uploads](#chunked-uploads).

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
| `session/new` | client -> daemon | Create an agent session (requires `agentAlias`, optional `cwd`, `sessionId`; an ID that is already live rebinds the caller to that canonical in-memory session instead of replacing its agent history, and is refused with `FORBIDDEN` when the forwarded environment the caller may use under its current grants differs from the one that session was created with; optional `keep_siblings` suppresses the idle same-mode sibling eviction for multi-session clients that manage sibling lifecycle themselves) |
| `session/close` | client -> daemon | Remove the live session owner; ACP durable history remains resumable |
| `session/kill` | client -> daemon | Remove the live session and tombstone its ACP durable history |
| `session/delete` | client -> daemon | Remove the live session and its selected durable history |
| `session/prompt` | client -> daemon | Run a turn (streamed via `session/update` notifications) |
| `session/cancel` | client -> daemon | Cancel an in-flight turn |
| `session/state` | client -> daemon | Read live session lifecycle state, active turn identity, and the optional current plan; active or queued work is represented by `state: "running"` so recovery clients can confirm terminal status before releasing retained work |
| `status` | client -> daemon | Server version, protocol version, active session list |
| `file/upload/begin`, `file/upload/chunk`, `file/upload/commit` | client -> daemon | Upload one file in ordered chunks; see [Chunked uploads](#chunked-uploads) |
| `session/update` | daemon -> client | Streaming notification during a turn (text chunks, tool calls, approvals) |
| `elicitation/create` | daemon -> client | Request interactive input for ask-user and poll flows |

### Atomic configuration writes

`config/set-many` accepts an ordered `sets` array containing 1–256 objects,
each with `prop` and `value`. It uses the same property syntax and value rules
as `config/set`. For example, after creating a permission profile named
`operator`, an administrator can create a complete user in one request:

```json
{"jsonrpc":"2.0","method":"config/set-many","params":{"sets":[{"prop":"users.example.uid","value":1001},{"prop":"users.example.permission_profiles","value":["operator"]}]},"id":2}
```

Success returns `{"props":["users.example.uid","users.example.permission_profiles"],"set":true}`.
All fields are staged on one candidate and validated before one persistent
commit. Later entries for the same property win. A staging or commit failure
leaves the persisted and live config unchanged; an invalid entry error names
its zero-based index.

The caller needs `Config:Update` and authorization for every requested path.
The daemon resolves current authority while holding the config write lock,
before staging any entry. If one path is forbidden, the entire batch is
refused with `FORBIDDEN`, including the entry index, even if earlier paths
were allowed. Revocation while waiting for that lock also refuses the batch.
Provider/model views are prepared from the complete candidate and installed
after the successful commit. Deletes and map-key operations are not part of
this method.

### Bidirectional requests

Either side may send a request on the established socket. The receiver must
answer with the same `id` and exactly one of `result` or `error`. Each peer uses
its own `zc-out-<number>` sequence for outbound requests; the prefix is not a
globally unique namespace. Correlation remains directional: each peer matches
responses only against its own pending-request map, so the same textual ID can
be in flight independently in opposite directions.

```json
{"jsonrpc":"2.0","method":"elicitation/create","params":{"message":"Continue?"},"id":"zc-out-0"}
{"jsonrpc":"2.0","result":{"action":"accept","content":{"answer":"yes"}},"id":"zc-out-0"}
```

An explicit `"result": null` is a successful response and still resolves the
pending caller. An `error` object resolves it as a failure. If the peer does not
answer, the initiating ask-user or poll operation retains its existing timeout
behavior.

Every frame must be a JSON object with `"jsonrpc": "2.0"`. Requests require a
string `method`; when present, `params` must be an object or array. Responses
require a string, numeric, or null `id` and exactly one response member. Invalid
JSON produces `-32700` (parse error). A malformed request-shaped envelope
produces `-32600` (invalid request), using its valid request ID when one can be
recovered. A malformed response-shaped envelope is logged without frame
contents and dropped without a reply, because echoing its ID could complete an
unrelated request in the opposite direction. Direction-ambiguous envelopes use
`id: null`. Unknown valid response IDs are likewise logged and ignored rather
than answered, preventing response loops.

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

### ACP durable lifecycle

Native RPC keeps the original visible transcript separate from the retained provider context. Automatic trimming does not delete or renumber original transcript rows. A trim notification is sent only after its retained-context snapshot and covered checkpoint boundary have committed together; a failed write suppresses that notification. The snapshot excludes runtime system prompts, recalled-memory injection, and hidden reasoning. On interruption, recovery appends visible checkpoint progress once while restoring the model from the latest retained snapshot plus later checkpoint events. An explicitly empty retained snapshot remains authoritative. Sessions without a snapshot keep the legacy provider-safe replay path.

ACP sessions can retain durable history before reaching a terminal state. When the in-memory owner is reaped, RPC prompt recovery considers only durable rows whose persisted `interaction_surface` is supported; an unsupported surface is left untouched so a later direct recovery can inspect the original checkpoint.

Recovery adds a client-visible interruption marker for an unfinished turn. Provider replay excludes that synthetic marker, and persisted tool output remains subject to the existing transcript bounds.

The lifecycle operations have distinct durable meanings:

- `session/close` may retain durable resumable history.
- `session/kill` retains the history but marks the row as tombstoned, so it is not eligible for runtime rehydration.
- `session/delete` removes the ACP row and its recoverable checkpoint before unregistering and removing the live session when the target is a live ACP session, or when no live mode exists and an ACP durable row is selected. A live same-ID Chat session remains isolated from ACP storage.

Kill and delete signal cancellation before attempting their fallible SQLite operation. If that operation fails for an idle target, the RPC reports an internal error and preserves the live owner and channel registration. If hard cancellation has already removed an active owner, the RPC cannot restore that live generation; the durable row and recoverable checkpoint remain available once storage is working again.

### Chunked uploads

`file/attach` carries a whole file in one frame, which caps it well below the
per-file limit once base64 is counted. `file/upload/begin`, `file/upload/chunk`,
and `file/upload/commit` carry the same upload in pieces and land it exactly as
`file/attach` does: in the session agent's workspace, under its content hash,
deduplicated per session, with the same marker in the result.

1. `file/upload/begin` with `session_id`, `size_bytes`, and optionally
   `filename` and `sha256` (hex). The result gives an `upload_id`,
   `chunk_bytes` (1 MiB), and `max_bytes` (the 10 MB per-file limit).
2. `file/upload/chunk` with `upload_id`, `offset`, and `data_b64`, in order.
   `offset` must equal the bytes received so far; the result reports
   `received_bytes`. Resending an accepted chunk with the same bytes is
   acknowledged unchanged, so a client can retry after a lost response.
3. `file/upload/commit` with `upload_id` once every declared byte has arrived.
   A declared `sha256` must match. The result is a `file/attach` file entry.

An upload belongs to the connection that began it: no other connection can
name it, and it is discarded when that connection closes. The limits:

- A connection may stage four uploads at a time.
- The daemon stages at most 256 MiB across all connections. The charge covers
  each upload's payload and the metadata it keeps (session id, owner, agent,
  filename, declared hash), so an empty payload cannot hold memory for free. A
  `filename` longer than 255 bytes is refused.
- An upload idle for five minutes is discarded. When a new upload does not
  fit in the budget, the daemon first reclaims every upload idle past that
  deadline, even one whose connection is still open, so a silent client
  cannot hold the budget.

The three methods are served on local connections only; a WSS peer gets
`-32012`. They need the `files:create` grant, and `begin` checks that the
principal owns the session (for a principal without `admin`) and may use the
session's agent.

An upload is bound to the exact session it was begun for: the live session
with that id, its owner, and its agent. `commit` stores it in one step under
the session lock. It first confirms that session is still the same one, then
checks the caller's credential, `files:create`, session ownership, and agent
entitlement against the policy in force at that moment, and only then writes
and indexes the file. A grant withdrawn while the upload was in progress, or
a session closed or recreated under the same id, fails the commit before
anything is written. The check and the write hold the accepted
authorization policy and the set of paired tokens still, so a policy change or
an unpairing that arrives after the check completes only once the file is
stored: the upload is ordered before it, never between the check and the
write. `file/attach` stores through the same step and carries
the same ownership requirement.

A repeat of an upload the session already indexed is checked against the file
on disk. If the file was edited, deleted, or replaced by a link, the uploaded
bytes are written back at the indexed path before it is returned.

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
| `upload.rs` | Chunked-upload staging: ordering, per-connection and process-wide bounds |

The `RpcTransport` trait is designed so that additional transports (vsock,
custom IPC) slot in without touching the dispatch or session logic. The
`local.rs` module wraps the Unix and Windows primitives behind a single
`LocalTransport` struct using `tokio::io::split`, so the read/write loop is
shared across both platforms.
