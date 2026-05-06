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

## Telnyx (SMS)

```toml
[channels.telnyx]
enabled = false                          # default: off — opt in explicitly
api_key = "KEY01234..."                  # V2 API key — Bearer auth for outbound
from_number = "+15555550100"             # E.164 — must be provisioned in Telnyx
messaging_profile_id = "mp_..."          # optional — send via a Messaging Profile
allowed_numbers = ["+15555550199"]       # E.164 allowlist; "*" = anyone (careful!)
public_key = "MCowBQYDK2VwAyEA..."       # base64 Ed25519 public key — see below
```

Telnyx's Programmable Messaging is a Twilio sibling. Inbound traffic arrives via webhooks at the gateway-hosted path `POST /telnyx/sms` — point Telnyx's webhook URL at `https://{tunnel}/telnyx/sms`, behind one of the configured `[tunnel]` providers (Cloudflare Tunnel, Tailscale Funnel, ngrok, Pinggy, or a custom command). A public tunnel is required because Telnyx will not deliver to non-routable addresses.

**Two distinct credentials**, copied separately from the Telnyx portal — operators routinely conflate them, so the names matter:

* `api_key` is the **V2 API key**. Sent as `Authorization: Bearer ...` on every outbound REST call (`POST https://api.telnyx.com/v2/messages`). Treat as a secret.
* `public_key` is the **Ed25519 webhook public key**, base64-encoded. Used to verify the `telnyx-signature-ed25519` header on inbound webhooks. Public, but copied from a different page in the portal than the API key. **When Telnyx rotates the signing key (or you provision a new portal user), you must update `public_key` here** — otherwise inbound webhooks will start failing signature verification.

Webhooks are signed with Ed25519 over the bytes `{telnyx-timestamp}|{raw_body}` (literal pipe separator). The gateway also enforces a **5-minute timestamp anti-replay window**: inbound requests with a `telnyx-timestamp` more than 300 seconds away from the gateway's clock are rejected with 401, even if the signature itself is valid. Keep your gateway host's clock in sync (NTP).

If your Telnyx account is on a trial plan, outbound numbers must be verified in the portal first — otherwise `POST /v2/messages` returns 4xx and the gateway logs the failure. SMS is metered per segment; long agent replies are split into ≤1600-char chunks with a `(i/N)` continuation marker, so a single agent turn can produce multiple billable segments.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
