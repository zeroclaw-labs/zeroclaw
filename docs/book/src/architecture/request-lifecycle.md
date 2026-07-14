# Request Lifecycle

What happens between "user sends a message" and "agent replies": the full path, with streaming, tool calls, and security gates annotated.

## Inbound

```mermaid
flowchart LR
    A[External event] -->|webhook / push / poll / WS| B[Channel adapter]
    B -->|decode, dedup, pair-check| C[Inbound envelope]
    C -->|workspace binding| D[Runtime: process_message]
```

A channel adapter (e.g. `discord.rs`, `telegram.rs`, `email_channel.rs`) receives platform-native events and converts them into a uniform inbound envelope. The adapter handles:

- **Decoding**: platform-specific payload → canonical message format
- **Deduplication**: prevents replaying the same message twice (restarts, retries)
- **Pair-check**: enforces the `[channels.<name>.allowed_users]` / IAM policy before the event reaches the runtime

If the channel is not paired or the user isn't allowed, the event is dropped before the runtime sees it.

## Agent loop

```mermaid
sequenceDiagram
    participant CH as Channel
    participant RT as Runtime
    participant SEC as Security
    participant MEM as Memory / history
    participant PR as Provider
    participant TL as Tool

    CH->>RT: process_message(envelope)
    Note over RT: resolve memory-inject policy from the turn's TurnOrigin
    RT->>MEM: recall(query, session scopes)
    MEM-->>RT: entries
    Note over RT: render [Memory context] preamble (engine-side)
    RT->>PR: chat(system, history, tools)
    loop Streaming
        PR-->>RT: StreamEvent::TextDelta
        RT-->>CH: draft update (if channel supports it)
    end
    PR-->>RT: StreamEvent::ToolCall(args)
    RT->>SEC: evaluate_tool_access(name, args, risk)
    alt Blocked
        SEC-->>RT: Err(reason)
        RT->>PR: chat(..., + tool_error)
    else Approval required
        SEC->>CH: ask_operator(prompt)
        CH-->>SEC: approved / denied
    else Allowed
        SEC-->>RT: Ok
    end
    RT->>TL: invoke(args)
    TL-->>RT: ToolResult
    RT->>MEM: append to turn/session history
    RT->>PR: chat(..., + tool_result)
    PR-->>RT: StreamEvent::TextDelta (final)
    RT-->>CH: reply(final)
    RT->>MEM: persist conversation/session history
```

Key properties:

- **Streaming is end-to-end.** The provider streams tokens. If the channel adapter reports `supports_draft_updates()`, the runtime edits a sent message in place as text arrives. Discord, Slack, and Telegram support this.
- **Tool calls are mid-stream.** The model can emit a tool call while still generating text. The runtime reads the stream to completion, flushing visible text as it arrives, then recovers the tool calls (from structured `ToolCall` events or by parsing the accumulated response), validates, invokes, feeds the result back, and starts a fresh stream for the next turn.
- **Security gates every tool call.** `evaluate_tool_access` consults the [autonomy level](../security/autonomy.md), allow/deny lists, and path boundaries. Medium-risk calls under `Supervised` autonomy go to the operator-approval path.
- **Memory context is engine-injected.** Before the first provider call, the turn engine resolves an injection policy from the turn's `TurnOrigin` (who initiated the turn): nested sub-turns never inject, scheduled origins (cron, daemon) inject with conversation-category entries excluded, and user-facing origins inject (excluding conversation entries when the turn has no session scope). A spawn site can suppress injection for any origin (for example a cron job with `uses_memory = false`), and turns that carry no memory backend skip it entirely. A single renderer applies time decay, relevance filtering, a prompt-poisoning skip set, and budget caps uniformly on every path; memory backends only answer `recall`, they do not format context.
- **History and memory are separate.** Session history preserves conversation, tool-call, and tool-result continuity. Explicit memory writes persist selected entries in the memory backend. Receipts ride in-band in the conversation text rather than as a separate persisted artifact. For payload ownership details, see [Memory and payload lifecycle](./memory-payload-lifecycle.md).

## Tool receipts

Successful tool executions can receive an HMAC-SHA256 receipt that is appended to the tool-result text and passed back to the model in the conversation, proving the signed result came from the runtime. The HMAC is keyed by an ephemeral in-memory key and computed over `tool_name || args || result || timestamp`. Receipts are not written to a separate on-disk log and are not chained; the model can echo them but cannot forge a new valid one without the key. See [Tool receipts](../security/tool-receipts.md).

## Outbound

Outbound messages go back through the same channel adapter. Adapters with multi-message support (Discord, Slack) can stream long replies as a sequence of messages; others (email, SMS) flush on stream completion.

## Where it lives in code

- Agent loop: `crates/zeroclaw-runtime/src/agent/turn/` (`run_tool_call_loop`), with entry points in `crates/zeroclaw-runtime/src/agent/loop_.rs` (`process_message`, `run`)
- Memory-context injection: `crates/zeroclaw-runtime/src/agent/memory_inject.rs` (`resolve_inject_policy`, `render_memory_context`), keyed on `TurnOrigin` from `zeroclaw-api`'s ingress types and invoked by the turn engine
- Tool-call access checks: `crates/zeroclaw-runtime/src/security/` (`iam_policy.rs` `evaluate_tool_access`)
- Channel orchestration: `crates/zeroclaw-channels/src/orchestrator/`
- Provider streaming: `crates/zeroclaw-api/src/model_provider.rs` (`StreamEvent` enum, re-exported from `zeroclaw-providers`), `compatible.rs` (SSE parser)

Since #7415, every transport (channels, CLI, cron, gateway WebSocket, RPC/zerocode, ACP, and the embedded `Agent` API) runs the same turn engine: `run_tool_call_loop` in `crates/zeroclaw-runtime/src/agent/turn/`. The streaming and embedded entry points are thin wrappers in `agent.rs` that set per-caller knobs (dedup, iteration-cap behavior, event emission) around the shared loop. The `turn/` module is one file per step:

| File(s) | Step |
|---|---|
| `mod.rs` | orchestrator: iteration control, knobs, steering drain |
| `history_window.rs` · `tool_specs.rs` · `vision_route.rs` | pre-call: history maintenance, tool specs, vision routing |
| `provider_call.rs` · `stream_consume.rs` · `stream_guard.rs` | the LLM call, stream consumption, mid-stream protocol guarding |
| `parse_response.rs` · `protocol_detect.rs` · `context_recovery.rs` | response interpretation, parse-issue detection, overflow recovery |
| `approval_gate.rs` · `call_prep.rs` | tool-call approval and preparation (dedup, hooks, delivery defaults) |
| `post_exec.rs` · `results_collect.rs` · `history_append.rs` · `max_iter.rs` | result recording, loop detection, history append, iteration cap |
| `context.rs` · `events.rs` · `knobs.rs` · `steering.rs` · `outcome.rs` · `redact.rs` · `delivery_defaults.rs` | shared types: turn context, events, per-caller knobs, steering, outcomes, credential scrubbing |
