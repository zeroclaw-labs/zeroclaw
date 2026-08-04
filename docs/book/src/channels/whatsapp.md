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
instance that should receive it: `GET`/`POST https://<your-public-url>/whatsapp/<alias>`
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

For Web mode, `dm_policy` and `group_policy` apply under **both** modes. `self_chat_mode` is personal-only:

| Field | Values | Applies under | Effect |
|---|---|---|---|
| `dm_policy` | `allowlist`, `ignore`, `all` | both modes | Controls direct messages |
| `group_policy` | `allowlist`, `ignore`, `all` | both modes | Controls group chats |
| `self_chat_mode` | `true`, `false` | personal only | Controls the user's self-chat |
| `mention_only` | `true`, `false` | both modes | Requires group messages to mention the bot |
| `passive_group_context` | `true`, `false` | both modes | Records allowed unaddressed group messages as context only |

`self_chat_mode` stays personal-only because it describes a personal account talking to itself.

The fromMe guard also stays inside the personal branch, but not because business mode lacks an equivalent. Business mode is still a WhatsApp Web linked-device session, and WhatsApp mirrors the operator's own outbound messages to linked devices as `fromMe` in either mode. The linked account is persisted as an authorized peer, so under business mode that mirror can satisfy the allowlist and reach dispatch, which is the shape #6353 closed for personal mode. That behaviour predates this change and is not introduced here; it is called out rather than asserted away, and repairing it is tracked separately.

### Compatibility note for `mode = "business"`

Business mode previously accepted `dm_policy` and `group_policy` and then never consulted either one, so a channel that read as restrictive answered every message it received. Both keys are now enforced under business mode.

`dm_policy` defaults to `allowlist`, so a business-mode deployment that relied on the previous permissive behavior must choose one of:

- **Keep answering everyone** - set `dm_policy = "all"` and `group_policy = "all"` explicitly.
- **Keep the restriction** - leave the defaults and make sure the senders you intend to serve are reachable through the channel's peer group, via `[peer_groups.<name>].external_peers`.

Do not wait for `config validate` to tell you this. Under `mode = "business"` it reports
`self_chat_mode` as inert and says nothing about `dm_policy` or `group_policy`, precisely because
those two are now live rather than inert. So the keys whose behaviour actually changed for you are
the ones the validator will not mention. Read this section before upgrading; that is the only
notice a business-mode deployment gets.

`passive_group_context = true` is opt-in and applies only to WhatsApp Web group chats. Allowed unaddressed group messages are stored in the room-scoped conversation history without starting an agent turn, sending reactions, downloading media, or calling the model. Later addressed messages in the same group can use that passive context.

## Restricting which groups (`allowed_groups`)

`allowed_groups` (Web mode) scopes the bot to a named set of group chats by JID. It is independent of `mode` - it applies in both business and personal mode, and runs before the chat-type policy. An empty list (the default) permits every group, so existing configs are unchanged. A non-empty list drops every group message whose chat JID matches no entry. **Direct messages always bypass this filter.**

Each entry matches either the full group JID (`123456789012345@g.us`) or the JID user part - the segment before `@` (`123456789012345`) - compared **exactly**, not as a string prefix (so `123` admits `123@g.us` but never `123999@g.us`). This gates group *identity*, which `group_policy` (chat type) and the sender allowlist (sender) do not.

```toml
[channels.whatsapp.myaccount]
enabled = true
session_path = "/var/lib/zeroclaw/wa.db"
# Only operate in these two groups; all other groups are dropped.
allowed_groups = ["120363012345678901@g.us", "120363098765432109"]
```

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
