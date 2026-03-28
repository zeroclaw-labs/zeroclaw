# AGENTS.md — channels/

> Multi-platform messaging transport layer. Each channel bridges ZeroClaw to an external platform (Telegram, Slack, Discord, email, etc.) via a uniform async trait.

## Overview

Channels are the I/O boundary between the agent loop and users. A channel listens for inbound messages (long-polling, WebSocket, or IMAP IDLE), converts them to `ChannelMessage`, and sends outbound `SendMessage`s back. The orchestrator in `mod.rs` manages per-sender conversation history, parallelism, backoff reconnection, typing indicators, draft streaming, and session persistence.

## Key Files

| File | Purpose |
|---|---|
| `traits.rs` | `Channel` trait, `ChannelMessage`, `SendMessage` structs |
| `mod.rs` | `start_channels()` factory, message dispatch loop, history management, all constants |
| `session_store.rs` | Append-only JSONL session persistence (`{workspace}/sessions/`) |
| `session_sqlite.rs` | SQLite session backend alternative |
| `session_backend.rs` | `SessionBackend` trait abstracting storage |
| `link_enricher.rs` | URL preview/metadata extraction for inbound messages |
| `transcription.rs` | Voice message transcription (shared by Telegram, Slack, Discord) |
| `tts.rs` | Text-to-speech outbound support |

## Trait Contract (`Channel`)

Required methods (you must implement these):
- `name() -> &str` — unique identifier string, used in logs and config routing
- `send(&self, message: &SendMessage) -> Result<()>` — deliver outbound message; must handle chunking internally
- `listen(&self, tx: Sender<ChannelMessage>) -> Result<()>` — long-running receiver loop; send messages on `tx`; must not return under normal operation

Default methods (override when the platform supports them):
- `health_check() -> bool` — called every ~30s (`CHANNEL_HEALTH_HEARTBEAT_SECS`)
- `start_typing/stop_typing` — refreshed every 4s (`CHANNEL_TYPING_REFRESH_INTERVAL_SECS`)
- `supports_draft_updates() -> bool` + `send_draft/update_draft/finalize_draft/cancel_draft` — progressive streaming; return a platform message ID from `send_draft` for subsequent edits
- `add_reaction/remove_reaction` — emoji reactions; requires platform-specific ID mapping (see Slack's `unicode_emoji_to_slack_name`)
- `pin_message/unpin_message`

All methods are `async` and `&self` — use interior mutability (`Mutex`, `RwLock`, `parking_lot`) for mutable state.

## Extension Playbook — Adding a New Channel

1. Create `src/channels/my_platform.rs` with a struct implementing `Channel`.
2. Add `pub mod my_platform;` and `pub use my_platform::MyPlatformChannel;` in `mod.rs` (alphabetical order). If the platform needs an optional dependency, gate with `#[cfg(feature = "channel-myplatform")]`.
3. Add a config struct implementing `crate::config::traits::ChannelConfig` (provides `name()` and `desc()`). Derive `Serialize, Deserialize, JsonSchema`.
4. Add the config field to `ChannelsConfig` in `src/config/` and wire it into `collect_configured_channels()` in `mod.rs` (~line 3765+).
5. In `collect_configured_channels`, instantiate your channel from config and push a `ConfiguredChannel { display_name, channel: Arc::new(...) }`.
6. If feature-gated, add the feature to `Cargo.toml` and document in the channel's doc comment.
7. Add tests (see Testing Patterns below).
8. Update `docs/setup-guides/` with platform-specific setup instructions.

## Factory Registration

`collect_configured_channels(config)` reads `config.channels_config.<name>` for each platform. When `Some`, it constructs the channel struct and wraps it in `Arc<dyn Channel>`. The returned `Vec<ConfiguredChannel>` is iterated in `start_channels()` to spawn per-channel listener tasks with backoff. Channels that need async initialization (e.g., Nostr relay connect) are built in `start_channels()` directly rather than in `collect_configured_channels()`.

## Message Routing & Session Management

- Inbound `ChannelMessage` carries `sender`, `reply_target`, `channel`, `thread_ts`, and `interruption_scope_id`.
- `thread_ts` is the platform's reply-anchor (Slack `ts`, Discord thread snowflake). Set it on `SendMessage` to reply in-thread.
- `interruption_scope_id` is for cancellation grouping — only `Some` inside genuine reply threads. Top-level messages use sender+channel scoping.
- Per-sender history is stored in `ConversationHistoryMap` (max `MAX_CHANNEL_HISTORY` = 50 messages). History is compacted when stale (`CHANNEL_HISTORY_COMPACT_KEEP_MESSAGES` = 12, truncated to 600 chars each).
- Proactive context-budget trimming at `PROACTIVE_CONTEXT_BUDGET_CHARS` (400k chars) prevents provider context-window overflows.
- Sessions persist to disk via `SessionStore` (JSONL) or `SessionSqlite` for cross-restart continuity.
- The `/new` command clears session state for the sender (`PendingNewSessionSet`).

## Platform-Specific Constraints

| Platform | Max message chars | Chunking function | Notes |
|---|---|---|---|
| Telegram | 4096 | `split_message_for_telegram` | 30-char continuation overhead reserved; prefers newline/space splits |
| Discord | 2000 | `split_message_for_discord` | Gateway WebSocket; must handle heartbeat/resume |
| Slack | ~12000 (blocks) | Markdown block splitting | Socket Mode (app_token) or polling; emoji names not Unicode |
| IRC | ~512 bytes | `split_message` (byte-aware) | Per-line byte limit, not char limit |
| Email | Unlimited | No chunking | IMAP IDLE with 29-min restart (RFC 2177); uses `subject` field |

All channels must handle chunking **inside** their `send()` implementation. The orchestrator does not split messages.

## Testing Patterns

- **Unit tests live in-file** (`#[cfg(test)] mod tests`), not in a separate test directory.
- `traits.rs` has a `DummyChannel` test impl — use it as a template for the minimal required implementation.
- Chunking functions get exhaustive edge-case tests: exactly-at-limit, one-over, emoji/multibyte boundaries, consecutive whitespace, empty input.
- Use `tokio::test` for async tests. Channel `listen()` tests use `mpsc::channel(1)` and assert on received messages.
- Integration tests requiring API keys should be `#[ignore]` with a doc comment explaining required env vars.
- Test message IDs follow the pattern `{platform}_{channel_id}_{platform_id}` (e.g., `slack_C123_1234567890.123456`).

## Common Gotchas

- **Allowed-users semantics vary**: empty list = deny all (Discord, Email), `["*"]` = allow all. Never default to allow-all.
- **Proxy support**: channels should accept `proxy_url: Option<String>` and use `crate::config::build_channel_proxy_client()` for HTTP clients.
- **Typing indicator lifetime**: platforms expire typing after 5-10s. The orchestrator refreshes every 4s, but your `start_typing` must be idempotent.
- **Draft streaming**: `send_draft` returns `Option<String>` (message ID). If `None`, the orchestrator falls back to non-streaming send. `finalize_draft` re-sends with final formatting (e.g., Markdown rendering).
- **Voice transcription**: shared via `transcription.rs`. Configure via `TranscriptionConfig` on the channel struct. Audio attachments are downloaded, transcribed, and inlined automatically.
- **PairingGuard**: some channels (Telegram) use `security::pairing::PairingGuard` for bind-on-first-use authentication. Check if your platform needs sender verification.
- **Message timeout**: defaults to 300s, scaled by tool iterations, capped at 4x (`CHANNEL_MESSAGE_TIMEOUT_SCALE_CAP`).
- **Backoff on listen failure**: initial 2s, max 60s exponential backoff with jitter. Your `listen()` must propagate errors, not swallow them silently.

## Cross-Subsystem Coupling

- `src/config/` — `ChannelsConfig` struct holds all channel configs; `ChannelConfig` trait for schema.
- `src/providers/` — `ChatMessage` type flows through conversation history; provider is selected per-message via route config.
- `src/security/` — `SecurityPolicy` enforces autonomy levels; `PairingGuard` for channel-level auth.
- `src/tools/` — tool calls happen inside the channel message handler loop; results stream back as draft updates.
- `src/memory/` — auto-save of messages longer than 20 chars (`AUTOSAVE_MIN_MESSAGE_CHARS`); memory context injected into prompts.
- `src/observability/` — `ChannelNotifyObserver` wraps the observer to forward tool-call events as threaded channel messages.
