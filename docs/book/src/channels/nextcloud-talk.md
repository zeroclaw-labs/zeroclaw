# Nextcloud Talk

Nextcloud Talk integration via the Talk Bot webhook protocol. Self-hosted, federated, and E2E-capable: another sovereign-communication option alongside [Matrix](./matrix.md) and [Mattermost](./mattermost.md).

## Who can talk to the agent

{{#peer-group nextcloud}}

## What this integration does

- Receives inbound Talk events via `POST /nextcloud-talk/<alias>` on the gateway (bare `/nextcloud-talk` still works as a deprecated fallback)
- Requires and verifies webhook signatures (HMAC-SHA256) with the installed bot secret
- Sends replies back to Talk rooms via the signed Nextcloud Talk Bot API

## Prerequisites

- **Nextcloud server 27.1 or later with Talk 17.1 or later.** This is a hard
  minimum, not a recommendation: the signed Talk Bot API this integration uses to
  send replies was introduced in Talk 17.1, and `occ talk:bot:install` below is
  unavailable on earlier releases.
- **Bot installed** with both the `webhook` and `response` features, which let
  Nextcloud deliver room messages to ZeroClaw and let ZeroClaw send replies:

  ```sh
  sudo -u www-data php occ talk:bot:install \
    -f webhook -f response \
    zeroclaw-bot '<shared-secret>' \
    'https://<your-public-url>/nextcloud-talk/<alias>'
  ```
- **Bot secret** from that installation. Nextcloud issues **one** shared secret per
  bot, used both to verify inbound webhook signatures and to sign outbound bot-API
  replies. Set it as `webhook_secret`, which is canonical. `bot_token` is a
  **deprecated alias for the same value**: if both are set they must be identical.
  It cannot hold a different outbound secret. Conflicting non-empty values are
  **not** silently resolved in favour of one; the conflict is logged and the alias
  resolves to no secret, so the channel then behaves exactly as if unconfigured:
  inbound `401`, no outbound send.
- **Publicly-reachable gateway**: see [Setup → Container](../setup/container.md) for tunnel options if self-hosted

Both directions fail closed on a missing secret, and there is no unauthenticated
mode:

- **Inbound**: signature verification is **mandatory**. With no resolved secret the
  webhook endpoint returns `401` and never reaches the agent. There is no
  "public" mode that accepts unverified webhooks.
- **Outbound**: no request is sent at all, so misconfiguration never puts an
  unsigned or wrongly-signed request on the wire.

> **Upgrading is a breaking change.** A deployment that previously ran without a
> secret accepted webhooks; it now rejects every one of them with `401`. Install
> the bot with `occ talk:bot:install`, then set that secret as `webhook_secret`
> before upgrading, or inbound messages stop being processed.

## Configuration

{{#config-fields channels.nextcloud_talk}}

The channel is read from the `default` alias. Set it through any config surface:

{{#config-where channels nextcloud_talk}}

`webhook_secret` can also be supplied at runtime via the generic env override {{#env-var-name channels.nextcloud_talk.default.webhook_secret}}, useful for rotating it without editing the config.

`app_token` is deprecated and unused (replies no longer go through OCS bearer auth); it's only still accepted so old configs that set it don't fail to parse.

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

Inbound requests must carry:

- `X-Nextcloud-Talk-Random` header
- `X-Nextcloud-Talk-Signature` header

ZeroClaw verifies:

```
expected_sig = hex(hmac_sha256(secret, random + raw_request_body))
if X-Nextcloud-Talk-Signature != expected_sig:
    return 401
```

Without a resolved secret, ZeroClaw returns `401` before parsing or dispatching
the webhook. There is no mode that accepts an unverified request.

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
- Outbound replies authenticate via the Bot API's HMAC signature (`webhook_secret`/`bot_token`), not a bearer token; there is no separate OCS bearer credential to manage
- Rate limits are Nextcloud-server dependent; the default bot doesn't run into them in normal conversation cadences
- Per-channel proxy: set `proxy_url` to override the global `[proxy]` setting for Nextcloud Talk only (`http://`, `https://`, `socks5://`, `socks5h://`)

## See also

- [Matrix](./matrix.md): richer E2EE but more operational complexity
- [Mattermost](./mattermost.md): similar self-hosted posture, different protocol
- [Channels → Overview](./overview.md)
