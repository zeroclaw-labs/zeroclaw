# Webhooks

A generic inbound HTTP channel. Any service that can POST a JSON payload (GitHub, Linear, Sentry, Zapier, cron-job.org, your own scripts) can hand work to the agent via a webhook URL.

Webhooks live inside the gateway — if the gateway is running, webhooks are reachable at `/webhook/<name>`.

## Configuration

```toml
[channels.webhooks.github-issues]
enabled = true
secret = "..."                    # HMAC secret for signature verification
dispatch_to = "sop:triage-issue"  # or a conversation ID, or a system prompt
allow_list = ["github.com"]       # enforce source IP allowlist

[channels.webhooks.grafana]
enabled = true
secret = "..."
dispatch_to = "cron:alert-handler"
```

The `dispatch_to` field points the inbound event at one of:

- `sop:<name>` — runs a Standard Operating Procedure with the payload as input
- `cron:<name>` — triggers a scheduled job off-schedule
- `conversation:<id>` — appends to a named conversation (agent responds normally)
- `system:<prompt>` — runs a one-shot agent call with the payload appended to a system prompt

## Signature verification

GitHub, Stripe, Slack, and most major sources sign webhooks with HMAC. Set `secret` to the shared secret; the channel verifies `X-Hub-Signature-256` / `X-Signature` / equivalent before dispatching.

Sources without signing (open webhooks) should at minimum set `allow_list` to restrict by source IP. A webhook URL with no auth and no IP allowlist is an open ingress — don't.

## Shape of what the agent sees

By default the agent receives the raw JSON payload as a user message. For structured sources, use `template`:

```toml
[channels.webhooks.sentry]
enabled = true
secret = "..."
dispatch_to = "sop:investigate-error"
template = """
New Sentry alert:
Level: {{ payload.level }}
Title: {{ payload.title }}
URL: {{ payload.web_url }}

Stack:
{{ payload.exception.values[0].stacktrace }}
"""
```

Templates use MiniJinja; `payload` is the full JSON body, plus `headers` and `query` for URL params.

## Replies

Webhook channels default to one-shot: the agent handles the event, writes to the receipts log, and returns 200. For webhook sources that wait for a reply (interactive bots, slash commands), set `reply_mode`:

```toml
[channels.webhooks.slack-slash]
enabled = true
secret = "..."
reply_mode = "inline-json"        # respond to POST with {"text": "..."}
template = "{{ payload.text }}"
```

Supported reply modes: `status-only` (default, 200 OK), `inline-json`, `inline-text`.

## Public exposure

By default the gateway binds to localhost only. To expose webhooks publicly:

1. **Reverse proxy** — run nginx / Caddy / Traefik in front, terminate TLS, proxy to localhost. See [Operations → Network deployment](../ops/network-deployment.md).
2. **Tunnel** — configure `[tunnel] provider = "ngrok" | "cloudflare" | "tailscale"` and restart the daemon. Tunnels start alongside the gateway.
3. **Public bind** — set `ZEROCLAW_ALLOW_PUBLIC_BIND=1` and `[gateway] host = "0.0.0.0"`. Not recommended without a firewall in front.

## Rate limiting

The webhook channel applies a per-webhook-name rate limit (default 10/s). Override:

```toml
[channels.webhooks.noisy]
rate_limit_per_sec = 50
```

Excess requests return 429.

## Code

- Channel: `crates/zeroclaw-channels/src/webhook.rs` (plus gateway ingress in `crates/zeroclaw-gateway/`)
- Templates: `crates/zeroclaw-runtime/src/templating.rs`

## See also

- [Operations → Network deployment](../ops/network-deployment.md)
- [SOP → Connectivity](../sop/connectivity.md) — webhook + SOP patterns
