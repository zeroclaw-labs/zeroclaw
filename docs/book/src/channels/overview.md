# Channels — Overview

A **channel** is a messaging surface the agent talks through. One ZeroClaw instance can bind multiple channels simultaneously — the same agent can answer in Discord, Telegram, email, and over the REST gateway without you running separate processes.

Channels are implementations of the `Channel` trait in `zeroclaw-api`. Each one is feature-gated at compile time, so a minimal build only includes the channels you want.

## Categories

### Chat platforms

Real-time messaging where the agent can hold a conversation, get notified of new messages via push or long-poll, and reply as a bot user.

| Channel | Feature flag | Dedicated guide |
|---|---|---|
| Matrix | `channel-matrix` | [Matrix](./matrix.md) |
| Mattermost | `channel-mattermost` | [Mattermost](./mattermost.md) |
| LINE | `channel-line` | [LINE](./line.md) |
| Nextcloud Talk | `channel-nextcloud-talk` | [Nextcloud Talk](./nextcloud-talk.md) |
| Discord, Slack, Telegram, Signal, iMessage, WeCom, DingTalk, Lark, QQ, IRC, Mochat, Notion | per channel | [Other chat platforms](./chat-others.md) |

### Social & broadcast

One-to-many or public-feed integrations.

| Channel | Feature flag | Protocol / service |
|---|---|---|
| Bluesky | `channel-bluesky` | AT Protocol |
| Nostr | `channel-nostr` | NIP-01 relays |
| Twitter / X | `channel-twitter` | API v2 |
| Reddit | `channel-reddit` | JSON API |

See [Social channels](./social.md).

### Email

| Channel | Feature flag | Notes |
|---|---|---|
| IMAP / SMTP | `channel-email` | Classic poll-based inbox |
| Gmail Push | `channel-gmail-push` | Google Pub/Sub push notifications — real-time, no polling |

See [Email](./email.md).

### Voice & telephony

| Channel | Feature flag | Service |
|---|---|---|
| ClawdTalk | `channel-clawdtalk` | Telnyx SIP real-time voice |
| Voice Call | `channel-voice-call` | Twilio / Telnyx / Plivo |
| Voice Wake | `channel-voice-wake` | Local wake-word detection |
| TTS | `channel-tts` | Outbound speech synthesis (OpenAI, ElevenLabs, Google Cloud, Edge, Piper) |

See [Voice & telephony](./voice.md).

### Webhooks & programmatic

| Channel | Feature flag | Shape |
|---|---|---|
| Webhook | (always on with gateway) | Inbound HTTP → agent |
| CLI | always on | Local stdin/stdout |
| Gateway REST/WS | always on | HTTP + WebSocket |
| ACP (Agent Client Protocol) | (always on with runtime) | JSON-RPC 2.0 over stdio — editor/IDE sessions |

See [Webhooks](./webhook.md) and [ACP](./acp.md).

## Configuration

Every channel is configured under `[channels.<name>]`:

```toml
[channels.discord]
enabled = true
bot_token = "..."
allowed_users = ["123456789012345678"]
reply_to_mentions_only = false
```

Channel-specific options live under the same block. Common keys across channels:

| Key | What it does |
|---|---|
| `enabled` | On/off without removing the section |
| `allowed_users` | Whitelist — empty means allow all |
| `allowed_destinations` | Restrict which rooms/channels/threads the bot answers in |
| `reply_to_mentions_only` | Ignore messages that don't @-mention the bot |
| `provider` | Override default model for this channel |
| `draft_update_interval_ms` | Streaming edit cadence (default 500 ms) |

## Pairing

Most channels require **pairing** — a one-time handshake that binds an incoming message source to the agent's policy. The onboarding wizard handles pairing for channels you configure during `zeroclaw onboard`; use `zeroclaw channel add` and `zeroclaw channel bind-telegram` (for Telegram specifically) to pair additional identities post-onboard. Without pairing, the channel rejects everything.

The rationale: an agent with a public Telegram bot token and no pairing is a publicly-accessible shell. Pairing is the gate.

## Streaming capability

Channels declare what kind of streaming they support — see [Providers → Streaming](../providers/streaming.md) for the capability matrix and what `supports_draft_updates` / `supports_multi_message_streaming` mean.

## Adding a channel

Implementing a new channel means adding a file to `crates/zeroclaw-channels/src/` that implements the `Channel` trait. The canonical reference is any existing channel of similar shape — `discord.rs` for push-based, `email_channel.rs` for polling, `webhook.rs` for HTTP-driven.
