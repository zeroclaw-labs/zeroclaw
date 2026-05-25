# Streaming

Every provider in ZeroClaw that speaks a streaming API streams token-by-token. The runtime forwards those streams to channel adapters that support partial updates (Discord, Slack, Telegram, the gateway's WebSocket), so the user sees text appear as the model generates it.

## What gets streamed

The provider trait emits `StreamEvent` values:

| Event | When |
|---|---|
| `TextDelta(String)` | New tokens of assistant text |
| `ReasoningDelta(String)` | Reasoning / chain-of-thought tokens (o-series, DeepSeek-R1, Qwen-thinking) |
| `ToolCall { name, args }` | The model has decided to call a tool |
| `PreExecutedToolCall` | A provider-side pre-executed tool call (e.g. Gemini grounded search) |
| `PreExecutedToolResult` | Its result |
| `Final { usage }` | Stream complete; token-usage totals |

Channels consume these events via the `Channel` trait's outbound stream hook.

## Capability flags

A provider exposes two flags so the runtime knows what it can expect:

```rust
fn supports_streaming(&self) -> bool { true }
fn supports_streaming_tool_events(&self) -> bool { true }
```

- **`supports_streaming`** — true for every actively maintained provider
- **`supports_streaming_tool_events`** — true when the provider emits `ToolCall` events during the stream rather than at the end

OpenAI-compatible providers differ: some stream tool-call arg deltas chunk-by-chunk, others only emit the call once complete. The `compatible.rs` SSE parser handles both.

## Channel-side streaming

Channels advertise their own streaming capabilities:

```rust
fn supports_draft_updates(&self) -> bool;           // edit a message in place
fn supports_multi_message_streaming(&self) -> bool; // split one reply into many messages
```

| Channel | Draft updates | Multi-message |
|---|:---:|:---:|
| CLI | ✓ | — |
| Discord | ✓ | ✓ |
| Slack | ✓ | ✓ |
| Telegram | ✓ | partial |
| Matrix | ✓ | — |
| Mattermost | ✓ | — |
| Email | — | — |
| SMS / voice | — | — |
| Gateway (WebSocket) | ✓ | ✓ |

When both the provider and the channel support streaming, the flow is: provider emits `TextDelta` → runtime passes to channel → channel edits the sent message. The edit cadence is bounded by `draft_update_interval_ms` in the channel config (default: 500 ms) to avoid rate-limiting.

## Reasoning blocks

Reasoning models (OpenAI o-series, DeepSeek-R1, Qwen-thinking variants) emit `ReasoningDelta` events separate from regular text. By default the runtime strips these from outbound streams — see `<think>…</think>` handling in `crates/zeroclaw-channels/src/orchestrator/mod.rs`. Users see the final answer, not the chain-of-thought.

To surface reasoning to the user:

```toml
[channels.<name>]
show_reasoning = true
```

This is off by default because reasoning content is (a) often verbose and (b) sometimes reveals internal deliberation that looks confusing to an end user.

Disabling reasoning entirely on a reasoning-capable model:

```toml
[providers.models.<name>]
think = false
reasoning_effort = "none"
```

Both fields are top-level; the right name depends on the provider/endpoint. Setting both covers Ollama native, Ollama OpenAI-compat, and upstream APIs that honour `reasoning_effort`.

## Tool calls mid-stream

When a model decides to call a tool, the provider emits `ToolCall`. The runtime:

1. Pauses reading from the provider's stream
2. Flushes any buffered text to the channel
3. Runs the tool (subject to security validation — see [Security → Overview](../security/overview.md))
4. Resumes the conversation with the tool result appended
5. Opens a new streaming call to the provider for the next assistant turn

From the user's perspective: text, then a visible indicator that the agent ran a tool (via channel-specific hints), then more text. For channels without typing indicators, the gap between the tool call and the next text chunk is the only signal.

## Non-streaming providers

If a provider returns the entire response in one shot (older OpenAI-compat endpoints, legacy Gemini), the runtime synthesises a single `TextDelta` containing the full reply followed by `Final`. Channel adapters still work — they just don't see partials.

## Code references

- `crates/zeroclaw-api/src/provider.rs` — `Provider` trait, `StreamEvent` enum
- `crates/zeroclaw-providers/src/compatible.rs` — OpenAI-compat SSE parser
- `crates/zeroclaw-providers/src/anthropic.rs` — Anthropic streaming
- `crates/zeroclaw-providers/src/ollama.rs` — Ollama streaming
- `crates/zeroclaw-channels/src/orchestrator/mod.rs` — channel-side stream consumption
