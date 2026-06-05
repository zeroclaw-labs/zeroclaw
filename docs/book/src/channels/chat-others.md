# Other Chat Platforms

Channels with working integrations but not yet pulled out into dedicated guides. Each is feature-gated; enable the matching `channel-<name>` feature at build time.

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

## Rocket.Chat

The Rocket.Chat channel is a REST-polling integration against any Rocket.Chat-compatible server (rocket.chat cloud or self-hosted). It polls each configured room for new messages and replies via `chat.postMessage`. It is a polling channel — no inbound webhook or public route is required.

**Getting credentials.** In the Rocket.Chat web UI, mint a Personal Access Token under **My Account → Personal Access Tokens → Add**. The PAT model requires two values, both shown in the creation dialog: the **token** itself (sent as the `X-Auth-Token` header) and the bot account's **`_id`** (sent as the `X-User-Id` header). Copy them into `auth_token` and `user_id` respectively.

```toml
[channels.rocketchat.default]
enabled = true                          # must be explicitly enabled (default false)
server_url = "https://chat.example.com" # server base URL; trailing slash optional (stripped at load)
auth_token = "..."                      # Personal Access Token (My Account → Personal Access Tokens), sent as X-Auth-Token
user_id = "..."                         # bot account's _id, sent as X-User-Id (shown next to the token)
allowed_users = ["alice"]               # usernames without leading @; empty = deny all, "*" = allow all
room_ids = ["GENERAL"]                  # DM / channel / private-group IDs to poll
poll_interval_secs = 10                 # default 10: REST poll cadence (floored at 2s)
excluded_tools = []                     # tools withheld from the model on this channel
```

**How it works.** Every `poll_interval_secs` (default 10, floored at 2s) the channel iterates over each id in `room_ids` and calls `GET /api/v1/chat.syncMessages?roomId={id}&lastUpdate={iso8601}`, advancing a per-room cursor by the most recent `_updatedAt` it has seen (the first poll looks back 60 seconds). A `chat.postMessage` accepts a room id uniformly for DMs, channels, and private groups, so the reply target is simply the room id. Find a `room_id` via the admin REST API or by opening the channel in admin mode. When `room_ids` is empty the listener has nothing to poll and simply idles. Outbound replies are sent via `POST /api/v1/chat.postMessage` with `{roomId, text}` (both auth headers applied); Rocket.Chat enforces no hard per-message limit, but bodies over 4000 characters are split with `(i/N)` continuation markers so they render cleanly.

**Allowlist & filtering.** `allowed_users` lists Rocket.Chat usernames **without** a leading `@`. An empty list **denies everyone**, `"*"` allows everyone, and matching is case-insensitive (a leading `@` on an entry is tolerated). The bot skips its own posts, system/bot-flagged messages, edited messages (ingestion is append-only), and empty bodies.

- **Slot:** alias-keyed `[channels.rocketchat.<alias>]`.
- **Polling channel:** no inbound webhook or public route is needed. The `auth_token` is a `#[secret]` config field — set it in config, not via environment variables.

## Zulip

The Zulip channel works against any Zulip-compatible server (zulipchat.com or self-hosted), using the long-poll Events API for inbound and `messages.send` for outbound. It is a polling channel — no inbound webhook or public route is required.

**Getting credentials.** Create a bot in the Zulip web UI under **Personal settings → Bots → Add a new bot**, then copy the bot's **email** and **API key** into `bot_email` and `api_key`. Both are sent as HTTP Basic auth (email as username, API key as password) on every request. The operator must also **subscribe the bot to each stream** listed in `streams` from the Zulip UI — v1 does not auto-subscribe.

```toml
[channels.zulip.default]
enabled = true                                   # must be explicitly enabled (default false)
server_url = "https://yourorg.zulipchat.com"     # server base URL; trailing slash optional (stripped at load)
bot_email = "agent-bot@yourorg.zulipchat.com"   # HTTP Basic auth username; also used to suppress self-messages
api_key = "..."                                  # HTTP Basic auth password (Personal settings → Bots → Add a new bot)
allowed_users = ["alice@yourorg.zulipchat.com"]  # sender emails; empty = deny all, "*" = allow all (case-insensitive)
streams = ["general"]                            # bot must be subscribed in the Zulip UI; empty = private messages only
default_topic = "agent"                          # default "agent": topic used when sending to a stream without an explicit topic
event_timeout_secs = 60                          # default 60: long-poll timeout for GET /api/v1/events
excluded_tools = []                              # tools withheld from the model on this channel
```

**How it works.** On `listen()` the channel registers a long-poll event queue via `POST /api/v1/register` (`event_types=["message"]`, narrowed to the configured `streams`), then loops on `GET /api/v1/events?queue_id=…&last_event_id=…`. Each call blocks until a message arrives or `event_timeout_secs` (default 60) elapses, after which it reissues. The cursor advances on every successful poll. If Zulip expires the queue (`BAD_EVENT_QUEUE_ID`) the channel re-registers and resumes; other transient errors back off and retry. Outbound replies are sent via `POST /api/v1/messages` (form-urlencoded, Basic auth); bodies over 8000 characters are split with `(i/N)` continuation markers.

**Allowlist, streams & topics.** `allowed_users` lists sender **emails**; an empty list **denies everyone**, `"*"` allows everyone, and matching is case-insensitive. `streams` are the streams to listen on (the bot must be subscribed to each); an **empty `streams` narrows to private-message events only**. When replying to a stream without an explicit topic, the channel uses `default_topic` (default `"agent"`). Recipient encoding for outbound sends: `stream:Stream Name`, `stream:Stream Name/Topic`, `private:user@example.com,user2@example.com` (multi-party DM), or a bare email (DM shorthand). The bot suppresses its own messages (`sender_email == bot_email`, case-insensitive) and skips edited messages.

- **Slot:** alias-keyed `[channels.zulip.<alias>]`.
- **Polling channel:** no inbound webhook or public route is needed. The `api_key` is a `#[secret]` config field — set it in config, not via environment variables.

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
