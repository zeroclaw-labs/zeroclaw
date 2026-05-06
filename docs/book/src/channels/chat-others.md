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

## Zulip

```toml
[channels.zulip]
enabled = true
server_url = "https://yourorg.zulipchat.com"   # or self-hosted https://chat.example.com
bot_email = "agent-bot@yourorg.zulipchat.com"  # bot account email
api_key = "xxxxxxxxxxxxxxxxxxxxxxxx"           # SECRET — bot API key from PSelf settings
allowed_users = ["alice@example.com"]          # `*` allows anyone; case-insensitive
streams = ["Engineering", "general"]           # streams the bot is subscribed to
default_topic = "agent"                         # used when sending to a stream without explicit topic
event_timeout_secs = 60                         # long-poll timeout (default 60s)
```

- **Auth model:** bot account email + API key, sent as HTTP Basic auth on every request. Mint via **Personal settings → Bots → Add a new bot**, then copy both the bot email and the API key shown in the dialog.
- **Bot subscription:** the bot **must be subscribed** to each stream listed in `streams`. v1 does not auto-subscribe — handle it once in the Zulip web UI when you add the bot. If `streams` is empty, the channel narrows to private-message events only.
- **Inbound (long-poll Events API):** `POST {server}/api/v1/register` mints a queue, then `GET {server}/api/v1/events?queue_id=…&last_event_id=…` blocks for up to `event_timeout_secs`. The cursor advances on every successful poll; `BAD_EVENT_QUEUE_ID` triggers a re-register. Filters drop the bot's own posts, anything outside `allowed_users`, edits, and unsupported message types.
- **Outbound recipient encoding:** the agent's runtime sets this automatically when responding, but for manual `zeroclaw channel send zulip <recipient> "hi"`:
  - `stream:Stream Name` — sends to a stream. Topic comes from `--thread` if set, otherwise `default_topic`.
  - `stream:Stream Name/Topic` — explicit topic in the recipient.
  - `private:user@example.com,user2@example.com` — DM (multi-party supported).
  - Bare email — DM shorthand.
- **Self-hosted TLS:** common in self-hosted installs. The default `reqwest` client honours system trust roots; private CAs need to be installed at the OS level.
- **Out of scope (v1):** rich Zulip markdown features (mentions parsing, code-block hints, custom emoji, math), reactions, message edits, file uploads, polls, scheduled messages, automatic stream subscription management, mention-only gating beyond the `allowed_users` allowlist, `request_approval()` integration.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
