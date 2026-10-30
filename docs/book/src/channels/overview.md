# Channels: Overview

A **channel** is a messaging surface the agent talks through. One ZeroClaw instance can bind multiple channels simultaneously: the same agent can answer in Discord, Telegram, email, and over the REST gateway without you running separate processes.

An agent lists the channels it answers on; see [Agents](../agents/overview.md) for how channels attach to an agent (and how a [peer group](./peer-groups.md) lets agents on a shared channel address each other).

Channels are implementations of the `Channel` trait in `zeroclaw-api`. Each one is feature-gated at compile time, so a minimal build only includes the channels you want.

The default ZeroClaw build includes a lean channel bundle: ACP, webhook, email, Telegram, Discord, and filesystem. These cover local/editor sessions, gateway ingress, and common first-run external messaging without compiling every bundled platform integration. Standard distribution artifacts add Git, Matrix, Lark, and WhatsApp Web; the Android artifact omits WhatsApp Web for target compatibility. For source installs that need the historical broad channel set, run `install.sh --source --preset full`, build with `--features channels-full`, or use individual `channel-*` features for selective builds:

<div class="os-tabs-src">

#### sh

```sh
./install.sh --source --preset full
cargo build --features channels-full
cargo build --no-default-features --features "agent-runtime,gateway,channel-slack"
```

</div>

## Categories

### Chat platforms

Real-time messaging where the agent can hold a conversation, get notified of new messages via push or long-poll, and reply as a bot user.

| Channel | Feature flag | Dedicated guide |
|---|---|---|
| Matrix | `channel-matrix` | [Matrix](./matrix.md) |
| Telegram | `channel-telegram` | [Telegram](./telegram.md) |
| Discord | `channel-discord` | [Discord](./discord.md) |
| Slack | `channel-slack` | [Slack](./slack.md) |
| Mattermost | `channel-mattermost` | [Mattermost](./mattermost.md) |
| LINE | `channel-line` | [LINE](./line.md) |
| Nextcloud Talk | `channel-nextcloud` | [Nextcloud Talk](./nextcloud-talk.md) |
| Signal | `channel-signal` | [Signal](./signal.md) |
| Twitch | `channel-twitch` | [Twitch](./social.md#twitch) |
| WhatsApp Cloud API | `channel-whatsapp-cloud` | [WhatsApp](./whatsapp.md) |
| WhatsApp Web | `whatsapp-web` | [WhatsApp](./whatsapp.md) |
| WeCom (Bot Webhook / AI Bot WS) | `channel-wecom`, `channel-wecom-ws` | [Other chat platforms](./chat-others.md) |
| iMessage, WeChat personal iLink Bot, DingTalk, Lark, QQ, IRC, Mochat, Notion | per channel | [Other chat platforms](./chat-others.md) |

### Social & broadcast

One-to-many or public-feed integrations.

| Channel | Feature flag | Protocol / service |
|---|---|---|
| Bluesky | `channel-bluesky` | AT Protocol |
| Nostr | `channel-nostr` | NIP-01 relays |
| Twitter / X | `channel-twitter` | API v2 |
| Reddit | `channel-reddit` | JSON API |

See [Social channels](./social.md).

### Developer platforms

| Channel | Feature flag | Protocol / service |
|---|---|---|
| Git | `channel-git` | Git forge behind a provider seam (GitHub, Gitea, Forgejo); REST polling of issues, PRs, CI runs & releases with per-event routing |

See [Git](./git.md).

### Email

| Channel | Feature flag | Notes |
|---|---|---|
| IMAP / SMTP | `channel-email` | Classic poll-based inbox |
| Gmail Push | `channel-email` | Google Pub/Sub push notifications: real-time, no polling |

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
| Gateway REST/WS | `gateway` (enabled by default) | HTTP + WebSocket |
| ACP (Agent Client Protocol) | `channel-acp-server` | JSON-RPC 2.0 over stdio: editor/IDE sessions |

See [Webhooks](./webhook.md) and [ACP](./acp.md).

### Event sources

Input-only transports that feed events into the agent loop or the SOP engine. They have no outbound reply; each one is also a [SOP fan-in](../sop/fan-in/overview.md).

| Channel | Feature flag | Shape |
|---|---|---|
| MQTT | `channel-mqtt` | Broker messages → agent or SOP |
| AMQP | `channel-amqp` | Broker deliveries → agent or SOP |
| Filesystem | `channel-filesystem` | Path changes → agent or SOP |

See [MQTT](./mqtt.md), [AMQP](./amqp.md), and [Filesystem](./filesystem.md).

## Configuration

Modern channel instances are configured under `[channels.<type>.<alias>]`, with `default` as the common first alias. Set them through any config surface:

{{#config-where channels}}

Secrets (bot tokens, API keys, passwords) are stored encrypted; set them through the gateway, zerocode, or `zeroclaw config set` (masked), never in plaintext. The `channels` entry on an agent binds a channel alias to that agent. Field names differ per channel; `zeroclaw config schema` is the authoritative list. Fields that recur across many channels:

| Key | What it does |
|---|---|
| `enabled` | On/off without removing the section |
| `mention_only` | Ignore messages that don't @-mention the bot (chat platforms) |
| `proxy_url` | Per-channel proxy (http/https/socks5/socks5h); overrides global `[proxy]` |
| `excluded_tools` | Tools withheld from the model when answering on this channel |
| `draft_update_interval_ms` | Streaming edit cadence (default 500 ms) |
| `approval_timeout_secs` | Seconds to wait for operator approval on `always_ask` tools before auto-denying |

Inbound senders are gated through [peer groups](./peer-groups.md), not a per-channel field.

### Model fallback notices

`channels.model_fallback_notice` controls notices for ordinary fallback to a different model within the same provider family. The default is `off`. Cross-family fallback notices and safeguard notices keep their existing behavior in every mode.

| Mode | Delivered notice |
|---|---|
| `off` | No ordinary same-family model fallback notice |
| `redacted` | A generic notice that a fallback model served the reply, without provider or model identifiers |
| `detailed` | The requested and served provider/model identifiers |

The setting applies globally to all channels that use the shared channel orchestrator. Choose the mode for every audience that can receive a reply, including shared rooms and public channels. `detailed` exposes routing identifiers to those audiences; `redacted` reports recovery without those identifiers. There are no per-channel overrides.

This setting does not control fallback notices in gateway web chat, ZeroCode, or RPC sessions; those use the agent turn path and retain their existing detailed notices with provider and model identifiers.

Set the mode through ZeroCode Config or another supported config editor, or use:

```bash
zeroclaw config set channels.model_fallback_notice redacted
```

The orchestrator reads the current live config when it delivers the reply, so a published mode change can affect a turn already in progress. Saving a file alone does not update a running daemon's live config. Use the existing config publication and reload paths described in [Config lifecycle](../architecture/config-lifecycle.md#saved-vs-applied); standalone CLI writes take effect on the next load or reload.

These notices are added only to outbound delivery. Stored assistant history keeps the model's response without the notice, so later turns do not receive this delivery metadata as conversation content. Attribution must identify a successful fallback to a different model before a notice is added; same-family, same-model profile changes remain indistinguishable in the current attribution record.

## Streaming capability

Channels declare what kind of streaming they support: see [Providers → Streaming](../providers/streaming.md) for the capability matrix and what `supports_draft_updates` / `supports_multi_message_streaming` mean.

## Adding a channel

Implementing a new channel means adding a file to `crates/zeroclaw-channels/src/` that implements the `Channel` trait. The canonical reference is any existing channel of similar shape: `discord.rs` for push-based, `email_channel.rs` for polling, `webhook.rs` for HTTP-driven.
