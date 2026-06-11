# Streaming

Every provider in ZeroClaw that speaks a streaming API streams token-by-token. The runtime forwards those streams to channel adapters that support partial updates (Discord, Slack, Telegram, the gateway's WebSocket), so the user sees text appear as the model generates it.

## What gets streamed

The provider trait emits `StreamEvent` values as the model generates output:
text deltas, structured tool calls, provider-side pre-executed tool calls and
their results, token-usage reports, and a final completion marker. The
authoritative, per-variant definitions live with the type in
`crates/zeroclaw-api/src/model_provider.rs` (`enum StreamEvent`); reasoning
tokens arrive as text deltas, not a separate variant.

Channels consume these events via the `Channel` trait's outbound stream hook.

## Capability flags

A provider exposes two flags so the runtime knows what it can expect:

```rust
fn supports_streaming(&self) -> bool { true }
fn supports_streaming_tool_events(&self) -> bool { true }
```

- **`supports_streaming`**: true for every actively maintained provider
- **`supports_streaming_tool_events`**: true when the provider emits `ToolCall` events during the stream rather than at the end

OpenAI-compatible providers differ: some stream tool-call arg deltas chunk-by-chunk, others only emit the call once complete. The `compatible.rs` SSE parser handles both.

## Channel-side streaming

Channels advertise their own streaming capabilities through the `Channel` trait:

```rust
fn supports_draft_updates(&self) -> bool;           // edit a message in place
fn supports_multi_message_streaming(&self) -> bool; // split one reply into many messages
```

A channel's capability follows from its config: a channel with the
`stream_mode` enum (off / partial / multi_message) supports both draft updates
and multi-message; a channel with the `stream_drafts` boolean supports draft
updates only. This table is generated from the channel config schema, so it
stays correct as channels gain or lose streaming support:

{{#channel-streaming-matrix}}

When both the provider and the channel support streaming, the flow is: provider emits `TextDelta` → runtime passes to channel → channel edits the sent message. The edit cadence is bounded by `draft_update_interval_ms` in the channel config (default: 500 ms) to avoid rate-limiting.

## Reasoning blocks

Reasoning models (OpenAI o-series, DeepSeek-R1, Qwen-thinking variants) emit `ReasoningDelta` events separate from regular text. By default the runtime strips these from outbound streams, see `<think>…</think>` handling in `crates/zeroclaw-channels/src/orchestrator/mod.rs`. Users see the final answer, not the chain-of-thought.

To surface reasoning to the user, enable it on the alias entry. This is off by default because reasoning content is (a) often verbose and (b) sometimes reveals internal deliberation that looks confusing to an end user.

To disable reasoning entirely on a reasoning-capable model, set the relevant reasoning field to off. Both fields are top-level; the right name depends on the provider/endpoint. Setting both covers Ollama native, Ollama OpenAI-compat, and upstream APIs that honour `reasoning_effort`.

## Tool calls mid-stream

When a model decides to call a tool, the provider emits `ToolCall`. The runtime:

1. Pauses reading from the provider's stream
2. Flushes any buffered text to the channel
3. Runs the tool (subject to security validation, see [Security → Overview](../security/overview.md))
4. Resumes the conversation with the tool result appended
5. Opens a new streaming call to the provider for the next assistant turn

From the user's perspective: text, then a visible indicator that the agent ran a tool (via channel-specific hints), then more text. For channels without typing indicators, the gap between the tool call and the next text chunk is the only signal.

## Non-streaming providers

If a provider returns the entire response in one shot (older OpenAI-compat endpoints, legacy Gemini), the runtime synthesises a single `TextDelta` containing the full reply followed by `Final`. Channel adapters still work; they just don't see partials.

## Code references

- `crates/zeroclaw-api/src/model_provider.rs`: `ModelProvider` trait, `StreamEvent` enum
- `crates/zeroclaw-providers/src/compatible.rs`: OpenAI-compat SSE parser
- `crates/zeroclaw-providers/src/anthropic.rs`: Anthropic streaming
- `crates/zeroclaw-providers/src/ollama.rs`: Ollama streaming
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`: channel-side stream consumption
