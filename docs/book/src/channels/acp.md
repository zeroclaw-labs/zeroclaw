# ACP — Agent Client Protocol

**ACP** is a JSON-RPC 2.0 protocol over stdio that lets editors and IDEs drive a running ZeroClaw agent as a session host. Newline-delimited JSON — lightweight, streamable, easy to wire to a subprocess.

Think of it as "LSP for agents": the editor launches `zeroclaw acp`, sends prompts over stdin, and receives session updates on stdout.

## What you'd use it for

- An editor extension that offers an "ask the agent about this file" command
- A terminal multiplexer integration that opens a side pane with an agent session
- A CI runner that drives the agent programmatically without a full gateway setup
- Anything that wants agent sessions without HTTP and without binding a port

## Protocol shape — v1

All messages are JSON-RPC 2.0 (newline-delimited). ZeroClaw implements **protocol version 1**.

### `initialize`

Handshake. Returns server capabilities.

```json
→ {"jsonrpc":"2.0","id":1,"method":"initialize"}
← {"jsonrpc":"2.0","id":1,"result":{
    "protocolVersion": 1,
    "agentCapabilities": {
      "loadSession": true,
      "promptCapabilities": {"image": false, "audio": false, "embeddedContext": false},
      "mcpCapabilities": {"http": false, "sse": false},
      "sessionCapabilities": {"resume": {}, "close": {}}
    },
    "agentInfo": {
      "name": "zeroclaw-acp",
      "title": "ZeroClaw ACP",
      "version": "0.7.x"
    },
    "authMethods": [],
    "_meta": {
      "zeroclaw": {
        "defaultModel": "anthropic/claude-sonnet-4.6",
        "maxSessions": 10,
        "sessionTimeoutSecs": 3600
      }
    }
  }}
```

`loadSession: true` and `sessionCapabilities: {"resume": {}, "close": {}}` indicate that session persistence is active. If the SQLite store could not be opened at startup, all three are absent or false and `session/load`, `session/resume`, and `session/close` will return `SESSION_NOT_FOUND` errors.

`_meta.zeroclaw` carries ZeroClaw-specific extension fields not in the base ACP spec. Clients that only implement the base spec can ignore this object.

The server always responds `protocolVersion: 1`. If you send a client-side `protocolVersion: 0`, you still get `1` back — v0 clients will see parse errors on the new message shapes; see [version compatibility](#version-compatibility) below.

### `session/new`

Open an isolated agent session. The optional `cwd` parameter (aliases: `workspaceDir`, `workspace_dir`) pins the per-session file-access boundary — it becomes the `workspace_dir` inside the `SecurityPolicy` that all file tools enforce. The agent's persistent data directory (memory, identity, cron) remains the daemon-level `workspace_dir` from config.

```json
→ {"jsonrpc":"2.0","id":2,"method":"session/new","params":{
    "cwd": "/path/to/project"
  }}
← {"jsonrpc":"2.0","id":2,"result":{
    "sessionId": "s-ab12cd",
    "workspaceDir": "/path/to/project"
  }}
```

`cwd` is canonicalized on intake — `../` traversal cannot escape the intended root. If `cwd` is omitted, the server uses the daemon's launch directory.

### `session/prompt`

Send a prompt. The response is a sequence of `session/update` notifications streaming back, terminated by the `session/prompt` result.

```json
→ {"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{
    "sessionId": "s-ab12cd",
    "prompt": "Summarise the changes in the last commit."
  }}
← {"jsonrpc":"2.0","method":"session/update","params":{
    "sessionId": "s-ab12cd",
    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type":"text","text":"The last commit..."}}
  }}
← {"jsonrpc":"2.0","method":"session/update","params":{
    "sessionId": "s-ab12cd",
    "update": {"sessionUpdate": "tool_call", "toolCallId": "tc-1", "title": "shell",
               "kind": "execute", "status": "pending", "rawInput": {...}}
  }}
← {"jsonrpc":"2.0","method":"session/update","params":{
    "sessionId": "s-ab12cd",
    "update": {"sessionUpdate": "tool_call_update", "toolCallId": "tc-1",
               "status": "finished", "rawOutput": "..."}
  }}
← {"jsonrpc":"2.0","id":3,"result":{
    "sessionId": "s-ab12cd",
    "stopReason": "end_turn",
    "content": "The last commit introduces..."
  }}
```

`stopReason` is `"end_turn"` on normal completion. The ACP completion signal is `stopReason`; ZeroClaw also includes the current final `content` string for existing clients.

### `session/update` notifications (agent → client)

ZeroClaw sends four kinds of `session/update` notification during a prompt turn. The discriminant is the `sessionUpdate` field inside `update`:

| `sessionUpdate` value | When emitted | Key fields |
|---|---|---|
| `agent_message_chunk` | Each streaming text token | `content.type = "text"`, `content.text` |
| `agent_thought_chunk` | Internal reasoning tokens (when enabled) | `content.type = "text"`, `content.text` |
| `tool_call` | Tool call initiated | `toolCallId`, `title`, `kind`, `status: "pending"`, `rawInput` |
| `tool_call_update` | Tool call completed | `toolCallId`, `status: "finished"`, `rawOutput`, `content[]` |

`toolCallId` on `tool_call` and `tool_call_update` are stable and correlated — the update completing a call carries the same `toolCallId` as the one that opened it.

The `name` field on `tool_call_update` is a ZeroClaw extension (not required by the base ACP spec). Clients can use it for display; it's safe to ignore.

### `session/request_permission` (agent → client, outbound request)

When a tool requires user approval (via `always_ask` in the autonomy config, or the `ask_user`/`escalate_to_human` tools), ZeroClaw issues a **JSON-RPC request** from agent to client. The client must reply with a result before the tool call proceeds.

```json
← {"jsonrpc":"2.0","id":"zc-out-0","method":"session/request_permission","params":{
    "sessionId": "s-ab12cd",
    "options": [
      {"optionId": "allow-once",  "name": "Allow once",  "kind": "allow_once"},
      {"optionId": "allow-always","name": "Always allow","kind": "allow_always"},
      {"optionId": "reject-once", "name": "Reject",      "kind": "reject_once"}
    ],
    "toolCall": {
      "toolCallId": "approval-...",
      "title": "Approve shell?",
      "kind": "execute",
      "status": "pending",
      "rawInput": {"tool": "shell", "summary": "git status --short"},
      "content": [{"type": "content", "content": {"type": "text", "text": "git status --short"}}]
    }
  }}
→ {"jsonrpc":"2.0","id":"zc-out-0","result":{
    "outcome": {"outcome": "selected", "optionId": "allow-once"}
  }}
```

The server-issued id (`"zc-out-N"`) is always a string prefixed `zc-out-` — disjoint from any integer or string ids the client uses for its own requests.

Response shape:
- `{"outcome": {"outcome": "selected", "optionId": "<id>"}}` — user picked an option
- `{"outcome": {"outcome": "cancelled"}}` — user dismissed the prompt

If the client never replies (crash, network drop, user closes IDE), the request times out after `sessionTimeoutSecs` and the tool call is denied.

`ask_user` uses the same `session/request_permission` mechanism, mapping the question's `choices` to permission options. Free-form (no-choices) `ask_user` is not supported until the [ACP elicitation RFD](https://github.com/zed-industries/agent-client-protocol/blob/main/docs/rfds/elicitation.mdx) lands. Calling `ask_user` without `choices` on an ACP session fast-fails with a clear error.

### `session/cancel` _(ZeroClaw extension)_

Abort an in-flight `session/prompt` turn. This method is a ZeroClaw extension,
not part of the base ACP spec. If ACP later standardizes a conflicting
`session/cancel`, ZeroClaw will move its extension to `_meta/session/cancel`.

**Cancel vs. stop:** `session/cancel` aborts an in-flight prompt turn and returns `stopReason: "cancelled"` with any streamed text accumulated up to the interrupt point. `session/stop` gracefully ends the session after the current turn completes — it waits for the turn to finish rather than interrupting it.

The canonical parameter is `sessionId`; `session_id` is accepted as a compatibility alias.

```json
→ {"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","method":"session/update","params":{
    "sessionId": "s-ab12cd",
    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type":"text","text":"partial..."}}
  }}
← {"jsonrpc":"2.0","id":3,"result":{
    "sessionId": "s-ab12cd",
    "stopReason": "cancelled",
    "content": "partial...\n\n[interrupted by user]"
  }}
```

Only one `session/prompt` may be active for a session at a time. A second prompt for the same session is rejected until the active turn completes or is cancelled.

If no turn is active for the session, the cancel is a noop — it succeeds silently without error. This follows ACP notification semantics: notifications must not produce errors.

### `session/stop` _(ZeroClaw extension)_

Cleanly end a session. Not in the base ACP spec — ZeroClaw-specific. If a future ACP spec revision adds `session/stop` with different semantics, this will be renamed `_meta/session/stop`.

```json
→ {"jsonrpc":"2.0","id":4,"method":"session/stop","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","id":4,"result":{"stopped":true}}
```

### `session/update` (client → server) _(ZeroClaw extension)_

ZeroClaw also accepts inbound `session/update` (and the legacy `session/event` alias) notifications from the client for custom event injection. Not in the base ACP spec — ZeroClaw-specific. If the ACP spec later defines an inbound `session/update` with different semantics, this will be renamed `_meta/session/update`.

## Session persistence

ZeroClaw automatically persists ACP sessions to SQLite. No configuration is required — the store opens at `<workspace_dir>/sessions/acp-sessions.db` whenever `zeroclaw acp` starts or a gateway WebSocket ACP connection is accepted. If the file cannot be created (read-only filesystem, bad permissions), the server falls back to in-memory-only sessions and `loadSession` reports `false` in the `initialize` response.

What is persisted:

- Session metadata: `sessionId`, `workspaceDir`, `created_at`, `last_activity`
- Full conversation history: every `ConversationMessage` written after each completed `session/prompt` turn, in one atomic transaction per turn

Sessions survive process restarts. A session created in one `zeroclaw acp` invocation can be loaded or resumed in a later one, as long as the same `workspace_dir` is in use (and therefore the same `acp-sessions.db` file).

Sessions are not automatically deleted. Use `session/close` to deactivate a session without deleting it, then `session/load` or `session/resume` to bring it back.

### `session/load` _(ZeroClaw extension)_

Restore a previously persisted session with **full history replay**. The server seeds the agent with the stored conversation history, then streams that history back to the client as a sequence of `session/update` notifications before returning. The client receives the same update stream it would have seen had the session never ended.

```json
→ {"jsonrpc":"2.0","id":5,"method":"session/load","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","method":"session/update","params":{
    "sessionId": "s-ab12cd",
    "update": {"sessionUpdate": "agent_message_chunk", "content": {"type":"text","text":"The last commit..."}}
  }}
← ... (remaining stored messages replayed as session/update notifications)
← {"jsonrpc":"2.0","id":5,"result":{}}
```

After `session/load` returns, the session is active and ready to accept `session/prompt` calls.

`session_id` is accepted as a snake_case alias for `sessionId`.

Errors:

| Code | Meaning |
|---|---|
| `-32000` `SESSION_NOT_FOUND` | No record exists for the given `sessionId` in the store |
| `-32001` `SESSION_LIMIT_REACHED` | `max_sessions` active sessions already in flight |
| `-32602` `INVALID_PARAMS` | Session is already active — call `session/close` first |
| `-32603` `INTERNAL_ERROR` | SQLite read failure |

### `session/resume` _(ZeroClaw extension)_

Restore a previously persisted session **without history replay**. The agent is seeded with the stored conversation history so it has full context for the next turn, but no `session/update` notifications are emitted. Use this when the client already has the history from a previous connection and only needs the agent state restored.

```json
→ {"jsonrpc":"2.0","id":5,"method":"session/resume","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","id":5,"result":{}}
```

After `session/resume` returns, the session is active and ready to accept `session/prompt` calls. Same errors as `session/load`.

**Load vs. resume:** use `session/load` when reconnecting after an unexpected disconnect and the client needs to rebuild its UI from the stored history. Use `session/resume` when the client already has the history (e.g., it stored it locally) and only needs the server-side agent state restored.

### `session/close` _(ZeroClaw extension)_

Deactivate an active session: cancels any in-flight turn, removes the session from the in-memory active set, and unregisters the ACP back-channel. The session record in the SQLite store is **not deleted** — the session can still be restored with `session/load` or `session/resume` later.

```json
→ {"jsonrpc":"2.0","id":6,"method":"session/close","params":{"sessionId":"s-ab12cd"}}
← {"jsonrpc":"2.0","id":6,"result":{}}
```

`session_id` is accepted as a snake_case alias for `sessionId`.

Returns `SESSION_NOT_FOUND` (`-32000`) if the session is not currently active (it may still exist in the store).

**Close vs. stop:** `session/close` deactivates the session while preserving its persistent record for later reload. `session/stop` also removes the session from memory but has the same effect on the store. Neither deletes the SQLite record.

## Configuration

```toml
[acp]
max_sessions = 10
session_timeout_secs = 3600       # idle sessions killed after 1 hour
```

When running `zeroclaw acp` as a subprocess, the command starts the server unconditionally. When running as a daemon, the gateway exposes ACP over WebSocket at `/acp` with no additional config required.

## Running

**As a subprocess (typical IDE integration):**

```bash
zeroclaw acp
```

The binary reads stdin, writes stdout, exits on EOF.

**As a WebSocket service (daemon mode):**

Start the daemon normally. The gateway always exposes ACP over WebSocket at the `/acp` endpoint — no extra config flag is required.

## Version compatibility

ACP v0 clients (using the flat `{streaming, maxSessions, ...}` initialize response and `kind: "text"|"tool_call"` session/update shape) will see deserialization errors on connecting to a v1 server. The discriminants and envelope shapes changed in a breaking way. Upgrade steps:

- Use `sessionUpdate` (not `kind`) to discriminate `session/update` notifications.
- Parse `session/prompt` results as `{sessionId, stopReason, content}` (not `{finished, usage}`).
- Implement `session/request_permission` response handling — the approval mechanism moved from a server notification to a client-answered RPC.
- Drop the `systemPrompt` param from `session/new` — it is not read.

## Security

ACP inherits the running config's autonomy level. When `[autonomy] level = "supervised"`, medium-risk tool calls trigger approval via the ACP back-channel — a `session/request_permission` outbound request the client must acknowledge. In `full` mode, tool calls execute without approval and `workspace_only` is implicitly disabled (the agent can reach paths outside the session cwd); `forbidden_paths` still apply.

The `cwd` from `session/new` becomes the `SecurityPolicy` workspace boundary used by all file and shell tools for that session. Note: the agent's system prompt currently reflects the daemon's global `workspace_dir` rather than the session `cwd` — this does not affect enforcement, only the directory the model believes it is working in.

## Code reference

- ACP server: `crates/zeroclaw-channels/src/orchestrator/acp_server.rs`
- ACP back-channel: `crates/zeroclaw-channels/src/acp_channel.rs`
- Session store (SQLite): `crates/zeroclaw-infra/src/acp_session_store.rs`
- Gateway ACP-over-WebSocket endpoint: `crates/zeroclaw-gateway/src/acp.rs`
- Per-session path enforcement: `crates/zeroclaw-config/src/policy.rs` (`SecurityPolicy::from_config`), `crates/zeroclaw-runtime/src/agent/agent.rs` (`from_config_with_session_cwd_and_mcp`)
- OS-level sandbox detection/backends: `crates/zeroclaw-runtime/src/security/detect.rs`, `landlock.rs`, `bubblewrap.rs`, `seatbelt.rs`

## See also

- [Channels → Overview](./overview.md)
- [Tools → MCP](../tools/mcp.md) — clients providing tools to the agent; ACP is the inverse
- [Security → Autonomy](../security/autonomy.md)
- [Security → Overview](../security/overview.md)
