# Streaming

Streaming is capability-driven. Providers that implement the streaming methods and return `supports_streaming() == true` can emit token deltas; other providers use the non-streaming response path. The runtime forwards available streams to channel adapters that support partial updates.

## What gets streamed

The provider trait emits `StreamEvent` values as the model generates output:
text deltas, structured tool calls, provider-side pre-executed tool calls and
their results, token-usage reports, and a final completion marker. The
authoritative, per-variant definitions live with the type in
`crates/zeroclaw-api/src/model_provider.rs` (`enum StreamEvent`); reasoning
tokens arrive as text deltas, not a separate variant.

The runtime consumes these events. The channel orchestrator uses the `Channel` trait's draft-delivery methods and capability flags to surface progressive output where supported.

## Capability flags

A provider exposes two flags so the runtime knows what it can expect:

```rust
fn supports_streaming(&self) -> bool { false }
fn supports_streaming_tool_events(&self) -> bool { false }
```

- **`supports_streaming`**: true only when the concrete provider opts into streaming; the trait default is false
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

When both the provider and the channel support streaming, the flow is: provider emits `TextDelta` → runtime passes to channel → channel edits the sent message. The edit cadence is bounded by that channel's `draft_update_interval_ms` setting to avoid rate-limiting; defaults vary by channel.

**Multi-message mode differs by channel:** Matrix and Discord split on `\n\n` paragraph boundaries. Telegram `multi_message` sends one message per completed agent text turn (text between tool calls), flushed via `Channel::flush_draft_turn` when the LLM turn ends.

## Reasoning blocks

`StreamEvent` has no separate `ReasoningDelta` variant. When a provider exposes reasoning during streaming, it uses the `reasoning` field on the `StreamChunk` carried by `TextDelta`; provider and runtime configuration determine whether that content is requested or surfaced. Consumers should follow the `StreamChunk` contract rather than matching a nonexistent event variant.

## Tool calls mid-stream

When a model decides to call a tool, the provider emits `ToolCall`. The runtime:

1. Pauses reading from the provider's stream
2. Flushes any buffered text to the channel
3. Runs the tool (subject to security validation, see [Security → Overview](../security/overview.md))
4. Resumes the conversation with the tool result appended
5. Opens a new streaming call to the provider for the next assistant turn

From the user's perspective: text, then a visible indicator that the agent ran a tool (via channel-specific hints), then more text. For channels without typing indicators, the gap between the tool call and the next text chunk is the only signal.

## Non-streaming providers

When `supports_streaming()` is false, callers use the provider's non-streaming chat path. Channel adapters can still send the completed reply, but they do not receive incremental provider stream events.

## Code references

- `crates/zeroclaw-api/src/model_provider.rs`: `ModelProvider` trait, `StreamEvent` enum
- `crates/zeroclaw-providers/src/compatible.rs`: OpenAI-compat SSE parser
- `crates/zeroclaw-providers/src/anthropic.rs`: Anthropic streaming
- `crates/zeroclaw-providers/src/ollama.rs`: Ollama streaming
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`: channel-side stream consumption
