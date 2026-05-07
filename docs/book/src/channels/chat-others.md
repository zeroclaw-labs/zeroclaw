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

## Vonage SMS

```toml
[channels.vonage]
enabled = true
api_key = "ABCDEF12"                              # public — Vonage Dashboard → Account → API settings
api_secret = "xxxxxxxxxxxxxxxx"                   # SECRET — paired with api_key for outbound POSTs
from_number_or_sender_id = "+15555550100"         # E.164, short code, or alphanumeric sender ID
allowed_numbers = ["+15555550199"]                # `*` allows anyone (use with care; this is the public PSTN)
signature_secret = "yyyyyyyyyyyyyyyy"             # SECRET — separate from api_secret, configured in dashboard
```

- **Auth model:** Vonage uses **two distinct credentials**, both copied from the dashboard:
  - `api_secret` — the legacy SMS API password. Sent in the outbound POST body alongside `api_key` (Vonage's `/sms/json` endpoint takes credentials in the form body, not headers).
  - `signature_secret` — the inbound webhook HMAC key, configured separately under **API settings → Signed messages** with algorithm **HMAC SHA-256**. Mixing this up with `api_secret` is a common operator footgun (analogous to Telnyx's api_key vs. public_key) — they're two unrelated values.
- **Inbound endpoint:** ZeroClaw's gateway hosts `POST /vonage/sms` on its public URL. In the Vonage dashboard, set the **Inbound SMS Webhook** to `https://{your-public-gateway-url}/vonage/sms` with HTTP method **POST** and `application/x-www-form-urlencoded`.
- **Signature verification:** the gateway recomputes Vonage's `sig` parameter on every inbound webhook — HMAC-SHA256 over alphabetically-sorted form params + `signature_secret`, hex-encoded. Mismatches return 401 and the message is dropped before reaching the agent loop.
- **Outbound:** `POST https://rest.nexmo.com/sms/json` with form fields `api_key`, `api_secret`, `from`, `to`, `text`. Bodies over 1600 chars are split into ≤1600-char chunks at sentence/word boundaries with a `(i/N) ` continuation marker. Vonage's per-message status (in the JSON response) is also checked — `status != "0"` surfaces as an error.
- **Public exposure:** the gateway needs to be reachable from Vonage's public IP range. Use one of the `[tunnel]` providers — Cloudflare Tunnel, Tailscale Funnel, ngrok, Pinggy, or a custom command. The tunnel must terminate TLS on a stable hostname so the signature stays valid across reconnects (Vonage signs the URL it was configured with).
- **Trial accounts:** Vonage trial accounts can only message verified destinations. This is a Vonage constraint, not a ZeroClaw bug.
- **Cost:** every outbound SMS segment is billable to your Vonage account. The default `enabled = false` is intentional — opt in only after you've confirmed the configuration is right.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
