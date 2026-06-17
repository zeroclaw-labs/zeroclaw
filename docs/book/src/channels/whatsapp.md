# WhatsApp

ZeroClaw supports two WhatsApp backends under the same `channels.whatsapp` config family:

| Mode | Use it when | Required selector |
|---|---|---|
| WhatsApp Cloud API | You have a Meta Business app and WhatsApp Business phone number ID | `phone_number_id` |
| WhatsApp Web | You want to link a regular WhatsApp account through the Web protocol | `session_path` |

Do not configure both selectors in the same channel unless you intentionally want Cloud API mode to win for backward compatibility.

## Who can talk to the agent

{{#peer-group whatsapp}}

## Cloud API mode

Cloud API mode is the Meta Business Platform integration. It requires a Meta Business account, a WhatsApp Business app, a phone number ID, a verify token, and an access token. It is the right mode for business deployments that receive messages through Meta webhooks.

The gateway must be reachable by Meta for inbound webhooks. Configure a tunnel under the top-level `[tunnel]` section (`tunnel_provider` and the related provider blocks, see the [config reference](../reference/config.md#tunnel)), or front the gateway with your own reverse proxy when developing locally.

Point Meta's Callback URL at the alias of the `[channels.whatsapp.<alias>]`
instance that should receive it — `GET`/`POST https://<your-public-url>/whatsapp/<alias>`
(e.g. `[channels.whatsapp.work]` → `/whatsapp/work`). This per-alias routing
(#6312) lets multiple WhatsApp numbers run side by side. The bare `/whatsapp`
path still works but is **deprecated**: it resolves to the lexicographically-first
alias (deterministic across restarts) and sets an `X-Zeroclaw-Deprecation` response
header. An unknown alias returns `404`. Single-instance deployments need no change.

## Web mode

WhatsApp Web mode links a regular WhatsApp account through the optional Web backend. It does not need a Meta Business account. It does need a ZeroClaw build with the `whatsapp-web` feature enabled and a persistent session database path.

On first start, the Web backend pairs the account using QR or pair-code linking (`pair_phone` seeds pair-code linking; leave it unset for QR). Keep `session_path` on persistent storage; removing it forces a fresh device link. Bind the channel to an agent via that agent's `channels` list.

The shared `interrupt_on_new_message` option applies to both Cloud API mode and Web mode. When enabled, a newer WhatsApp message from the same sender/chat cancels the in-flight response.

## Personal and business behavior

For Web mode, `mode = "personal"` applies separate DM, group, and self-chat policies:

| Field | Values | Effect |
|---|---|---|
| `dm_policy` | `allowlist`, `ignore`, `all` | Controls direct messages |
| `group_policy` | `allowlist`, `ignore`, `all` | Controls group chats |
| `self_chat_mode` | `true`, `false` | Controls the user's self-chat |
| `mention_only` | `true`, `false` | Requires group messages to mention the bot |

The default `mode = "business"` does not apply the personal DM/group policy split. For peer-gated regular-account deployments, use `mode = "personal"` with `dm_policy = "allowlist"` and `group_policy = "allowlist"`.

## Configuration surfaces

{{#config-fields channels.whatsapp}}

{{#config-where channels whatsapp}}

{{#secret-config channels.whatsapp.<alias>.access_token}}

The same applies to `verify_token` and `app_secret` (Cloud API).

## Start and check

After configuring one mode, start the channel runner:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw channel start
```

</div>

Use `zeroclaw channel doctor` for a first check. For Web mode, also confirm the binary was built with `whatsapp-web`; for Cloud API mode, confirm the webhook tunnel and Meta verify token agree.
