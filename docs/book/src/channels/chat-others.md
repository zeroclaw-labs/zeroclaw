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

## Sinch (SMS)

```toml
[channels.sinch]
enabled = false                    # disabled by default — opt in explicitly
service_plan_id = "..."            # public Sinch project ID
api_token = "..."                  # Bearer token for outbound (xms/v1/.../batches)
region = "us"                      # "us" or "eu" (data residency)
from_number = "+15555550100"       # E.164, provisioned in your Sinch console
allowed_numbers = ["+15555550199"] # exact match; "*" allows all
callback_secret = "..."            # HMAC-SHA256 secret for inbound webhooks
```

Sinch is a Twilio-sibling SMS gateway popular in the Nordics and APAC. Mirrors the [Twilio](#) gateway-registered pattern: this is a webhook-driven channel, not a polling one.

**Auth model.** Outbound requests use Bearer auth on `https://{region}.sms.api.sinch.com/xms/v1/{service_plan_id}/batches`. The `service_plan_id` identifies your Sinch project (public) and the `api_token` is the secret credential. Inbound webhooks are signed separately with `callback_secret` — do not reuse the API token.

**Inbound webhook.** Configure your Sinch service plan's "Callback URL" to point at:

```
https://{public-gateway-url}/sinch/sms
```

This must be reachable from the public internet, so you'll typically run one of the configured `[tunnel]` providers (Cloudflare Tunnel, Tailscale Funnel, ngrok, Pinggy, or a custom command). The gateway hosts `POST /sinch/sms` directly — there is no webhook subscription dance.

**Signature scheme.** Sinch sends an `x-sinch-webhook-signature` header in the format `v1,{nonce},{base64-sig}`. The signature is HMAC-SHA256 over `nonce_bytes || raw_body` keyed by `callback_secret`. The gateway verifies this header on every inbound request and returns `401` on mismatch — there is no fail-open path. The signature covers the body only, not the URL, so reverse-proxy host rewrites are harmless.

**Region selection.** `region = "us"` targets `us.sms.api.sinch.com`; `region = "eu"` targets `eu.sms.api.sinch.com`. Pick whichever matches the data residency configured for your Sinch project — the wrong region will return 401/404 even with valid credentials.

**Trial accounts.** Sinch trial credentials can only send to numbers you've verified in the Sinch dashboard. Add your own test number to the allowed list and verify it before expecting messages to land.

**Cost.** SMS pricing is per-segment and varies by destination country — long bodies are split into ≤1600-char chunks with a `(i/N)` continuation marker, and each chunk is billed separately. See Sinch's pricing page for current rates.

---

## When to prefer a dedicated guide

Channels with more intricate setup (OAuth flows, end-to-end encryption, multi-device considerations) live in their own pages:

- [Matrix](./matrix.md) — E2EE, device verification, Synapse/Dendrite specifics
- [Mattermost](./mattermost.md)
- [LINE](./line.md)
- [Nextcloud Talk](./nextcloud-talk.md)

If you run into configuration friction on any channel above, file an issue with the repro and we'll consider promoting it to a dedicated guide.
