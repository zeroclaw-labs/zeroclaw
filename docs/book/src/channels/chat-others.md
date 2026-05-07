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

## Signal

```toml
[channels.signal]
enabled = true
phone_number = "+14155550123"
signal_cli_rest_url = "http://localhost:8080"   # signal-cli-rest-api service
```

Signal integration requires running the [signal-cli-rest-api](https://github.com/bbernhard/signal-cli-rest-api) container locally — Signal has no official bot API, so we tunnel through `signal-cli`.

## iMessage (macOS only)

```toml
[channels.imessage]
enabled = true
provider = "linq"                  # Linq Partner API for iMessage/RCS/SMS
api_key = "..."
```

**macOS-only** and requires either Linq as a third-party relay, or direct AppleScript automation (experimental, requires Full Disk Access and Accessibility grants).

## WeCom (企业微信)

```toml
[channels.wecom]
enabled = true
corp_id = "..."
corp_secret = "..."
agent_id = 1000001
```

Chinese enterprise WeChat. Custom app required in the corp admin panel.

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
```

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

```toml
[channels.rocketchat]
enabled = true
server_url = "https://chat.example.com"   # any RC-compatible server (rocket.chat-cloud or self-hosted)
auth_token = "xxxxxxxxxxxxxxxxxxxxxxxx"   # SECRET — Personal Access Token, copy from RC web UI
user_id = "abc123"                         # Bot account `_id`, shown next to the token in the PAT dialog
allowed_users = ["alice"]                  # `*` allows anyone; usernames are matched case-insensitively
room_ids = ["GENERAL", "RID_DM_alice"]    # rooms to poll — DM, channel, or private group ids
poll_interval_secs = 10                    # default 10s; lower bound 2s
```

- **Auth model:** Personal Access Token. Mint via **My Account → Personal Access Tokens → Add**. Both the `auth_token` and the `user_id` shown in the dialog are required (RC sends them as `X-Auth-Token` and `X-User-Id` headers). No OAuth flow.
- **Inbound (polling):** for each `room_ids` entry, the agent polls `GET {server}/api/v1/chat.syncMessages?roomId={rid}&lastUpdate={iso8601}` every `poll_interval_secs`. The cursor is the most recent `_updatedAt` already dispatched. Filters drop the bot's own messages, anything outside `allowed_users`, system/bot-flagged posts, and message edits (agent ingestion is append-only).
- **Outbound:** `POST {server}/api/v1/chat.postMessage` with `roomId` + `text`. Bodies over ~4000 characters are split at sentence/word boundaries with a `(i/N) ` continuation marker; Rocket.Chat doesn't enforce a per-message ceiling, but very long messages render awkwardly in the UI.
- **Recipient format:** the room id directly (DMs, channels, and private groups all use the same `chat.postMessage` endpoint). The agent's runtime sets this automatically when responding to inbound messages.
- **Compatibility:** any Rocket.Chat-API-compatible server. Self-hosted instances with private TLS roots need their CA trusted at the OS level.
- **Rate budget:** default 10s × N rooms = 6N requests/min. Stay under your server's REST rate limit (default in RC is generous; admins can tune).
- **Out of scope (v1):** DDP/WebSocket realtime, outgoing-webhook receive, attachments, reactions, threads, edit propagation, mention-only gating, `request_approval()`. Polling is the simplest operator story; a follow-up can layer DDP for sub-second latency.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
