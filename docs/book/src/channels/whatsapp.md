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

Cloud API mode is the Meta Business Platform integration. It requires a Meta Business account, a WhatsApp Business app, a phone number ID, a verify token, an access token, and an app secret. It is the right mode for business deployments that receive messages through Meta webhooks.

Inbound webhooks are signature-verified against `app_secret`, and verification is mandatory. With no app secret configured the gateway cannot verify a request, so it answers `401` and processes nothing. Set `app_secret` before pointing Meta at the callback URL.

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

For Web mode, `mode = "personal"` applies separate DM, group, and self-chat policies:

| Field | Values | Effect |
|---|---|---|
| `dm_policy` | `allowlist`, `ignore`, `all` | Controls direct messages |
| `group_policy` | `allowlist`, `ignore`, `all` | Controls group chats |
| `self_chat_mode` | `true`, `false` | Controls the user's self-chat |
| `mention_only` | `true`, `false` | Requires group messages to mention the bot |
| `passive_group_context` | `true`, `false` | Records allowed unaddressed group messages as context only |

The default `mode = "business"` does not apply the personal DM/group policy split. For peer-gated regular-account deployments, use `mode = "personal"` with `dm_policy = "allowlist"` and `group_policy = "allowlist"`.

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

## Tool approval over chat (`approval_timeout_secs`)

When a tool needs approval (it is in `always_ask`, or the risk profile does not
auto-approve it), the agent posts the request into the chat the message came from
and waits for a reply. Answer with the token from the prompt:

```
a1b2c3 yes
a1b2c3 no
a1b2c3 always
```

`approval_timeout_secs` bounds that wait. **The default is 300 seconds, and `0`
denies immediately** rather than disabling approval, so a zero is a way to refuse
every gated tool, not a way to wait forever. On timeout the request is denied and
the token is discarded, so a late reply cannot approve a call nobody is waiting
on any more.

**Who may answer.** The token is a correlator, not a password: it travels in
plaintext into the chat, so in a group every member can read it.

**The two modes differ, and the difference is a security boundary rather than an
implementation detail.**

In **Web mode**, a reply is honoured only when it comes from the same chat the
prompt was posted into **and** from a peer this alias is authorized to take
instructions from. A reply that fails either check is logged and ignored, and
the request stays open so the operator can still answer it. In a group the
prompt says so, because otherwise there is no way to tell why a bystander's
reply did nothing.

The authorized peers are the ones the canonical resolver returns for this
alias, which is the peer group whose `channel` points at it:

```toml
[peer_groups.whatsapp_default]
channel = "whatsapp.personal"   # this alias only; bare "whatsapp" covers every alias
external_peers = ["+15550100"]
```

There is no `allowed_numbers` field to set. That was the v2 spelling, and
migration folds it into a peer group like the one above, so a v2 config keeps
working and a v3 config has nowhere to put the old key.

See [Peer groups](./peer-groups.md) for the full field list and the identifier
shape each channel matches against.

In **Cloud API mode**, neither check is applied. Its pending entry is a bare
responder keyed by the token, with no chat and no identity recorded alongside
it, so the webhook treats possession of the token as authority. In a group that
means any member who can read the prompt can answer it, including from a
different chat. Until that path is hardened, a Cloud-mode approval is proof
that someone possessed the token and nothing more. It authenticates neither the
chat nor the responder, so prefer Web mode wherever either matters.

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
