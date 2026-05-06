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

## Twitch chat

```toml
[channels.twitch]
enabled = true
bot_username = "mybot"                # Twitch login of the bot account
oauth_token = "oauth:xxxxxxxxxxxx"    # SECRET — `oauth:` prefix added automatically if missing
channels = ["#mychannel", "anotherchannel"]   # `#` prefix added if missing; entries are lowercased
allowed_users = ["streamer", "moderator"]      # `*` allows anyone
mention_only = false                  # respond only when @-mentioned
```

- **Auth model:** Twitch chat is IRC-compatible. The OAuth token is sent as `PASS oauth:{token}` against `irc.chat.twitch.tv:6697` (TLS). Mint the token at <https://twitchapps.com/tmi/> for one-click setup, or via the Twitch CLI Device Code Flow if you need scope control.
- **Internals:** thin wrapper over the existing IRC channel (`channel-twitch` feature depends on `channel-irc`). All IRC-side logic — connect/reconnect, message splitting, nick collision handling — is shared with the plain IRC channel. The only differences are Twitch-specific defaults (server host, port, OAuth-style PASS, no SASL).
- **Channel name normalization:** Twitch channel names are case-insensitive Twitch logins; the adapter auto-prefixes `#` and lowercases each entry. Empty entries (e.g. trailing commas) are dropped.
- **Inbound:** every message in a joined channel arrives with `channel = "twitch"` so routing/auditing distinguishes it from plain IRC. The standard `allowed_users` allowlist applies (case-insensitive Twitch logins; `"*"` wildcard).
- **Outbound:** plain `PRIVMSG #channel :body`. Long messages are split by the IRC channel's existing chunker.
- **Out of scope (v1):** IRCv3 message tags (badges, color, msg-id, sub status), whispers (deprecated by Twitch over IRC; modern whispers use the Helix API), Twitch-specific commands (`/timeout`, `/ban`, `/announce`, `/raid`), subscription/bits/channel-points event handling, `request_approval()`. Defer to follow-up issues once we know what operators actually need.

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

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
