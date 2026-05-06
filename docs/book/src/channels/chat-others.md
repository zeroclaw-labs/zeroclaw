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

---

## Plivo (SMS)

```toml
[channels.plivo]
enabled = false                    # default; flip to true once configured
account_id = "MAxxxxxxxxxxxxxxxx"   # Plivo Auth ID (public; not secret)
auth_token = "..."                  # Plivo Auth Token (secret — keeps both
                                    # outbound Basic auth and the inbound
                                    # webhook HMAC key)
from_number = "+15555550100"        # E.164 number leased from Plivo
allowed_numbers = ["+15555550199"]  # E.164 inbound allowlist; "*" allows all
```

**Auth model.** Plivo issues an Auth ID + Auth Token pair per account. The Auth ID is public and goes in the URL; the Auth Token is secret and is reused as both the HTTP Basic password (outbound `Message` POST) and the HMAC-SHA256 key for inbound webhook signature verification.

**Inbound webhook.** The gateway hosts `POST /plivo/sms`. Configure your Plivo Application's "Message URL" to your gateway's public URL — for example `https://your-tunnel.example.com/plivo/sms`. The gateway needs to be reachable from the public internet; the existing options are [Cloudflare Tunnel](../tunnels/cloudflare.md), ngrok, or Tailscale Funnel. Localhost-only deployments cannot receive Plivo webhooks.

**Signature scheme.** Plivo uses signature version 3 (V3): the gateway computes `HMAC-SHA256(URL || nonce || raw_body)` keyed by the Auth Token, base64-encodes the digest, and constant-time compares against the `X-Plivo-Signature-V3` header. The nonce is delivered in `X-Plivo-Signature-V3-Nonce` and ties each request to a specific signature so replays cannot be reused. Failed verification returns `401` and the message never reaches the agent.

**Allowlist.** `allowed_numbers` is an exact-match E.164 allowlist (whitespace stripped, case-insensitive). Use `"*"` to accept every inbound sender — only set this if the number receives traffic exclusively from people you trust to talk to the agent.

**Trial accounts.** Plivo trial accounts can only send to numbers verified in the Plivo console. To agent-reply during trial, verify the inbound numbers you expect first; otherwise the outbound `Message` POST will fail with a 4xx and the gateway will log the error.

**Cost.** Plivo charges per outbound message segment (typically `$0.0035–$0.0085` US, varies by destination country) and per inbound message. Outbound replies that exceed 1600 characters are split into `(i/N)`-prefixed chunks before send and each chunk is billed as a separate segment.

**Default `enabled = false`.** As with every webhook-driven channel, Plivo stays inert until you set `enabled = true` and the gateway is reachable. The `[channels.plivo]` section can be present in config without affecting runtime as long as `enabled` stays false.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
