# Design: Microsoft Teams Bot Channel (`channel-msteams`)

- Status: implemented
- Date: 2026-07-17 (revised after PR review)
- Scope: plain-text send/receive, inbound JWT validation, @mention
  gating, DM policy, sender allowlist, typing indicators, and outbound
  chunking for Teams' per-message size limit. Progressive delivery — both
  Teams' native streaming bubble and paragraph-per-message — is deliberately
  **not** offered; §"Why the streaming protocol is not used" records why
- Risk tier: High. The change-risk routing in
  [agent-guidelines](book/src/contributing/agent-guidelines.md#stability-and-risk)
  classifies trust-boundary and `.github/workflows/` changes as High, and
  this PR is both: it adds a new inbound authentication boundary (the
  listener authenticates Bot Connector JWTs and holds the bot's Connector
  credential) and adds a required CI check (the Teams entry in the
  `test-channel-features` matrix). No existing boundary is
  weakened, but the new one gets focused review.
- Reference implementations studied:
  - OpenClaw `extensions/msteams/` at `db3213264a` (TypeScript, Bot
    Framework model) — primary architectural reference
  - `osodevops/ms-teams-cli` (Rust, Graph API delegated-auth model) —
    Rust-level reference for OAuth token flows only; its auth model is
    explicitly NOT suitable for unattended bot messaging (its own
    `docs/auth.md` says bot mode is the correct direction for that)
- Controlling Microsoft specifications:
  - [Bot Connector authentication](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-authentication?view=azure-bot-service-4.0):
    the inbound JWT contract (issuer, audience, signed `serviceUrl`, key
    endorsements, clock skew)
  - [Send and receive messages](https://learn.microsoft.com/en-us/azure/bot-service/rest-api/bot-framework-rest-connector-send-and-receive-messages?view=azure-bot-service-4.0):
    Connector activity POST and reply addressing
  - [Stream bot messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/streaming-ux):
    Teams native streaming — the protocol this channel does **not** use. Cited
    for the constraints that rule it out: every frame carries the whole
    response so far and may only extend the previous frame, plus a 1:1-only
    limitation and a 1 request/s throttle
  - [Format your bot messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/format-your-bot-messages):
    the per-activity size limit that drives outbound chunking
  - [Rate limiting for bots](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/rate-limit):
    the per-conversation and per-tenant send quotas, and the required
    backoff on `429`

## 1. Problem

ZeroClaw has 30+ channels but no Microsoft Teams support. The
`microsoft365` module in `zeroclaw-tools` is a Tool (Graph API for
mail/calendar), not a Channel — it cannot receive or send Teams chat
messages as a bot.

### Packaging model: native now, plugin later

[ADR-006](book/src/architecture/decisions/ADR-006-runtime-channel-plugins.md)
makes runtime-installable plugins the target packaging model for optional
channels, and [#8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850)
owns migration sequencing and capability-gap tracking. ADR-006 permits a
native implementation only as an explicit exception that names the missing
host capability or operational constraint, the native code path depending
on it, and the condition that permits migration.

Inbound ingress is no longer the gap.
[#8862](https://github.com/zeroclaw-labs/zeroclaw/pull/8862) and
[#8949](https://github.com/zeroclaw-labs/zeroclaw/pull/8949) added the
`webhook-ingress` capability to `wit/v0/channel.wit`: the gateway hosts
`/plugin/{path}` and hands `parse-webhook` the method, query, headers
(including `Authorization`) and exact body. A Teams plugin could therefore
verify the Bot Framework token and bind the activity to it inside the
component. Receiving webhooks does not make a plugin equivalent to this
channel, though. For Teams the exception is:

- **Remaining operational constraints**:
  - *Channel-addressed tool delivery.* Plugin channels are constructed
    asynchronously, so the synchronous `build_channel_map` and
    `register_channels_for_tools` surfaces cannot see them, and tools that
    address a channel by name cannot target a plugin channel yet. Teams is
    reachable from those tools as a native channel.
  - *Supported distribution and configuration.*
    [#8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850) still
    tracks feeding plugins the canonical channel config, native-or-plugin
    provider selection and active routing, publishing compatible plugin
    packages, and shipping a supported WASM-enabled artifact.
    [#10996](https://github.com/zeroclaw-labs/zeroclaw/issues/10996) tracks
    seeding the channel instance configuration and egress grants during
    plugin installation. Until those land, operators have no supported path
    to install and configure a Teams plugin.
- **Native code path that depends on them**: the orchestrator constructs
  `MsTeamsChannel` in `build_channel_by_id` and
  `collect_configured_channels` and reaches it from `deliver_announcement`,
  `listing.rs` lists it, and `MsTeamsChannel::listen()` hosts the axum
  `/api/messages` route, where `bind_activity_to_claims()` authenticates each
  request against the signed token before any state is recorded.
- **Condition that permits migration**: once channel-addressed tools can
  target plugin channels and the #8850 and #10996 work gives operators a
  supported install and configuration path, this channel can move to a
  plugin without protocol changes. `auth.rs`, `activity.rs`, and
  `conversation.rs` already hold no axum types; only `mod.rs` does. That
  port is separate work and is not part of this change.

## 2. Decision summary

| Decision | Choice |
| --- | --- |
| Protocol model | Azure Bot Service / Bot Framework (not Graph change notifications) |
| HTTP ingress | Channel-hosted axum server inside `Channel::listen()`, same pattern as `webhook.rs`. `zeroclaw-gateway` binary untouched. |
| Tenancy | Single-tenant bot (`tenant_id` required). Multi-tenant deferred. |
| ConversationReference storage | In-memory only. After daemon restart, proactive sends fail until the peer messages the bot again. Persistence deferred. |
| Inbound auth | Validate `Authorization: Bearer <JWT>` against Bot Framework JWKS. Reject before body processing. |
| Outbound auth | OAuth2 client-credentials against Entra, scope `https://api.botframework.com/.default`, token cached until expiry or until the credentials it was minted for change. |
| Feature flag | `channel-msteams` in `zeroclaw-channels` |
| DM policy | Configurable via `allow_dms` (default `true`). When `false`, inbound personal-chat messages are dropped. |
| Progressive delivery | Not offered. One reply per turn, posted when the answer is complete; personal and group chats show a typing indicator while it runs. Teams' native streaming protocol requires each frame to carry the whole response so far and to only extend the last one, which cannot be reconciled with redacting a credential the model emits across several frames. §"Why the streaming protocol is not used" carries the argument. Paragraph-per-message delivery is out for a related reason — each paragraph is a permanent message drawn from text the outbound leak policy has not run over. |
| Outbound size limit | Teams rejects a single activity past ~100 KB with `413` (`MessageSizeTooBig`), so `send()` splits oversize replies into ordered chunks at paragraph/line/word boundaries. |
| Not supported | Media attachments, Adaptive Cards, SSO, polls, file consent, reactions, message delete |

## 3. Protocol overview

Operator-side prerequisites (done by the operator, not by ZeroClaw):

1. Create an Azure Bot resource + Entra app registration → obtain
   **App ID**, **client secret**, **Tenant ID**.
2. Set the bot messaging endpoint to `https://<domain>/api/messages`
   (operator provides domain/reverse proxy to the configured port).
3. Enable the Microsoft Teams channel on the Azure Bot.
4. Sideload a minimal Teams app manifest (`botId` = App ID).

### Inbound (Teams → ZeroClaw)

```
Teams POSTs an Activity JSON to /api/messages
  with header: Authorization: Bearer <JWT>
  ├─ validate JWT (reject 401 before touching the body): RS256 signature
  │  via JWKS, aud == app_id, iss == the Bot Framework issuer only,
  │  exp and nbf with a 300s clock-skew allowance, the signing key's
  │  channel endorsements cover activity.channelId, and the signed
  │  serviceurl claim matches the activity's serviceUrl
  ├─ only activity.type == "message" produces a ChannelMessage
  ├─ record ConversationReference (service_url, conversation.id,
  │  conversation.conversationType, from.id/name) in the in-memory map
  ├─ text cleanup: remove the bot's own <at>…</at> mention, unwrap every
  │  other mention to its display name (so who-was-addressed survives
  │  into the prompt), decode HTML entities
  ├─ gating (in order):
  │    1. allow_dms — personal-chat messages dropped when false
  │    2. mention_only — group/channel messages must @-mention the
  │       bot when true; never applied to personal chats (a DM is
  │       definitionally addressed to the bot)
  │    3. sender allowlist via peer_groups (`channel_external_peers`
  │       resolver, matching every other channel; empty = deny,
  │       `"*"` = allow all)
  ├─ build ChannelMessage → tx.send()
  └─ respond 200 immediately (agent turn runs async; Teams has a ~15s
     delivery timeout)
```

JWT validation endpoints (Bot Framework, single-tenant):

- OpenID config: `https://login.botframework.com/v1/.well-known/openidconfiguration`
  → `jwks_uri` → JWKS (cached; keys rotate)
- Expected `aud`: the configured `app_id`
- Expected `iss`: exactly `https://api.botframework.com`, resolved through
  `auth::connector_issuers()`. Connector-to-bot tokens are always minted by
  this issuer; the tenant's Entra issuers mint tokens for *outbound*
  Graph/SSO flows, not for these activity POSTs, so accepting them here
  would widen the trust boundary with no legitimate caller.
- `exp` and `nbf` are both enforced, with a 300s clock-skew allowance ("up
  to 5 minutes" per the Bot Framework authentication spec).
- The JWKS cache is bounded on both sides, and the two bounds are tracked
  separately. Refresh *attempts* are spaced at least 60s apart whether or not
  they succeed, so a flood of unknown `kid`s cannot re-probe a failing issuer
  once per request; token headers name their `kid` before any signature check,
  which makes that rate an unauthenticated lever. Independently, only a key set
  fetched within the last 24h may be served, so a key the issuer has
  *withdrawn* stops being trusted without waiting for a restart. The two rules
  compose fail-closed: when the cache is past 24h and the mandatory refresh
  either fails or is rate-limited, the request is rejected rather than answered
  from retained keys.
- Two binding checks gate everything downstream: the signing key's
  `endorsements` must cover the activity's `channelId` (per Microsoft's Bot
  Connector authentication contract), and the token's signed `serviceurl`
  claim must match the activity's `serviceUrl` before any conversation
  reference is recorded or any connector token is sent to that host.

### Outbound (ZeroClaw → Teams, proactive)

```
send(SendMessage)
  ├─ look up ConversationReference by recipient (conversation id)
  ├─ acquire connector token:
  │  POST https://login.microsoftonline.com/{tenant_id}/oauth2/v2.0/token
  │  grant_type=client_credentials
  │  client_id={app_id} client_secret={app_password}
  │  scope=https://api.botframework.com/.default
  │  (cached until expiry minus skew)
  ├─ split the text to Teams' per-message size budget (see below)
  ├─ require a TLS destination before attaching the token (see below)
  └─ POST {service_url}/v3/conversations/{conversation_id}/activities
     one request per chunk, in order
     body: { "type": "message", "text": ... }
     header: Authorization: Bearer <connector token>
```

`service_url` is taken from the stored ConversationReference (Teams
sends it on every inbound activity); it is never hardcoded.

Because it is runtime input rather than a constant, every Connector call
checks the destination scheme before the bearer token is attached, and the
check lives at that single choke point rather than at URL construction so it
is bound to the credential instead of to one caller's path. `https` is
required; plain `http` is accepted only for a loopback host, which is a local
mock rather than a production Connector endpoint and cannot carry the token
off the machine. Microsoft treats this token as password-equivalent and
serves `serviceUrl` over TLS, so a public plain-HTTP destination is either a
broken deployment or an attempt to capture the credential, and the send fails
instead. The Entra token endpoint needs no such check: it is a hardcoded
HTTPS constant (`connector_token_url`), not runtime input.

#### Proxy coverage

The channel has three egresses, not one: Connector sends, the JWKS fetch that
authenticates inbound activities, and the Entra token request. All three
resolve their client through the same per-channel `proxy_url` (falling back to
the global runtime proxy), because a deployment that can only reach the
internet through a corporate proxy has to route the auth calls too. Covering
only the sends fails in a way that reads as unrelated: inbound activities are
rejected with 401 because the keys cannot be fetched, and no reply goes out
either because the token cannot be minted, while the proxy looks correctly
configured. Lark resolves its tenant-token fetch through its channel client
for the same reason.

The auth types hold a resolver rather than a client, since `proxy_url` is live
config and a client built at construction would keep dialing direct after a
reload changed it. The client factory caches per proxy setting, so resolving
on every call adds no connection pool. When no resolver is installed the auth
types fall back to the global runtime proxy rather than a bare client, so a
caller that forgets to wire one loses the per-channel override but not the
`[proxy]` settings the rest of the daemon obeys.

#### Outbound message size limit

Teams measures a message in UTF-16 code units — including `@`-mentions and
reactions — and rejects anything past ~100 KB with `413`
(`MessageSizeTooBig`); Microsoft recommends staying under 80 KB. `send()`
therefore splits content against a deliberately conservative character
budget (`TEAMS_MAX_MESSAGE_CHARS` = 18 000) before POSTing: it prefers a
paragraph break, then a line break, then a word boundary, and only hard-cuts
when a single unbroken run overflows. The split is lossless — concatenating
the chunks reproduces the input, so code and indentation survive — and an
in-budget reply is sent unchanged as a single activity. Because the split
lives in `send()`, every reply is covered.

A chunk is a message of its own, so a failure partway through leaves the
earlier chunks in the chat and the rest undelivered. The failed chunk is not
reposted past its throttle budget: creating an activity is not idempotent and
the Connector offers no idempotency key, so a retry after an ambiguous failure
risks showing the same text twice. The error propagates and the orchestrator
records it, which matches how every other splitting channel in the repo
behaves.

Splitting here rather than asking the model to shorten its answer is
deliberate: the ceiling is a hard, deterministic transport constraint counted
in UTF-16, which a model cannot estimate reliably, and mechanical chunking is
lossless and costs no extra round-trip. This matches every other channel in
the repo (Discord 2 000, Telegram 4 096, Slack 40 000, Lark card ~28 KB).

#### Outbound rate limits

Teams applies several quotas to a bot's outbound sends:

| Quota | Limit | Applies to |
| --- | --- | --- |
| Per bot per conversation, "send to conversation" | 7/1s, 8/2s, 60/30s, 1800/1h | `send()`, each split chunk, the typing indicator |
| Per conversation, all bots | 14/1s, 16/2s | shared with other apps in the conversation |
| Per app per tenant | 50 RPS | everything |

The four send windows are sliding *counts*, not four statements of one rate:
they imply minimum spacings of 143 ms, 250 ms, 500 ms and 2 000 ms
respectively, and only a window shorter than a given burst can bind it. Bursts
here are bounded by one reply's chunk count, so the 1s and 2s windows are the
reachable ones; the hourly budget is left to `429` backoff
rather than self-enforced, since honoring 1800/hour as a rate would cost a
ten-chunk reply twenty seconds of delivery for a bound no realistic
conversation reaches.

A chunk is a distinct message, so it cannot be skipped without losing it.
`TEAMS_CHUNK_SEND_SPACING` (500 ms) separates the chunks of one oversize reply.
Without it a long enough reply trips a window on its own, which Microsoft's own
guidance calls out ("message splitting at the service level results in higher
than expected RPS"). The value is the tightest reachable window's spacing
(250 ms, from 8/2s) doubled for headroom, which leaves the 1s and 2s windows at
2/7 and 4/8 so a concurrent turn in the same conversation still fits. The
spacing goes between chunks only, so the common single-activity reply waits not
at all.

On `429`, `activity_request` retries up to `CONNECTOR_MAX_ATTEMPTS` (3),
honoring `Retry-After` when Teams sends one and otherwise doubling from 1 s
with ±25% jitter, capped at 10 s per wait. The budget and the base are chosen
together rather than separately: two waits of 1 s and 2 s cannot cumulate to
less than 2.25 s even at the bottom of both jitter bands, so a filled 1s or 2s
window, the two a single reply's burst can fill, has certainly reopened. The
30s and hourly windows are not waited out, because they fill only when the
conversation is genuinely over budget and reporting that beats holding a turn
for half a minute. Every retrying request delivers a reply or one of its
chunks, so one deadline covers them all: the per-turn budget
(`channels.message_timeout_secs`, 300 s by default), which a 10 s ceiling on
one wait sits well inside. Microsoft's own sample retries three times from a
2 s base capped at 20 s; this is tighter on purpose, for that deadline.
`Retry-After` is read only in its delay-seconds form: honoring the HTTP-date
form would import the service's clock, and the local backoff is the better
answer when the two disagree.

Retrying is scoped by call site, since it is right only where losing the
request loses content *and nothing behind it would resend*:

| Request | Policy |
| --- | --- |
| `send()` and its chunks | `Retry` |
| Typing indicator | `FailFast` |

The typing indicator is superseded by the reply itself and its caller already
treats an error as "skip", so retrying it would only stall the turn it is meant
to announce. A reply is the opposite: nothing follows it to carry the answer
again, so it waits the throttle out.

`502`/`504` are deliberately *not* retried even though Microsoft's guidance
lists them alongside `429`: creating an activity is not idempotent and the
Connector exposes no idempotency key, so retrying an ambiguous gateway failure
risks posting a user-visible message twice. One reported failure is the better
outcome. Note also that the generic `PacedChannel` wrapper
(`reply_min_interval_secs`) does not cover any of this: it is off by default.

### Conversation ID semantics (learned from OpenClaw `inbound.ts`)

- Personal (1:1) chats use opaque `a:…` conversation IDs; team channels
  use `19:…@thread.tacv2`.
- Channel conversation IDs may carry `;messageid=…` suffixes — normalize
  by splitting on `;` for the reply target, keep the message id for
  threading.
- `conversation.conversationType == "personal"` ⇒
  `Channel::is_direct_message()` returns true (skips mention gating and
  the reply-intent classifier).

## 4. New files

```
crates/zeroclaw-channels/src/msteams/
  mod.rs           MsTeamsChannel; impl Channel + Attributable
  auth.rs          inbound JWT validation (JWKS fetch + cache),
                   outbound client-credentials token (cache)
  activity.rs      Activity / ConversationReference serde types,
                   bot-mention removal + non-bot mention unwrapping,
                   HTML entity decoding, conversation-id normalization,
                   mention detection
  conversation.rs  in-memory ConversationReference store
```

### `Channel` trait mapping

| Method | Behavior |
| --- | --- |
| `name()` | `"msteams"` |
| `listen()` | axum server on `0.0.0.0:{port}`, route `POST {path}`. Refuses to start when `app_id`, `tenant_id`, or `app_password` is empty: `app_id` is the audience inbound tokens are validated against, while `tenant_id` and `app_password` are the endpoint and secret Entra mints every Connector token from, so a channel missing any of them would bind, report itself ready, and fail one reply at a time instead of once at startup. The refusal is a typed `MsTeamsListenerFatalError`, which the orchestrator's supervisor recognises as non-retryable: it records the failure once and parks the listener instead of restarting it on a backoff every attempt of which fails identically. Modelled on Discord's `DiscordListenerFatalError`, and deliberately narrow — a bind conflict or an unreachable Entra endpoint stays on the retry path |
| `send()` | proactive Connector API POST |
| `self_handle()` | bot id from `activity.recipient.id` (set on first inbound) — self-loop guard |
| `self_addressed_mention()` | `<at>BotName</at>` form for the per-channel system prompt |
| `is_direct_message()` | `conversationType == "personal"` |
| `health_check()` | true once listener is bound |
| `start_typing()` | one-shot `typing` activity (no `streaminfo` entity). Carries the visual feedback for the whole turn in personal and group chats. Skipped in team channels, which have no indicator to show (see below) |
| `stop_typing()` | no-op — Teams' typing indicator expires on its own |
| everything else | trait defaults (deferred) |

### Why the streaming protocol is not used

Teams has a native **streaming messages** feature — the gray "thinking" bubble
Copilot shows, which OpenClaw drives through the Teams SDK's `ctx.stream`
(`reply-stream-controller.ts`). This channel does not use it. Each turn posts
one reply when the answer is complete.

The protocol's own rules are what rule it out. Every frame carries the whole
response so far, not a delta, and each frame may only extend what the previous
one published; Teams refuses a frame that retracts rendered text, and the
closing message replaces what is on screen rather than what a reader has
already seen. So a frame is irrevocable in the only sense that matters here.

That collides with the outbound leak policy in `security.leak_detection`, which
every other outbound path runs before anything reaches a channel. A detector
needs enough of a value to recognise it, so the frames that assemble a
credential all arrive before any of them looks like one. Publishing the
accumulation as it stands renders the prefix; redacting once the value
completes would retract text already shown, which Teams rejects and a reader
has already read either way.

Withholding the pending tail closes the gap for a bounded pattern. A keyed
form (`api_key = …`) has a known start and a length ceiling, so the text that
could still become one is identifiable, and it is released on the frame that
either completes the value — redacted, in the same position — or rules it out.
Frames stay monotonic and nothing leaks.

It does not close the gap for an unbounded pattern. A database connection URL
can span arbitrary text and newlines; a PEM block runs from its `BEGIN` marker
to an `END` marker that may never arrive. For these, "not yet decidable" and
"never going to complete" are the same observation, so the choice is between
publishing a prefix that may turn out to be a credential and stalling the
bubble until the turn ends. Neither is a streaming implementation worth
shipping, and picking one per pattern family means a partial-match state
machine for each — a security boundary of its own, not a detail of adding a
channel.

A completed answer has none of this structure: the whole text is decidable, so
it is redacted once and posted once. That is what this channel does. The
streaming protocol stays unimplemented until the boundary problem has an
answer of its own, and this design records the reason so a later attempt starts
from it rather than rediscovering it.

Two consequences are worth stating plainly, since streaming is what would
otherwise cover them:

- A personal chat shows a typing indicator rather than accumulating text, so a
  slow turn gives no sense of progress beyond "working".
- A team channel shows nothing at all, because Teams has no typing indicator
  there either (see below), so a long channel turn is silent until its reply
  arrives.

#### Typing indicator scope

Personal and group chats show the ordinary typing indicator while a turn runs.
Team channels are skipped: Teams draws no typing indicator in a
channel for anyone, bot or human, which Microsoft's documentation team states
directly ([msteams-docs#1451](https://github.com/MicrosoftDocs/msteams-docs/issues/1451),
closed with "Typing indicator is only supported in 1:1 and group chat. It is not
supported in Teams scope"), on a report whose repro is this exact case.

The Connector does not report this. Posting `{"type": "typing"}` to a channel
conversation returns `202 Accepted` from every regional endpoint while the
channel shows nothing, verified against a live channel. So the waste is
invisible from the response and has to be decided by scope: the orchestrator
refreshes the indicator every 4 seconds for the length of the turn, and each
refresh spends one request from the same per-conversation window
(1800/hour, §Rate limits) that the reply itself draws on.

The check reads `conversationType` from the stored reference, which is already
in memory, so a skipped turn acquires no token and opens no connection.
`start_typing` is handed only the recipient, which is the conversation id with
any `;messageid=` thread suffix stripped, so it could not address a thread even
where one exists — moot here, since only channels have threads and channels are
the skipped case.

## 5. Config schema

New `MSTeamsConfig` in `crates/zeroclaw-config/src/schema.rs`, modeled
on `MattermostConfig` (`#[prefix = "channels.msteams"]`, `Configurable`
derive, `#[secret]` on the secret field):

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `enabled` | bool | `false` | standard channel gate |
| `app_id` | String | — | Azure Bot App ID |
| `app_password` | String | — | `#[secret]`; client secret |
| `tenant_id` | String | — | single-tenant Entra tenant |
| `port` | u16 | `3978` | axum listen port |
| `path` | String | `"/api/messages"` | webhook route |
| `allow_dms` | bool | `true` | whether the bot responds in personal (1:1) chats at all; when `false`, inbound personal-chat activities are dropped |
| `mention_only` | `Option<bool>` | `None` (= true in groups) | group/channel gating only; personal chats are exempt by definition (gated by `allow_dms` instead). Named `mention_only` to match the existing telegram/mattermost convention. |
| `interrupt_on_new_message` | bool | `false` | when `true`, a newer message from the same sender in the same conversation cancels the in-flight agent run and starts a fresh response (history preserved); default queues instead. Feeds the orchestrator's `InterruptOnNewMessageConfig`. **Applied channel-wide:** it is on for every `msteams` alias if any alias enables it, the same rule every channel uses. Per-alias resolution is deferred (§9). |
| `proxy_url` | `Option<String>` | `None` | per-channel proxy for every outbound call: Connector sends, the JWKS fetch and the Entra token request (§"Proxy coverage"). Falls back to the global `[proxy]` settings when unset. |

The remaining fields, `excluded_tools`, `reply_min_interval_secs` and
`reply_queue_depth_max`, are the standard per-channel tool filter and
`PacedChannel` settings, with the same meaning and defaults as on every other
channel.

Multiple aliases (`[channels.msteams.<alias>]`) follow the standard
HashMap pattern; each alias runs its own listener, so aliases must use
distinct ports. One documented exception to per-alias resolution:
`interrupt_on_new_message` is applied channel-wide (see the field note
above).

## 6. Wiring checklist (mirror of the `mattermost` touchpoints)

| Location | Change |
| --- | --- |
| `crates/zeroclaw-channels/src/lib.rs` | `#[cfg(feature = "channel-msteams")] pub mod msteams;` |
| `crates/zeroclaw-channels/Cargo.toml` | `channel-msteams = ["dep:axum", "dep:jsonwebtoken", "dep:thiserror"]`; add to the `channels-full` list. All three are existing optional dependencies of the crate (`jsonwebtoken` v10 on the aws-lc-rs backend), so the feature must enable each one: `axum` is also a dev-dependency, which lets the tests compile without it and would hide a missing entry. `cargo check -p zeroclaw-channels --no-default-features --features channel-msteams --lib` is the check that proves the closure. |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | `pub use crate::msteams::MsTeamsChannel;`; `"msteams" =>` arm in `build_channel_by_id` + `#[cfg(not(...))]` bail arm; configured-channel collection loop; `msteams` arm in `deliver_announcement` for dotted cron delivery refs; `msteams` arm in `is_non_retryable_channel_listener_error` recognising `MsTeamsListenerFatalError`; add `msteams` to the "Unknown channel" supported list; add `msteams` field to `InterruptOnNewMessageConfig` (mechanical updates to the many test literals). |
| `crates/zeroclaw-channels/src/listing.rs` | `ChannelCompileSpec { schema_name: Some("Microsoft Teams"), type_keys: &["msteams"], compiled: cfg!(feature = "channel-msteams") }` |
| `crates/zeroclaw-config/src/schema.rs` | `MSTeamsConfig` struct + `pub msteams: HashMap<String, MSTeamsConfig>` on the channels struct; add to the `channel.*` allowlist const, `ChannelInfo` list, `has_any_enabled`, row iterator, `Configurable` registration list, `ChannelConfig` impl. |
| `crates/zeroclaw-api/src/attribution.rs` | `ChannelKind` variant `#[strum(serialize = "msteams")] MsTeams` |
| `.github/workflows/ci.yml` | a `Microsoft Teams` entry in the `test-channel-features` matrix, shown as `Test (channel Microsoft Teams)`, which runs the `msteams` tests plus the Teams supervisor test and the two channel-registration drift tests with `--features channel-msteams`. The matrix job is in the `gate` job's `needs`, so the entry is a required check. Necessary because no other lane runs these tests: the default `Test` lane does not enable the feature, and `Lint` compiles it under `ci-all` without running it. |
| `src/channels/` re-export | **Deliberately absent.** `mattermost` has a `src/channels/mattermost.rs` shim, but `src/channels/mod.rs` declares only `matrix` and `telegram`, so that file and most of its neighbours are orphans left behind by the crate split and are never compiled. A Teams copy would be dead code. |
| `Cargo.toml` (workspace root), `Containerfile`, `dev/ci/docker-tags.toml`, `setup.bat` | wherever `channel-mattermost` appears in feature lists, that is the `channels-full` bundle, the `all-features` container tag, and the installer's `all` preset, but deliberately **not** the lean `dist` selection. Consequence: the prebuilt release binaries and the `minimal` / `default-features` / `dist` container tags do **not** carry Teams, while the published `all-features` tag does; operators on a lean artifact build from source with `--features channel-msteams` (or `channels-full`). The user guide states this explicitly. |
| `docs/book/src/channels/msteams.md` + `SUMMARY.md` + `overview.md` + `docs/book/peer-groups.toml` | user-facing setup guide, its table-of-contents and overview entries, and the peer-group sender description it renders |

## 7. Single-source-of-truth compliance (AGENTS.md)

Pre-edit ritual answers for every state-bearing field:

| Field | Verdict |
| --- | --- |
| `app_id` / `app_password` / `tenant_id` | Source of truth is `Config` (`channels.msteams.<alias>`). The channel does NOT copy them into struct fields; it resolves through a `&Config`-backed resolver/closure at use time, following the `peer_resolver` pattern in `mattermost.rs`. |
| Sender allowlist | Source of truth is `Config.peer_groups` (no per-channel `allow_from` field — that would duplicate the peer-group registry). Resolved via the `channel_external_peers` closure at message time, never cached. |
| Connector OAuth token cache | Source of truth is **created here** (issued by Entra at runtime). A time-bounded materialized credential, not a copy of config state. `tokio::sync::RwLock` with expiry. Bounded by the credentials as well as by the clock: the entry records a SHA-256 fingerprint of the `app_id`/`app_password` pair Entra minted it for, and is served back only to that pair. Without it, a same-tenant secret rotation or bot-identity swap would keep posting under the retired credential for up to the token's remaining hour, since the provider is cached per tenant and the credentials are passed per call. The fingerprint is stored instead of the pair so the cache can reject a mismatch without holding the secret twice. |
| JWKS cache | Source of truth is Microsoft's JWKS endpoint; the cached copy is a runtime materialized view. Two independent bounds with separate timestamps: the last *attempt* spaces fetches at least 60s apart regardless of outcome, and the last *success* caps how long a key set may be served at 24h. A stale cache whose mandatory refresh fails or is rate-limited serves nothing. |
| ConversationReference map | Source of truth is **created here** (delivered by Teams per activity; exists nowhere else in the codebase). In-memory `RwLock<HashMap<String, ConversationReference>>`. |
| `bot_identity` (id/name) | Source of truth is the platform (first inbound `activity.recipient`). Write-once `std::sync::OnceLock`. `mattermost.rs::bot_identity` answers the same question with a `tokio::sync::OnceCell` because it has to await an API call to learn its own identity; Teams is handed the identity on every inbound activity, so the initialization is synchronous and an async cell would buy nothing. |

## 8. Testing plan

Unit tests (no live Azure):

- JWT validation: expired token, not-yet-valid (`nbf`) token, wrong `aud`,
  wrong issuer (including a tenant Entra issuer), bad signature, malformed
  header, unknown `kid` → all rejected with 401; valid token accepted (test
  keys generated in-test). The clock-skew allowance is honored at both
  bounds.
- Binding checks: a signing key whose `endorsements` omit the activity's
  `channelId` is rejected; an activity whose `serviceUrl` disagrees with the
  token's signed `serviceurl` claim is rejected **without** recording a
  conversation reference.
- Proxy coverage: the JWKS fetch and the Entra token request both leave
  through the client the channel resolves, so a configured proxy carries the
  auth egresses and not just the sends.
- Destination TLS: `https` and loopback `http` may carry the Connector token;
  a public `http` host, a loopback lookalike hostname, and a non-HTTP scheme
  may not. A send addressed at a plain-HTTP service URL fails at the guard,
  before any request goes out.
- Activity deserialization: personal vs channel conversation, mention
  entities, `;messageid=` suffix normalization.
- Text cleanup: the bot's own mention is removed while other mentions are
  unwrapped to display names (non-bot mentions survive into the prompt);
  HTML entity decoding; the line structure the author typed survives the
  cleanup, with or without a mention to remove, while the seam a removed
  mention leaves closes to a single space rather than fusing two words.
- Gating: `allow_dms` on/off; `mention_only` on/off × personal/channel;
  peer-group allowlist filtering.
- Outbound tag strip: a message that is nothing but a tool-call envelope is
  never posted, while prose that merely talks about the tags is sent verbatim.
- Typing: `start_typing()` POSTs a bare `typing` activity.
- Outbound chunking: an in-budget reply is a single unchanged activity; an
  oversize reply splits into chunks that each fit the budget, concatenate
  back to the original, and prefer paragraph/line boundaries. A split reply
  paces its chunks; an unsplit one does not wait.
- Rate limits: a `429` on a content-bearing send is retried and the message
  still lands; a conversation that stays throttled fails after the attempt
  budget rather than retrying forever; a throttled typing indicator is
  skipped without retrying, so it cannot stall the turn. `Retry-After` in
  delay-seconds wins over the local backoff, the HTTP-date form and garbage
  both fall back to it, and the jittered backoff stays inside its documented
  bounds including at shift overflow. One assertion pins the budget to the 2s
  window, so changing the base or the attempt count in isolation fails.
- Self-loop guard: activity where `from.id == recipient.id` is dropped.
- `send()`: wiremock stub of `login.microsoftonline.com` token endpoint
  + Connector `/v3/conversations/.../activities`; assert bearer header,
  payload shape, token reuse before expiry (pattern: `webhook.rs`
  tests).
- Unknown-conversation send → clear error (no stored reference).

These tests only compile with `channel-msteams` enabled, which the default
test lane does not do, so they run in the `Test (channel Microsoft Teams)`
entry of the `test-channel-features` matrix (a required check via the `gate`
job).

Manual validation (operator): sideload manifest, DM the bot, @mention it
in a team channel, confirm replies and threading.

## 9. Deferred

- ConversationReference persistence (survive restarts)
- Multi-tenant bot support
- Media attachments (inbound download allowlist, outbound upload)
- Adaptive Cards, polls, approval prompts (`request_approval`)
- Reactions, message delete (`redact_message`)
- Graph API enrichment (member lookup for allowlist UPN resolution —
  OpenClaw's `resolve-allowlist.ts` equivalent)
- Per-alias `interrupt_on_new_message` resolution (today any alias that
  enables it turns it on channel-wide)
- Code-fence reopening when outbound chunking has to hard-cut inside a
  fenced block that is itself larger than the per-message budget
