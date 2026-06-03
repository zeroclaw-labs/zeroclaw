# Other Chat Platforms

Channels with working integrations but not yet pulled out into dedicated guides. Each is feature-gated; enable the matching `channel-<name>` feature at build time.

## Pacing outbound replies (`reply_min_interval_secs`)

Every outbound channel accepts an optional `reply_min_interval_secs = N` field (range `0..=REPLY_MIN_INTERVAL_MAX_SECS`, default `0`). When set, the orchestrator wraps the channel in a per-(channel, recipient) pacing layer so consecutive outbound replies to the same peer wait at least `N` seconds apart. `0` (the default) is a passthrough — no wrapper allocated, no overhead.

```toml
[channels.whatsapp]
reply_min_interval_secs = 120    # 0 = no pacing (default)
reply_queue_depth_max   = 16     # bounded queue when pacing > 0 (0 = use default)
```

When the floor is active, sends that arrive before the floor elapses enter a bounded FIFO queue. A background worker drains the queue at the floor rate so replies still land in order at the configured cadence. The queue depth defaults to **16** (good for the "agent went briefly bursty" case) and is capped at `REPLY_QUEUE_DEPTH_CEILING` (`1024`). When the queue is full the **newest** send is dropped and a `WARN` is emitted with `channel_alias`, redacted `recipient`, `queue_depth`, `queue_max`, and `dropped_chars` — body content stays out of logs.

Streaming draft updates within a single reply are **not** paced (they would freeze the live preview); only the final `send` (and the terminal `finalize_draft` write) enter the queue. Different recipients are independent — pacing for one peer does not block messages to another. The wrapper retains state for up to `PACING_RECIPIENT_CAP` (1024) distinct peers, evicting the least-recently-touched idle row when the cap is reached.

Use case: paired-identity channels where sub-second replies are an AI-tell. Wire-level coverage exists end-to-end across nine channels (Telegram, Discord, Slack, Mattermost, Webhook, iMessage, Matrix, Signal, WhatsApp); integration tests pin the floor + overflow contract on Telegram and WhatsApp Web.

## Discord

```toml
[channels.discord]
enabled = true
bot_token = "..."                  # create at https://discord.com/developers/applications
allowed_guilds = ["123..."]
allowed_users = []
reply_to_mentions_only = true
draft_update_interval_ms = 750     # bump if hitting Discord rate limits
```

- **Bot intents needed:** Message Content Intent, Server Members Intent. Set in the Developer Portal.
- **[Streaming](../providers/streaming.md):** full — edits messages in place and splits long replies into multiple messages.
- **Tool-call indicator:** typing indicator while tools run; visible code-block preview for shell and browser calls.

## Slack

```toml
[channels.slack]
enabled = true
bot_token = "xoxb-..."            # classic bot token
app_token = "xapp-..."            # for Socket Mode
signing_secret = "..."
allowed_channels = ["C01..."]
```

- **Socket Mode** is the default (no public webhook URL needed).
- For HTTP Events API instead, drop `app_token` and point Slack's event subscription URL at `/slack/events` on the gateway.
- Supports multi-message streaming, threaded replies, and slash-command ingress.

## Telegram

```toml
[channels.telegram]
enabled = true
bot_token = "..."                  # from @BotFather
allowed_users = [123456789]
allowed_chats = [-100987...]       # group / channel IDs
use_long_polling = true            # default — no webhook needed
```

- Long polling is the default; no public URL required. Switch to webhook mode by setting `webhook_url` (then expose the gateway).
- Streaming draft edits are supported but capped by Telegram's rate limit. Tune `draft_update_interval_ms` if you see "Too Many Requests".

## iMessage (macOS only)

```toml
[channels.imessage]
enabled = true
provider = "linq"                  # Linq Partner API for iMessage/RCS/SMS
api_key = "..."
```

**macOS-only** and requires either Linq as a third-party relay, or direct AppleScript automation (experimental, requires Full Disk Access and Accessibility grants).

## WeCom Bot Webhook (企业微信群机器人)

```toml
[channels.wecom.default]
enabled = true
webhook_key = "..."                 # key from the group bot webhook URL
```

WeCom Bot Webhook is send-only through the group bot webhook API. Use it for simple outbound delivery into a WeCom group when ZeroClaw does not need to receive messages from WeCom.

## WeCom channel choices

| Use case | Config block | Transport | Direction |
|---|---|---|---|
| Send simple messages into a WeCom group bot webhook | `[channels.wecom.<alias>]` | WeCom group bot webhook | Outbound only |
| Receive and reply as a WeCom AI Bot | `[channels.wecom_ws.<alias>]` | WeCom AI Bot long connection over WebSocket | Bidirectional |

`wecom_ws` uses WebSocket as the transport, but it is not a generic WebSocket-compatible channel. It implements WeCom's AI Bot long-connection protocol, including subscription, inbound callback frames, response commands, request acknowledgements, user/group allowlists, and encrypted attachment handling.

## WeCom AI Bot Long Connection (企业微信智能机器人长连接)

```toml
[channels.wecom_ws.default]
enabled = true
bot_id = "..."
secret = "..."
allowed_users = ["zeroclaw_user"]    # empty denies all users
allowed_groups = ["zeroclaw_group"]  # empty denies all groups
bot_name = "danya"                   # optional group mention alias
stream_mode = "partial"
file_retention_days = 7
max_file_size_mb = 20
# proxy_url = "http://127.0.0.1:7890"  # optional per-channel override
```

This channel connects to WeCom's AI Bot long-connection API over WebSocket. Use it when ZeroClaw needs to receive WeCom messages and reply as the AI Bot. For simple outbound-only group webhook delivery, use `[channels.wecom.<alias>]` instead.

The WebSocket is only the transport. The channel still implements WeCom-specific subscription/auth, `msg_callback` parsing, `aibot_respond_msg` / `aibot_send_msg` replies, request acknowledgement handling, allowlists, group addressing, and encrypted attachment handling. Enabling `wecom_ws` does not change existing webhook behavior.

Access control is explicit. If both `allowed_users` and `allowed_groups` are empty, inbound messages are denied. Use `"*"` only for controlled test deployments.

Set `bot_name` to the visible WeCom robot name when using the channel in groups. This lets ZeroClaw recognize messages such as `@danya say hi` as addressed to the bot during reply-intent prechecks.

Attachments sent by WeCom can be downloaded into the workspace cache and represented to the model as local markers such as `[IMAGE:/absolute/path.png]` or `[Document: /absolute/path.bin]`.

Outbound image payloads are not supported yet. `stream_mode` supports `"partial"` for progressive draft updates or `"off"` for final replies only.

## WeChat personal iLink Bot (微信个人号 iLink)

```toml
[channels.wechat]
enabled = true
allowed_users = ["*"]
# api_base_url, cdn_base_url, and state_dir are optional overrides.
```

WeChat personal iLink Bot is a different channel from WeCom. It uses QR-code login against the iLink Bot API for personal WeChat conversations and should not be used for WeCom enterprise bot traffic.

## DingTalk

```toml
[channels.dingtalk]
enabled = true
app_key = "..."
app_secret = "..."
robot_code = "..."
```

Alibaba's enterprise messenger. Same bot shape as WeCom.

## Lark / Feishu

```toml
[channels.lark]
enabled = true
app_id = "..."
app_secret = "..."
# use_feishu = true  # route this Lark-compatible channel to Feishu endpoints
```

Build with `channel-lark` for either Lark or Feishu. The root `channel-feishu` feature is an alias for `channel-lark`; runtime selection still happens through `use_feishu = true`.

## QQ

```toml
[channels.qq]
enabled = true
bot_id = "..."
bot_token = "..."
```

Tencent's consumer messenger. Bot API access requires developer registration.

## IRC

```toml
[channels.irc]
enabled = true
server = "irc.libera.chat"
port = 6697
tls = true
nickname = "zeroclaw"
channels = ["#mychannel"]
nickserv_password = "..."          # optional
```

Classic IRC. Supports SASL, NickServ auth, and multiple channels.

## Mochat

```toml
[channels.mochat]
enabled = true
api_key = "..."
# additional provider-specific fields
```

## Notion

```toml
[channels.notion]
enabled = true
integration_token = "..."
databases = ["..."]                # DB IDs the agent can write to
```

Treats a Notion database as a message surface. Useful for asynchronous workflows where the "channel" is a task inbox.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)
- [Signal](./signal.md)
- [WhatsApp](./whatsapp.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
