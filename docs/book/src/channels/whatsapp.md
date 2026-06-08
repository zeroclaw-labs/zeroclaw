# WhatsApp

ZeroClaw supports two WhatsApp backends under the same `channels.whatsapp` config family:

| Mode | Use it when | Required selector |
|---|---|---|
| WhatsApp Cloud API | You have a Meta Business app and WhatsApp Business phone number ID | `phone_number_id` |
| WhatsApp Web | You want to link a regular WhatsApp account through the Web protocol | `session_path` |

Do not configure both selectors in the same channel unless you intentionally want Cloud API mode to win for backward compatibility.

## Cloud API mode

Cloud API mode is the Meta Business Platform integration. It requires a Meta Business account, a WhatsApp Business app, a phone number ID, a verify token, and an access token. It is the right mode for business deployments that receive messages through Meta webhooks.

```toml
[channels.whatsapp.default]
enabled = true
phone_number_id = "<meta-phone-number-id>"
verify_token = "<webhook-verify-token>"
access_token = "<meta-access-token>"
# app_secret = "<meta-app-secret>" # recommended for webhook signature verification
```

The gateway must be reachable by Meta for inbound webhooks. Configure a tunnel via `zeroclaw config set gateway.tunnel.provider=<ngrok|cloudflare>` and the related `gateway.tunnel.*` fields (see the [Config reference](../reference/config.md)), or front the gateway with your own reverse proxy when developing locally.

## Web mode

WhatsApp Web mode links a regular WhatsApp account through the optional Web backend. It does not need a Meta Business account. It does need a ZeroClaw build with the `whatsapp-web` feature enabled and a persistent session database path.

```toml
[channels.whatsapp.default]
enabled = true
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
mode = "personal"
dm_policy = "allowlist"
group_policy = "allowlist"
mention_only = true

[agents.assistant]
enabled = true
channels = ["whatsapp.default"]
```

On first start, the Web backend pairs the account using QR or pair-code linking. `pair_phone` can seed pair-code linking, but leave it unset if you want QR pairing:

```toml
[channels.whatsapp.default]
pair_phone = "<country-code-and-number-without-plus>"
```

Keep `session_path` on persistent storage. Removing it forces a fresh device link.

The `channels` entry binds the channel alias to the agent that should answer it. Use your real agent alias instead of `assistant`.

## Personal and business behavior

For Web mode, `mode = "personal"` applies separate DM, group, and self-chat policies:

| Field | Values | Effect |
|---|---|---|
| `dm_policy` | `allowlist`, `ignore`, `all` | Controls direct messages |
| `group_policy` | `allowlist`, `ignore`, `all` | Controls group chats |
| `self_chat_mode` | `true`, `false` | Controls the user's self-chat |
| `mention_only` | `true`, `false` | Requires group messages to mention the bot |

The default `mode = "business"` does not apply the personal DM/group policy split. For peer-gated regular-account deployments, use `mode = "personal"` with `dm_policy = "allowlist"` and `group_policy = "allowlist"`.

## Restrict who can talk to the agent

Inbound peer authorization lives in `peer_groups`. A group can target every WhatsApp alias with `channel = "whatsapp"` or one alias with `channel = "whatsapp.default"`.

```toml
[peer_groups.whatsapp_ops]
channel = "whatsapp.default"
agents = []
external_peers = ["<allowed-whatsapp-peer>"]
ignore = []
```

Use the peer identifier shape that the active backend reports. Cloud API usually reports sender phone identifiers from the webhook payload. Web mode may report chat or JID-shaped identifiers. Keep examples and fixtures neutral; do not commit real phone numbers, account IDs, or chat IDs.

## Configuring from the CLI

Prefer `zeroclaw config set` for WhatsApp fields:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw config set channels.whatsapp.default.session-path ~/.zeroclaw/state/whatsapp-web/session.db
```

</div>

`zeroclaw channel add <type> <CONFIG>` is not the recommended setup path for WhatsApp. It takes a JSON object at the CLI layer, but current channel setup is routed through `zeroclaw config set` so secret handling, pairing, and peer authorization stay explicit.

## Start and check

After configuring one mode, start the channel runner:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw channel start
```

</div>

Use `zeroclaw channel doctor` for a first check. For Web mode, also confirm the binary was built with `whatsapp-web`; for Cloud API mode, confirm the webhook tunnel and Meta verify token agree.
