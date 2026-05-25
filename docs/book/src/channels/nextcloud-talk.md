# Nextcloud Talk

Nextcloud Talk integration via the Talk Bot webhook protocol. Self-hosted, federated, and E2E-capable — another sovereign-communication option alongside [Matrix](./matrix.md) and [Mattermost](./mattermost.md).

## What this integration does

- Receives inbound Talk events via `POST /nextcloud-talk` on the gateway
- Verifies webhook signatures (HMAC-SHA256) when a secret is configured
- Sends replies back to Talk rooms via the Nextcloud OCS API

## Prerequisites

- **Nextcloud server** with the Talk app enabled (v17 or later recommended)
- **Bot account** in Talk settings — give it a display name (e.g. `zeroclaw-bot`) and generate a bot webhook URL
- **Webhook secret** from the Talk admin UI if you want signature verification (strongly recommended)
- **Publicly-reachable gateway** — see [Setup → Container](../setup/container.md) for tunnel options if self-hosted

## Configuration

```toml
[channels.nextcloud_talk]
enabled = true
server_url = "https://cloud.example.com"
bot_name = "zeroclaw-bot"                      # the display name of the bot in Talk
webhook_secret = "..."                         # shared secret from Talk admin UI
username = "..."                               # bot's Nextcloud login for outbound API
password = "..."                               # app password (not the account password)
allowed_rooms = ["ROOM-TOKEN-1"]               # tokens from webhook payloads
allowed_users = ["*"]                          # actor IDs; "*" = allow all (use for first-time test only)
```

Environment override: `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` takes precedence over the config value. Useful for rotating secrets without editing the config.

Full field reference: [Config](../reference/config.md).

## Gateway endpoint

```bash
zeroclaw daemon
```

Configure your Talk bot's webhook URL to point at:

```
https://<your-public-url>/nextcloud-talk
```

Local development? Configure `[tunnel]` in your config (ngrok, Cloudflare, or Tailscale) and the gateway exposes itself on startup — see [Operations → Network deployment](../ops/network-deployment.md).

## Signature verification

When `webhook_secret` is set, inbound requests must carry:

- `X-Nextcloud-Talk-Random` header
- `X-Nextcloud-Talk-Signature` header

ZeroClaw verifies:

```
expected_sig = hex(hmac_sha256(secret, random + raw_request_body))
if X-Nextcloud-Talk-Signature != expected_sig:
    return 401
```

Without a secret, no verification — don't expose this endpoint publicly in that mode.

## Message routing

- **Bot-originated events** (`actorType = "bots"`) are ignored — prevents feedback loops
- **System events** (joins, leaves, membership changes) are ignored
- **Non-message events** are ignored
- **User messages** are dispatched to the agent loop
- **Replies** go back to the originating room via the `token` in the webhook payload

## Quick validation

1. Set `allowed_users = ["*"]` for first-time testing
2. Send a test message in the configured Talk room
3. Confirm ZeroClaw receives and replies in the same room
4. Tighten `allowed_users` to explicit actor IDs (e.g. `["alice", "bob"]`)

## Troubleshooting

- **`404 Nextcloud Talk not configured`** — `[channels.nextcloud_talk]` section missing or `enabled = false`
- **`401 Invalid signature`** — secret mismatch, wrong random header, or body-signing bug. Check the raw body is being signed (not the parsed JSON)
- **No reply, webhook `200`** — event was filtered. Check logs for "actorType = bots" or "user not in allowed_users"
- **Replies delivered but look wrong** — check thread context; Talk replies are currently root-level only

## Streaming

Nextcloud Talk does not support message edits via the Bot API, so streaming draft updates are disabled for this channel. Replies are sent on stream completion only.

## Self-hosting notes

- TLS: terminate at your reverse proxy; webhook signature verification works over HTTP-to-container loopback
- The OCS API is authenticated via HTTP Basic — use an app password, not the account password
- Rate limits are Nextcloud-server dependent; the default bot doesn't run into them in normal conversation cadences

## See also

- [Matrix](./matrix.md) — richer E2EE but more operational complexity
- [Mattermost](./mattermost.md) — similar self-hosted posture, different protocol
- [Channels → Overview](./overview.md)
