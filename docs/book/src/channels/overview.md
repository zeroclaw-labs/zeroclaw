# Channels — Overview

A **channel** is a messaging surface the agent talks through. One ZeroClaw instance can bind multiple channels simultaneously — the same agent can answer in Discord, Telegram, email, and over the REST gateway without you running separate processes.

Channels are implementations of the `Channel` trait in `zeroclaw-api`. Each one is feature-gated at compile time, so a minimal build only includes the channels you want.

## Categories

> **Feature gating note.** The schema compiles every channel's `*Config` struct unconditionally except `NostrConfig` (`#[cfg(feature = "channel-nostr")]`) and `VoiceWakeConfig` (`#[cfg(feature = "voice-wake")]`). The runtime channel implementations in `zeroclaw-channels` are separately gated by per-channel Cargo features at build time — see that crate's `Cargo.toml` for the canonical list. The tables below describe the schema surface; runtime build flags are a parallel concern.

### Chat platforms

Real-time messaging where the agent can hold a conversation, get notified of new messages via push or long-poll, and reply as a bot user.

| Channel | Dedicated guide |
|---|---|
| Matrix | [Matrix](./matrix.md) |
| Mattermost | [Mattermost](./mattermost.md) |
| LINE | [LINE](./line.md) |
| Nextcloud Talk | [Nextcloud Talk](./nextcloud-talk.md) |
| Discord, Slack, Telegram, Signal, iMessage, WeCom, DingTalk, Lark / Feishu, QQ, IRC, Mochat, Notion | [Other chat platforms](./chat-others.md) |

### Social & broadcast

One-to-many or public-feed integrations.

| Channel | Protocol / service |
|---|---|
| Bluesky | AT Protocol |
| Nostr | NIP-01 relays (schema-gated by `channel-nostr`) |
| Twitter / X | API v2 (OAuth 2.0 Bearer Token) |
| Reddit | OAuth 2.0 |

See [Social channels](./social.md).

### Email

| Channel | Notes |
|---|---|
| IMAP / SMTP | IDLE-first delivery, polling fallback when the server doesn't advertise IDLE |
| Gmail Push | Google Pub/Sub push notifications — real-time, no polling |

See [Email](./email.md).

### Voice & telephony

| Channel | Service |
|---|---|
| ClawdTalk | Telnyx SIP real-time voice |
| Voice Call | Twilio / Telnyx / Plivo |
| Voice Wake | Local wake-word detection (schema-gated by `voice-wake`) |
| TTS | Outbound speech synthesis (OpenAI, ElevenLabs, Google Cloud, Edge, Piper) — top-level `[tts]`, not a `[channels.*]` block |

See [Voice & telephony](./voice.md).

### Webhooks & programmatic

| Channel | Shape |
|---|---|
| Webhook | Inbound HTTP → agent (embedded HTTP server on `port`) |
| CLI | Local stdin/stdout |
| Gateway REST/WS | HTTP + WebSocket (the gateway is a separate surface from `[channels.webhook]`) |
| ACP (Agent Client Protocol) | JSON-RPC 2.0 over stdio — editor/IDE sessions |

See [Webhooks](./webhook.md) and [ACP](./acp.md).

## Configuration

Every channel is configured under `[channels.<name>]`. The shape is uniform — each `*Config` struct in `crates/zeroclaw-config/src/schema.rs` is mapped to a TOML table:

```toml
[channels.<name>]
enabled = true
# channel-specific fields follow
```

For the canonical example for any specific channel, jump to its dedicated page (Matrix, Mattermost, LINE, Nextcloud Talk, Webhook, Email, Voice, Social) or to [Other chat platforms](./chat-others.md) for Discord, Slack, Telegram, Signal, iMessage, WeCom, DingTalk, Lark/Feishu, QQ, IRC, Mochat, and Notion.

Common keys recurring across many (but not all) channel schemas:

| Key | What it does | Channels that have it |
|---|---|---|
| `enabled` | On/off without removing the section | All |
| `allowed_users` | Sender allowlist; empty = deny all (some channels also support `"*"` = allow all) | Most chat channels |
| `mention_only` | Only respond when the bot is @-mentioned | Discord, Slack, Telegram, Matrix, Mattermost, IRC, Lark, Signal-via-policy |
| `interrupt_on_new_message` | Cancel an in-flight reply when a newer message arrives | Discord, Slack, Telegram, Matrix, Mattermost |
| `stream_mode` | `"off"` / `"partial"` / `"multi_message"` for progressive replies | Discord, Telegram, Matrix |
| `draft_update_interval_ms` | Minimum interval between draft edits when streaming | Discord, Slack (default 1200), Telegram, Matrix |
| `proxy_url` | Per-channel `[proxy]` override (`http://`, `https://`, `socks5://`, `socks5h://`) | Discord, Telegram, Matrix, Mattermost, Slack, DingTalk, QQ, Lark, Nextcloud Talk, Signal |
| `approval_timeout_secs` | Seconds to wait for operator approval on `always_ask` tools | Discord, Slack, Matrix, Telegram, Signal, Voice Call |

Beyond these, every channel has its own platform-specific fields (auth, room/channel scoping, mode toggles). Don't assume a key applies to every channel — check the dedicated page or the [Config reference](../reference/config.md).

## Pairing

Most channels require **pairing** — a one-time handshake that binds an incoming message source to the agent's policy. The onboarding wizard handles pairing for channels you configure during `zeroclaw onboard`; use `zeroclaw channel add` and `zeroclaw channel bind-telegram` (for Telegram specifically) to pair additional identities post-onboard. Without pairing, the channel rejects everything.

The rationale: an agent with a public Telegram bot token and no pairing is a publicly-accessible shell. Pairing is the gate.

## Streaming capability

Channels declare what kind of streaming they support — see [Providers → Streaming](../providers/streaming.md) for the capability matrix and what `supports_draft_updates` / `supports_multi_message_streaming` mean.

## Adding a channel

Implementing a new channel means adding a file to `crates/zeroclaw-channels/src/` that implements the `Channel` trait. The canonical reference is any existing channel of similar shape — `discord.rs` for push-based, `email_channel.rs` for polling, `webhook.rs` for HTTP-driven.
