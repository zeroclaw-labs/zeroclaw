# Channels — Overview

A **channel** is a messaging surface the agent talks through. One ZeroClaw instance can bind multiple channels simultaneously — the same agent can answer in Discord, Telegram, email, and over the REST gateway without you running separate processes.

Channels are implementations of the `Channel` trait in `zeroclaw-api`. Each one is feature-gated at compile time, so a minimal build only includes the channels you want.

The default ZeroClaw build includes a lean channel bundle: ACP, webhook, email, and Telegram. These cover local/editor sessions, gateway ingress, and common first-run external messaging without compiling every bundled platform integration. Build with `--features channels-full` when you want the historical broad channel set, or use individual `channel-*` features for selective builds:

```bash
cargo build --features channels-full
cargo build --no-default-features --features "agent-runtime,gateway,channel-discord"
```

## Categories

### Chat platforms

Real-time messaging where the agent can hold a conversation, get notified of new messages via push or long-poll, and reply as a bot user.

| Channel | Feature flag | Dedicated guide |
|---|---|---|
| Matrix | `channel-matrix` | [Matrix](./matrix.md) |
| Mattermost | `channel-mattermost` | [Mattermost](./mattermost.md) |
| LINE | `channel-line` | [LINE](./line.md) |
| Nextcloud Talk | `channel-nextcloud` | [Nextcloud Talk](./nextcloud-talk.md) |
| Signal | `channel-signal` | [Signal](./signal.md) |
| WhatsApp Cloud API | `channel-whatsapp-cloud` | [WhatsApp](./whatsapp.md) |
| WhatsApp Web | `whatsapp-web` | [WhatsApp](./whatsapp.md) |
| Discord, Slack, Telegram, iMessage, WeCom Bot Webhook, WeCom AI Bot Long Connection, WeChat personal iLink Bot, DingTalk, Lark, QQ, IRC, Mochat, Notion | per channel | [Other chat platforms](./chat-others.md) |

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
| Gmail Push | `channel-email` | Google Pub/Sub push notifications — real-time, no polling |

See [Email](./email.md).

### Voice & telephony

| Channel | Feature flag | Service |
|---|---|---|
| ClawdTalk | `channel-clawdtalk` | Telnyx SIP real-time voice |
| Voice Call | `channel-voice-call` | Twilio / Telnyx / Plivo |
| Voice Wake | `voice-wake` | Local wake-word detection |
| TTS | always compiled with channel support | Outbound speech synthesis (OpenAI, ElevenLabs, Google Cloud, Edge, Piper) |

See [Voice & telephony](./voice.md).

### Webhooks & programmatic

| Channel | Feature flag | Shape |
|---|---|---|
| Webhook | `channel-webhook` | Inbound HTTP → agent |
| CLI | always on | Local stdin/stdout |
| Gateway REST/WS | always on | HTTP + WebSocket |
| ACP (Agent Client Protocol) | `channel-acp-server` | JSON-RPC 2.0 over stdio — editor/IDE sessions |

See [Webhooks](./webhook.md) and [ACP](./acp.md).

## Configuration

Modern channel instances are configured under `[channels.<type>.<alias>]`, with `default` as the common first alias:

```toml
[channels.discord.default]
enabled = true
bot_token = "..."
allowed_users = ["123456789012345678"]
reply_to_mentions_only = false

[agents.assistant]
enabled = true
channels = ["discord.default"]
```

The `channels` entry binds the channel alias to the agent that should answer it. Some older per-channel guides still show legacy flat examples; prefer the alias shape above for new config. Channel-specific options live under the same block. Common keys across channels:

| Key | What it does |
|---|---|
| `enabled` | On/off without removing the section |
| `allowed_users` | Whitelist — empty means allow all |
| `allowed_destinations` | Restrict which rooms/channels/threads the bot answers in |
| `reply_to_mentions_only` | Ignore messages that don't @-mention the bot |
| `provider` | Override default model for this channel |
| `draft_update_interval_ms` | Streaming edit cadence (default 500 ms) |

## Pairing

Most channels require **pairing** — a one-time handshake that binds an incoming message source to the agent's policy. `zeroclaw onboard channels` walks you through pairing each channel you configure; use `zeroclaw channel bind-telegram` for Telegram-specific identities and the channel-specific guide for channels such as WhatsApp or Signal. Without pairing, the channel rejects everything.

The rationale: an agent with a public Telegram bot token and no pairing is a publicly-accessible shell. Pairing is the gate.

## Streaming capability

Channels declare what kind of streaming they support — see [Providers → Streaming](../providers/streaming.md) for the capability matrix and what `supports_draft_updates` / `supports_multi_message_streaming` mean.

## Adding a channel

Implementing a new channel means adding a file to `crates/zeroclaw-channels/src/` that implements the `Channel` trait. The canonical reference is any existing channel of similar shape — `discord.rs` for push-based, `email_channel.rs` for polling, `webhook.rs` for HTTP-driven.
