# Other Chat Platforms

Channels with working integrations but not yet pulled out into dedicated guides. Each is feature-gated; enable the matching `channel-<name>` feature at build time.

## Pacing outbound replies (`reply_min_interval_secs`)

Every outbound channel accepts an optional `reply_min_interval_secs = N` field (range `0..=REPLY_MIN_INTERVAL_MAX_SECS`, default `0`). When set, the orchestrator wraps the channel in a per-(channel, recipient) pacing layer so consecutive outbound replies to the same peer wait at least `N` seconds apart. `0` (the default) is a passthrough, no wrapper allocated, no overhead.

When the floor is active, sends that arrive before the floor elapses enter a bounded FIFO queue. A background worker drains the queue at the floor rate so replies still land in order at the configured cadence. The queue depth defaults to **16** (good for the "agent went briefly bursty" case) and is capped at `REPLY_QUEUE_DEPTH_CEILING` (`1024`). When the queue is full the **newest** send is dropped and a `WARN` is emitted with `channel_alias`, redacted `recipient`, `queue_depth`, `queue_max`, and `dropped_chars`: body content stays out of logs.

Streaming draft updates within a single reply are **not** paced (they would freeze the live preview); only the final `send` (and the terminal `finalize_draft` write) enter the queue. Different recipients are independent: pacing for one peer does not block messages to another. The wrapper retains state for up to `PACING_RECIPIENT_CAP` (1024) distinct peers via idle-state LRU eviction: only rows with no queued work and no in-flight send are reclaimed, so the cap is a target for idle state rather than an unconditional hard bound under an all-active burst.

Use case: paired-identity channels where sub-second replies are an AI-tell. Wire-level coverage exists end-to-end across nine channels (Telegram, Discord, Slack, Mattermost, Webhook, iMessage, Matrix, Signal, WhatsApp); integration tests pin the floor + overflow contract on Telegram and WhatsApp Web.

> **Webhook caveat:** on a synchronous webhook channel the outbound reply is the HTTP response to the caller's request. A non-zero `reply_min_interval_secs` floor can hold that response open for the floor duration, which may exceed the caller's own request timeout. Set the floor only when the webhook caller tolerates a delayed response, or leave it at `0` and pace upstream.

## Telegram

### Setup

1. **Create a bot** with [@BotFather](https://t.me/botfather) on Telegram. Send `/newbot`, pick a name, and copy the token BotFather returns.
2. **Add the bot to your config** with the token you just copied. The token is
   a secret, so set it through a surface that encrypts it rather than typing it
   into `config.toml`:

   {{#config-where channels telegram}}

   {{#secret-config channels.telegram.<alias>.bot_token}}

3. **Start the bot**: send `/start` to it in a Telegram chat so it can begin
   receiving messages.

### Who can talk to the agent

{{#peer-group telegram}}

Inbound senders are gated through **peer groups**, not a per-channel
`allowed_users` field. After the channel is configured, authorized senders
are listed in `[peer_groups.<name>].external_peers`. See
[Peer Groups](./peer-groups.md) for the full decision logic.

If you want to let anyone message the bot without pairing, add `"*"` to the
peer group's `external_peers`:

```toml
[peer_groups.telegram_default]
channel = "telegram"
external_peers = ["*"]
```

### Binding a Telegram identity

Once the channel is configured, use `zeroclaw channel bind-telegram` to
authorize a specific Telegram user. The identity can be:

- A **numeric user ID** (e.g., `zeroclaw channel bind-telegram 111111111`)
- An **@username** (e.g., `zeroclaw channel bind-telegram @alice`)

To find your Telegram user ID, send a message to [@userinfobot](https://t.me/userinfobot)
or use a bot that echoes back your chat ID.

## iMessage (macOS only)

iMessage is bridged through the Linq Partner API (`[channels.linq.<alias>]`):

**macOS-only** and requires either Linq as a third-party relay, or direct AppleScript automation (experimental, requires Full Disk Access and Accessibility grants).

## WeChat personal iLink Bot (微信个人号 iLink)

WeChat personal iLink Bot uses QR-code login against the iLink Bot API for personal WeChat conversations.

## DingTalk

Alibaba's enterprise messenger.

## Lark / Feishu

Build with `channel-lark` for either Lark or Feishu. The root `channel-feishu` feature is an alias for `channel-lark`; runtime selection still happens through `use_feishu = true`.

## QQ

Tencent's consumer messenger. Bot API access requires developer registration.

## IRC

Classic IRC. Supports SASL, NickServ auth, and multiple channels.

## Mochat

## Notion

Treats a Notion database as a message surface. Useful for asynchronous workflows where the "channel" is a task inbox.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md): E2EE, device verification, Synapse/Dendrite specifics
- [Discord](./discord.md)
- [Slack](./slack.md)
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)
- [Signal](./signal.md)
- [WhatsApp](./whatsapp.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
