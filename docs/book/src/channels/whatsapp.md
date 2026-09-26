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

`push_name` sets the display name recipients see; leave it unset and the account keeps the name the phone was registered with. It is applied on connect, only when it differs from the name the linked device already carries, and a failure to apply it is logged without stopping the channel. Cloud API mode ignores it; there the display name comes from the Meta Business profile.

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

`self_chat_mode` stays personal-only because the self-chat affordance is scoped to the personal branch by design. `mode` selects ZeroClaw's policy posture, not a WhatsApp account type: both modes drive the same linked-device session.

WhatsApp mirrors the linked account's outbound messages as `fromMe` events in either mode. Business mode drops these echoes before approval-reply handling or user-message dispatch, so they cannot start another agent turn. Personal mode retains its intentional self-chat and explicit operator-trigger handling; this business-mode rule does not remove those exceptions.

Admitted WhatsApp Web one-to-one messages, identified by the `@s.whatsapp.net` or `@lid` chat domain, bypass the reply-intent classifier even when precheck is enabled. Channel access policies still apply before dispatch, and bypassing this classifier does not guarantee a reply. Group and other chat domains do not receive this direct-message exemption; other existing bypass conditions, such as an explicit mention, remain unchanged. Business-mode `fromMe` echoes are dropped before they can receive the exemption.

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

`allowed_groups` (Web mode) scopes the bot to a named set of group chats by JID. It is independent of `mode` - it applies in both business and personal mode, and runs before the chat-type policy. An empty list is **not** permission: what it means is decided by `group_policy`. Under `allowlist` (the default) or `ignore` an empty list admits no group, and under `all` it admits every group. A list that admits everything cannot be told apart from a list nobody configured, so open group access has to be asked for by name. A non-empty list drops every group message whose chat JID matches no entry, and keeps doing so under every policy including `all`, so `all` widens the empty-list default rather than overriding an explicit list. **Direct messages always bypass this filter.**

Each entry matches either the full group JID (`123456789012345@g.us`) or the JID user part - the segment before `@` (`123456789012345`) - compared **exactly**, not as a string prefix (so `123` admits `123@g.us` but never `123999@g.us`). This gates group *identity*, which `group_policy` (chat type) and the sender allowlist (sender) do not.

```toml
[channels.whatsapp.myaccount]
enabled = true
session_path = "/var/lib/zeroclaw/wa.db"
# Only operate in these two groups; all other groups are dropped.
allowed_groups = ["120363012345678901@g.us", "120363098765432109"]
```

## Polls

The `poll` tool posts a native WhatsApp poll in Web mode instead of the numbered text fallback used on channels without native polls. The tool accepts 2–10 options; the WhatsApp library accepts up to 12, but the tool schema does not expose 11–12. `multi_select` lets a voter select multiple options.

Raw phone-number recipients are checked against the channel's number allowlist, and a disallowed number returns an error instead of silently doing nothing. As with ordinary sends, JID recipients bypass that number check. `duration_minutes` does not expire a native poll.

Votes are not read back yet: the poll card shows the result to people in the chat, and the agent only learns that the poll was posted.

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

## Voice notes

In Web mode the channel reads voice notes and can answer with one. Both halves
are off until they are configured, and each is configured separately.

### Reading them

A voice note is transcribed when `[transcription]` is enabled and a
transcription provider is configured. One provider is enough on its own: it is
bound without any per-agent setting. With several configured, the owning
agent's `transcription_provider` chooses between them, and an agent that names
none leaves the choice unbound rather than routing audio to whichever vendor
came first, so the note arrives untranscribed and the reason is logged.

The transcript reaches the agent as the message content, prefixed `[Voice] `,
so a spoken message and a typed one are told apart in the history without a
second field.

- A note longer than `transcription.max_duration_secs` is skipped, and so is
  one whose download or transcription fails, or whose transcript comes back
  empty. The message then carries the ordinary media placeholder instead, and
  the failure is logged.
- Only voice notes are read. Other audio, such as a forwarded clip or a music
  file, is ignored unless `transcription.transcribe_non_ptt_audio = true`.

### Answering with one

Speaking a reply needs `tts.enabled` and a `tts_provider` on the owning agent.
An agent with no provider named stays silent rather than picking one, and says
so in the log.

**A chat is answered by voice once a voice note from it has been transcribed.**
Receiving the audio is not enough: a note that is skipped, that fails to
download, or that comes back with an empty transcript leaves the chat as it
was. A message that never reaches this point at all, because a policy dropped
it or because it is passive group context, leaves it alone too. With
`transcribe_non_ptt_audio` on, other transcribed audio sets it as well.

The state is per chat, not per sender, and the next processed message that is
not transcribed audio clears it, so a conversation drops back to text once
someone types. In a group that means whoever spoke last decides, and the spoken
reply is audible to the whole group.

When the reply is spoken, the chat gets **both**: the text as usual, and a
voice note alongside it. The chat keeps a readable record, and nothing is only
available as audio.

Four things shape the voice note itself:

- It is sent as a real voice note (`ptt`), Opus-encoded, not as an audio
  attachment, so it renders as the waveform bubble rather than a file card.
- What is spoken is the last eligible reply the chat produced, after about ten
  quiet seconds. The queue holds one reply per chat and each new one replaces
  the last, so an answer that arrives in several parts is not read out in full:
  the part that was queued when the pause ran out is the part that is spoken.
  Nothing tells the channel that an answer is complete.
- Typing does not call back a reply already queued. Clearing the chat's state
  stops the *next* reply from being queued; the task already waiting never
  looks at that state again, and hands its reply to the synthesizer regardless.
  So a message typed during those ten seconds is answered in text, while the
  reply that was already waiting is still spoken.
- Replies that do not read well aloud stay text-only: anything of 40 bytes or
  fewer, or that begins with a URL, a JSON document, a bracketed marker or
  `Error`, or that carries a code fence, tool-call markup or raw weather-tool
  output. These are shapes, not settings; a reply is judged by what it looks
  like.

### What does not apply here

The peer-group `output_modality` setting does not drive the automatic voice
reply. It is read for Matrix and Telegram, where a group can be declared
`voice`, `text` or `mirror`; on WhatsApp what decides is the per-chat rule
above, and declaring a peer group `text` does not switch it off.

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
