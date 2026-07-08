# Nextcloud Talk

Nextcloud Talk integration via the Talk Bot webhook protocol. Self-hosted, federated, and E2E-capable: another sovereign-communication option alongside [Matrix](./matrix.md) and [Mattermost](./mattermost.md).

## Who can talk to the agent

{{#peer-group nextcloud}}

## What this integration does

- Receives inbound Talk events via `POST /nextcloud-talk/<alias>` on the gateway (bare `/nextcloud-talk` still works as a deprecated fallback)
- Verifies webhook signatures (HMAC-SHA256) when a secret is configured
- Sends replies back to Talk rooms via the Nextcloud OCS API

## Prerequisites

- **Nextcloud server** with the Talk app enabled (v17 or later recommended)
- **Bot account** in Talk settings, give it a display name (e.g. `zeroclaw-bot`)
- **Bot app token** from the Talk admin UI for OCS API bearer auth (used for outbound replies)
- **Webhook secret** from the Talk admin UI if you want signature verification (strongly recommended)
- **Publicly-reachable gateway**: see [Setup → Container](../setup/container.md) for tunnel options if self-hosted

## Configuration

{{#config-fields channels.nextcloud_talk}}

The channel is read from the `default` alias. Set it through any config surface:

{{#config-where channels nextcloud_talk}}

`webhook_secret` can also be supplied at runtime via the generic env override {{#env-var-name channels.nextcloud_talk.default.webhook_secret}}, useful for rotating it without editing the config.

## Gateway endpoint

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon
```

</div>

Configure your Talk bot's webhook URL to point at the alias of the
`[channels.nextcloud_talk.<alias>]` instance that should receive it:

`https://<your-public-url>/nextcloud-talk/<alias>`

For example, `[channels.nextcloud_talk.work]` receives `POST /nextcloud-talk/work`.
This per-alias routing (#6312) lets you run several Talk bots side by side and
deliver each one's webhooks to the right instance.

The bare `https://<your-public-url>/nextcloud-talk` path still works but is
**deprecated**: it resolves to the lexicographically-first alias (deterministic
across restarts) and returns an `X-Zeroclaw-Deprecation` response header.
Single-instance deployments can keep using it unchanged. An unknown alias returns `404`.

Local development? Configure `[tunnel]` in your config (ngrok, Cloudflare, or Tailscale) and the gateway exposes itself on startup: see [Operations → Network deployment](../ops/network-deployment.md).

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

Without a secret, no verification: don't expose this endpoint publicly in that mode.

## Message routing

- **Bot-originated events** (`actorType = "bots"`) are ignored: prevents feedback loops
- **System events** (joins, leaves, membership changes) are ignored
- **Non-message events** are ignored
- **User messages** are dispatched to the agent loop
- **Replies** go back to the originating room via the `token` in the webhook payload

## Quick validation

1. Set `external_peers = ["*"]` in the peer group for first-time testing
2. Send a test message in the configured Talk room
3. Confirm ZeroClaw receives and replies in the same room
4. Tighten the peer group to explicit actor IDs (e.g. `["alice", "bob"]`)

## Troubleshooting

- **`404 Nextcloud Talk not configured`**: `[channels.nextcloud_talk.default]` section missing or `enabled = false`
- **`401 Invalid signature`**: secret mismatch, wrong random header, or body-signing bug. Check the raw body is being signed (not the parsed JSON)
- **No reply, webhook `200`**: event was filtered. Check logs for "actorType = bots" or a sender not in the peer set
- **Replies delivered but look wrong**: check thread context; Talk replies are currently root-level only

## Streaming

Nextcloud Talk does not support message edits via the Bot API, so streaming draft updates are disabled for this channel. Replies are sent on stream completion only.

## Self-hosting notes

- TLS: terminate at your reverse proxy; webhook signature verification works over HTTP-to-container loopback
- The OCS API is authenticated via Bearer token: use the bot app token from the Talk admin UI
- Rate limits are Nextcloud-server dependent; the default bot doesn't run into them in normal conversation cadences
- Per-channel proxy: set `proxy_url` to override the global `[proxy]` setting for Nextcloud Talk only (`http://`, `https://`, `socks5://`, `socks5h://`)

## See also

- [Matrix](./matrix.md): richer E2EE but more operational complexity
- [Mattermost](./mattermost.md): similar self-hosted posture, different protocol
- [Channels → Overview](./overview.md)
