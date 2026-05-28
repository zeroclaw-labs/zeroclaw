# Webhooks

The `webhook` channel is a generic inbound/outbound HTTP adapter. It runs its own embedded HTTP server on a port you choose, accepts JSON-shaped messages, hands them to the agent, and (optionally) POSTs the agent's replies to a URL you specify. Use it as the universal adapter for any system that can produce an HTTP POST.

> **Not the same as the gateway's `/webhook` endpoint.** The gateway service has its own `POST /webhook` for paired clients hitting the agent over HTTP — that lives under `[gateway]` and is described in [Operations → Network deployment](../ops/network-deployment.md). This page documents the `[channels.webhook]` channel only.

## Configuration

```toml
[channels.webhook]
enabled = true
port = 8090                                     # TCP port the channel binds (0.0.0.0:{port})
listen_path = "/webhook"                        # path the embedded server listens on; default "/webhook"
send_url = "https://example.com/callback"       # optional outbound URL for agent replies
send_method = "POST"                            # "POST" (default) or "PUT"
auth_header = "Bearer s3cret"                   # optional Authorization header value for outbound requests
secret = "..."                                  # optional shared secret for inbound HMAC-SHA256 verification
```

Full field reference: [Config](../reference/config.md#channelswebhook).

## Inbound

The channel binds `0.0.0.0:{port}` and routes `POST {listen_path}`.

Request body (JSON):

```json
{
  "sender": "alice",
  "content": "Hello, agent.",
  "thread_id": "optional-conversation-id"
}
```

- `sender` — required, used as the message's sender identity.
- `content` — required, the user message handed to the agent. Empty content returns `400`.
- `thread_id` — optional. If set, the agent's reply targets the same thread; otherwise replies target `sender`.

Success returns `200 OK`. Malformed JSON or empty `content` returns `400`. Backpressure (channel queue full) returns `503`.

## Signature verification

When `secret` is set, every inbound request must carry an `X-Webhook-Signature` header:

```
X-Webhook-Signature: sha256=<hex-encoded HMAC-SHA256 of the raw body>
```

The channel computes `HMAC-SHA256(secret, raw_body)`, hex-encodes it, and compares against the header value (the `sha256=` prefix is stripped before decode). Mismatch or missing header returns `401`.

When `secret` is unset, **no verification runs** — every request is accepted. Don't expose an unsecured webhook channel to the public internet; either set `secret`, restrict access at a reverse proxy, or run the listener bound to a private network only.

## Outbound

When `send_url` is set, every agent reply is delivered as an HTTP request to that URL:

```
{send_method} {send_url}
Authorization: {auth_header}    # only if auth_header is set
Content-Type: application/json

{
  "content": "agent reply text",
  "thread_id": "optional thread id",
  "recipient": "optional recipient id"
}
```

- `send_method` is `POST` (default) or `PUT`. Any other value falls back to `POST`.
- `auth_header` is sent verbatim as the `Authorization` header value — include the scheme yourself (e.g. `Bearer xyz`, `Basic dXNlcjpwYXNz`).
- `recipient` is omitted when empty.
- Non-2xx responses raise an error in logs; the agent reply is considered failed.

When `send_url` is unset, agent replies are dropped silently (logged at `debug`). This is the right configuration for fire-and-forget inbound flows where the response is delivered through some other channel.

## Public exposure

The channel binds to `0.0.0.0` directly. To expose it on the public internet:

1. **Reverse proxy** — terminate TLS at nginx / Caddy / Traefik and proxy to the channel's port. See [Operations → Network deployment](../ops/network-deployment.md).
2. **Tunnel** — configure `[tunnel]` (`ngrok`, `cloudflare`, or `tailscale`) and the daemon brings up the tunnel alongside the channel.
3. **Local-only** — run inside a private network and have your producer hit the LAN/loopback address directly.

Always pair public exposure with `secret`. An unauthenticated webhook listener is an open ingress to the agent.

## Code

- Channel: `crates/zeroclaw-channels/src/webhook.rs`
- Config: `crates/zeroclaw-config/src/schema.rs` (`WebhookConfig`)

## See also

- [Operations → Network deployment](../ops/network-deployment.md) — TLS termination, tunnels, the gateway's separate `/webhook`
- [Channels → Overview](./overview.md)
