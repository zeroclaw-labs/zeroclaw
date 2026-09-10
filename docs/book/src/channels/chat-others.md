# Other Chat Platforms

Channels with working integrations but not yet pulled out into dedicated guides. Each is feature-gated; enable the matching `channel-<name>` feature at build time.

## Pacing outbound replies (`reply_min_interval_secs`)

Every outbound channel accepts an optional `reply_min_interval_secs = N` field (range `0..=REPLY_MIN_INTERVAL_MAX_SECS`, default `0`). When set, the orchestrator wraps the channel in a per-(channel, recipient) pacing layer so consecutive outbound replies to the same peer wait at least `N` seconds apart. `0` (the default) is a passthrough, no wrapper allocated, no overhead.

When the floor is active, sends that arrive before the floor elapses enter a bounded FIFO queue. A background worker drains the queue at the floor rate so replies still land in order at the configured cadence. The queue depth defaults to **16** (good for the "agent went briefly bursty" case) and is capped at `REPLY_QUEUE_DEPTH_CEILING` (`1024`). When the queue is full the **newest** send is dropped and a `WARN` is emitted with `channel_alias`, redacted `recipient`, `queue_depth`, `queue_max`, and `dropped_chars`: body content stays out of logs.

Streaming draft updates within a single reply are **not** paced (they would freeze the live preview); only the final `send` (and the terminal `finalize_draft` write) enter the queue. Different recipients are independent: pacing for one peer does not block messages to another. The wrapper retains state for up to `PACING_RECIPIENT_CAP` (1024) distinct peers via idle-state LRU eviction: only rows with no queued work and no in-flight send are reclaimed, so the cap is a target for idle state rather than an unconditional hard bound under an all-active burst.

Use case: paired-identity channels where sub-second replies are an AI-tell. Wire-level coverage exists end-to-end across nine channels (Telegram, Discord, Slack, Mattermost, Webhook, iMessage, Matrix, Signal, WhatsApp); integration tests pin the floor + overflow contract on Telegram and WhatsApp Web.

> **Webhook caveat:** on a synchronous webhook channel the outbound reply is the HTTP response to the caller's request. A non-zero `reply_min_interval_secs` floor can hold that response open for the floor duration, which may exceed the caller's own request timeout. Set the floor only when the webhook caller tolerates a delayed response, or leave it at `0` and pace upstream.

## iMessage (macOS only)

iMessage is bridged through the Linq Partner API (`[channels.linq.<alias>]`):

**macOS-only** and requires either Linq as a third-party relay, or direct AppleScript automation (experimental, requires Full Disk Access and Accessibility grants).

## Sendblue (iMessage/SMS, any OS)

Sendblue (`[channels.sendblue.<alias>]`) is a hosted iMessage/SMS relay, so
unlike the AppleScript bridge it needs no Mac and runs on any host.

```toml
[channels.sendblue.main]
enabled = true
api_key_id = "…"        # sb-api-key-id
api_secret_key = "…"    # sb-api-secret-key
from_number = "+15550000000"
```

Outbound goes to `POST https://api.sendblue.com/api/send-message`.

### Inbound: polling (default)

The channel polls `GET /api/v2/messages` every `poll_interval_secs` (default
`15`, floor `5`). This is the default because it needs nothing but the API
credentials: no publicly reachable endpoint, no webhook configured in the
Sendblue dashboard, and none of the replay exposure below.

Only messages that arrive after the listener starts are dispatched, so a
restart does not re-answer the account's backlog. `created_at_gte` is
inclusive, so the boundary message is de-duplicated on its `message_handle`.

### Inbound: webhook (opt-in)

Set `poll_interval_secs = 0` and a `signing_secret`, then register the
gateway's `POST /sendblue[/<alias>]` route with Sendblue. Registration is an
API call, not a dashboard step, and uses the same credentials the channel
already holds:

```bash
curl -X POST https://api.sendblue.com/api/account/webhooks \
  -H "sb-api-key-id: $SB_API_KEY_ID" \
  -H "sb-api-secret-key: $SB_API_SECRET_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "type": "receive",
    "webhooks": [
      { "url": "https://your-gateway.example.com/sendblue/main",
        "secret": "<same value as signing_secret>" }
    ]
  }'
```

> **No signature scheme.** Sendblue does not sign message webhooks. Instead of
> an HMAC over the body it **echoes the configured secret verbatim** in the
> `sb-signing-secret` header, so the gateway can only compare that value; there
> is nothing to recompute. Because the secret is not bound to the request body,
> a captured header can be replayed with forged content. Sendblue enforces
> HTTPS on webhook URLs, which is what keeps the header off the wire — do not
> terminate the route on plain HTTP. With `signing_secret` unset the route
> refuses inbound requests with `401` rather than accepting them
> unauthenticated. Prefer polling unless you need push latency.
>
> Sendblue's separate Verify product *does* sign
> (`X-Sendblue-Signature: t=…,v1=…`), but that is a different webhook type and
> is not what a message channel receives.

### Both modes

Senders are gated by the channel's peer group, matched literally against the
E.164 number. A handle arriving without its leading `+` is normalized before
the check, so both forms match one allowlist entry.

Sendblue reports the bot's own sends alongside inbound traffic on both paths.
Those carry `is_outbound: true` and are dropped before dispatch, so the agent
does not answer itself.

## WeChat personal iLink Bot (微信个人号 iLink)

WeChat personal iLink Bot uses QR-code login against the iLink Bot API for personal WeChat conversations.

## WeCom (企业微信 / WeChat Work)

Two WeCom variants are implemented. They map to different WeCom products, build features, and config keys, so pick the one that matches how the bot is provisioned:

| | Bot Webhook (群机器人) | AI Bot WebSocket (智能机器人) |
|---|---|---|
| Build feature | `channel-wecom` | `channel-wecom-ws` |
| Config key | `[channels.wecom.<alias>]` | `[channels.wecom_ws.<alias>]` |
| Message flow | Outbound text only | Bidirectional with streaming drafts |
| Endpoint | `https://qyapi.weixin.qq.com/cgi-bin/webhook/send` | `wss://openws.work.weixin.qq.com` long connection |

### Bot Webhook (群机器人)

A WeCom **group robot** (群机器人) sends text messages to the group it was created in. Add a robot to the target group from the WeCom client, copy the webhook `key` out of its webhook URL, and configure the channel under the `default` alias:

```toml
[channels.wecom.default]
enabled = true
webhook_key = "<GROUP-BOT-WEBHOOK-KEY>"
```

Then bind it to an agent with `channels = ["wecom"]`.

This variant is outbound only: WeCom group-robot webhooks expose no callback that ZeroClaw subscribes to, so group messages never reach the agent through it. Use it to push agent notifications into a group. For conversations that start from inbound WeCom messages, use the AI Bot WebSocket variant below.

### AI Bot WebSocket (智能机器人)

The WeCom **AI Bot** (智能机器人) long-connection API gives a bidirectional channel: it receives single-chat and group messages and replies over the same socket, including streaming draft updates. The bot credentials (`bot_id` and `secret`) are issued when the AI Bot is created in the WeCom admin console.

```toml
[channels.wecom_ws.primary]
enabled = true
bot_id = "<AI-BOT-ID>"
secret = "<AI-BOT-SECRET>"

# Inbound authorization. Empty lists deny everything: without at least one
# entry no inbound message is accepted. "*" allows any sender and disables
# the allowlist, so prefer concrete IDs.
allowed_users = ["<wecom-user-id>"]
allowed_groups = ["<group-chat-id>"]

# Optional: how the bot is addressed in group text, e.g. "danya" for
# "@danya say hi". Lets the generic reply-intent precheck recognize that a
# group message was addressed to the bot.
bot_name = "<BOT-NAME>"
```

Then bind it to an agent with `channels = ["wecom_ws.primary"]` (use the alias from the config key).

Defaults worth knowing:

- Downloaded media attachments are decrypted, cached under the channel workspace, and cleaned up after `file_retention_days` (default 7); downloads over `max_file_size_mb` (default 20) are rejected.
- Replies stream as draft updates by default (`stream_mode = "partial"`); set `stream_mode = "off"` for a single whole-reply delivery. Only `partial` and `off` are supported on this channel; `multi_message` is rejected at startup.
- The per-channel `proxy_url` and `excluded_tools` fields apply as usual.

Inbound sender IDs may also come from a [peer group](./peer-groups.md) whose `channel` is `wecom_ws` or `wecom_ws.<alias>`, instead of the `allowed_*` lists above.

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
- [Telegram](./telegram.md): bot creation, aliases, pairing, and peer authorization
- [Discord](./discord.md)
- [Slack](./slack.md)
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)
- [Signal](./signal.md)
- [WhatsApp](./whatsapp.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
