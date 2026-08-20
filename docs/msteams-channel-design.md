# Design: Microsoft Teams Bot Channel (`channel-msteams`)

- Status: implemented
- Date: 2026-07-17 (revised after PR review)
- Scope: plain-text send/receive, inbound JWT validation, @mention
  gating, DM policy, sender allowlist, streaming draft updates (the gray
  "thinking" message that resolves into the final reply), typing
  indicators, and outbound chunking for Teams' per-message size limit.
  `multi_message` is deliberately **not** offered; §"`multi_message` is not
  offered" records why
- Risk tier: High. The change-risk routing in
  [agent-guidelines](book/src/contributing/agent-guidelines.md#stability-and-risk)
  classifies trust-boundary and `.github/workflows/` changes as High, and
  this PR is both: it adds a new inbound authentication boundary (the
  listener authenticates Bot Connector JWTs and holds the bot's Connector
  credential) and adds a required CI lane. No existing boundary is
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
    Teams native streaming, its 1:1-only limitation, and its 1 request/s
    throttle. The two-minute cap on one streaming session appears only in the
    error table, as `403 ContentStreamNotAllowed` / "Content stream finished
    due to exceeded streaming time", described as "the strict time limit of
    two minutes"
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
native implementation only as an explicit capability-based exception that
names the missing host capability, the native code path depending on it,
and the condition that permits migration. For Teams those are:

- **Missing host capability**: supervised inbound HTTP ingress for a
  plugin. The plugin channel contract in `wit/v0/channel.wit` is poll-based
  (`poll-message`) and `wit/` defines no HTTP capability at all, so a
  channel plugin can neither receive the Connector's activity POSTs nor
  read the `Authorization` header the JWT check requires. ADR-006 lists
  "inbound listener or webhook traffic can reach the plugin under
  supervised runtime ownership" among the conditions it is still waiting
  on, which is why that ADR remains `proposed`.
- **Native code path that depends on it**: `MsTeamsChannel::listen()` hosts
  the axum `/api/messages` route, and `bind_activity_to_claims()`
  authenticates each request against the signed token before any state is
  recorded. Azure Bot Service delivers activities by POSTing to a public
  HTTPS endpoint, so there is no polling alternative to fall back on.
- **Condition that permits migration**: once the host owns the HTTPS
  ingress, can hand a plugin the request headers and body, and can keep
  the Connector credential outside the component, this channel can move to
  a plugin without protocol changes. `auth.rs`, `activity.rs`, and
  `conversation.rs` already hold no axum types; only `mod.rs` does.

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
| Streaming replies | Implemented via the existing draft pipeline (`send_draft`/`update_draft`/`finalize_draft`). `partial` uses Teams' native streaming protocol, which the platform allows in 1:1 chats only — group chats show a typing indicator and receive one final reply instead, and team channels receive the final reply with no indicator, since Teams has none in that scope. `multi_message` is refused and reads as `off`. (A message-edit fallback for groups was implemented and then rejected; see §3.) |
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
lives in `send()`, every reply is covered, whichever `stream_mode` produced
it.

The ceiling is not lifted for streaming: the streaming spec's error table
carries `403 ContentStreamNotAllowed` ("Message size too large") and points
at the same size document. A stream cannot be chunked, since every frame
must contain what was streamed before it and a stream closes with exactly
one final activity, so `partial` handles an oversize answer by leaving the
stream rather than by splitting inside it. Frames stop going out once the
accumulation passes the budget, because past that point none of them can
land; finalize then takes the opened bubble down and delivers the answer
through `send()`, which splits. Giving up on the stream is recorded once per
draft (`DEBUG`), not on every delta that follows it into the same branch. An
answer already past the budget on its first update never opens a stream at
all, so no bubble appears and there is none to take down. The reply would arrive either way (the
orchestrator resends through `send()` when finalize fails), but only by way
of an error path, at the cost of requests spent to be refused.

Splitting here rather than asking the model to shorten its answer is
deliberate: the ceiling is a hard, deterministic transport constraint counted
in UTF-16, which a model cannot estimate reliably, and mechanical chunking is
lossless and costs no extra round-trip. This matches every other channel in
the repo (Discord 2 000, Telegram 4 096, Slack 40 000, Lark card ~28 KB).

#### Outbound rate limits

Teams applies two separate quotas, and the two delivery paths are governed by
different ones:

| Quota | Limit | Applies to |
| --- | --- | --- |
| Per bot per conversation, "send to conversation" | 7/1s, 8/2s, 60/30s, 1800/1h | `send()`, each split chunk, typing (group chats only) |
| Streaming API | 1 request/s, and a 2-minute cap per stream | `partial` draft updates |
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

Each path is paced by the mechanism its quota allows, and the two are not
interchangeable:

- `partial` skips: `draft_update_interval_ms` (default 1500) drops an update
  that arrives early. Skipping is free because every update carries the whole
  response so far, and blocking is not an option, since the caller is the
  agent's token loop. The documented streaming cap is 1/s; the default keeps
  the same headroom over it that Microsoft's own Teams AI SDK takes, which
  buffers to 1.5 s rather than sitting on the limit.
- Split chunks wait: a chunk is a distinct message, so it cannot be skipped
  without losing it. `TEAMS_CHUNK_SEND_SPACING` (500 ms) separates the chunks
  of one oversize reply instead. Without it a long enough reply trips a window
  on its own, which Microsoft's own guidance calls out ("message splitting at
  the service level results in higher than expected RPS"). The value is the
  tightest reachable window's spacing (250 ms, from 8/2s) doubled for headroom,
  which leaves the 1s and 2s windows at 2/7 and 4/8 so a concurrent turn in the
  same conversation still fits. The spacing goes between chunks only, so the
  common single-activity reply waits not at all.

On `429`, `activity_request` retries up to `CONNECTOR_MAX_ATTEMPTS` (3),
honoring `Retry-After` when Teams sends one and otherwise doubling from 1 s
with ±25% jitter, capped at 10 s per wait. The budget and the base are chosen
together rather than separately: two waits of 1 s and 2 s cannot cumulate to
less than 2.25 s even at the bottom of both jitter bands, so a filled 1s or 2s
window, the two a single reply's burst can fill, has certainly reopened. The
30s and hourly windows are not waited out, because they fill only when the
conversation is genuinely over budget and reporting that beats holding a turn
for half a minute. Every retrying request turns out to be a send outside a
stream, so one deadline covers them all: the per-turn budget
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
| The finalize activity | `FailFast` |
| Intermediate streaming frames (`informative`, `streaming`) | `FailFast` |
| Typing indicator | `FailFast` |
| Bubble takedown (closing `final` + `DELETE`) | `FailFast` |

The frames, the typing indicator and the cancel are superseded by whatever
comes next, and their callers already treat an error as "skip". Retrying them
would stall the agent's token loop for seconds to redeliver a frame nobody will
see, which is the very cost `draft_update_interval_ms` skips updates to avoid.

The finalize activity looks like it belongs in the other row and does not,
because the orchestrator answers a failed finalize by resending the whole
answer through `send()`, whose chunks retry: the content is covered one layer
up, so waiting here only delays that fallback. The case where waiting is most
tempting is exactly the case where it is most futile. A stream past its
two-minute deadline cannot accept the message however many times it is offered,
and Teams reports that state as a `429` ("API calls quota exceeded") as readily
as a `403`, so a retrying finalize spends its whole budget on a session that is
already gone. Observed live: a 150 s tool call left three attempts over five
seconds to fail against a stream Teams had closed at the two-minute mark, all
of it ahead of a fallback that then delivered normally.

`502`/`504` are deliberately *not* retried even though Microsoft's guidance
lists them alongside `429`: creating an activity is not idempotent and the
Connector exposes no idempotency key, so retrying an ambiguous gateway failure
risks posting a user-visible message twice. One reported failure is the better
outcome. Note also that the generic `PacedChannel` wrapper
(`reply_min_interval_secs`) does not cover any of this: it is off by default
and deliberately does not pace draft paths.

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
| `listen()` | axum server on `0.0.0.0:{port}`, route `POST {path}`. Refuses to start when `app_id`, `tenant_id`, or `app_password` is empty: the first two authenticate inbound activities, and Entra mints every Connector token from the secret, so a channel missing any of them would bind, report itself ready, and fail one reply at a time instead of once at startup |
| `send()` | proactive Connector API POST |
| `self_handle()` | bot id from `activity.recipient.id` (set on first inbound) — self-loop guard |
| `self_addressed_mention()` | `<at>BotName</at>` form for the per-channel system prompt |
| `is_direct_message()` | `conversationType == "personal"` |
| `health_check()` | true once listener is bound |
| `supports_draft_updates()` | `true` when the effective `stream_mode` is `partial` |
| `supports_draft_updates_for()` | per-message refinement of the above: `partial` ⇒ personal (1:1) chats only, because Teams' native streaming is 1:1-only (group chats get a typing indicator plus one final reply, team channels get the reply alone, and Teams would otherwise notify on the placeholder rather than the answer); `off` ⇒ false |
| `send_draft()` | `partial` (1:1 only): register a lazy local draft handle; **no activity is POSTed** and the orchestrator's placeholder text is dropped, so the Teams stream opens on the first real update (mirrors OpenClaw's lazy `HttpStream`) and the gray bubble never flashes "...". Returns `None` in every other case, including a second concurrent turn in the same chat, which Teams cannot stream |
| `update_draft_progress()` | informative update (`streamType: "informative"`) — the gray status text ("thinking…", tool status), clamped to the documented informative ceiling; opens the stream if it's the draft's first content |
| `update_draft()` | content chunk (`streamType: "streaming"`, accumulated text); opens the stream if it's the draft's first content, and stops emitting frames once the accumulation passes the per-message size budget, where none of them could land |
| `finalize_draft()` | carries no `replyToId` and no thread suffix, unlike the ordinary send the orchestrator falls back to on failure; the trait passes the draft handle rather than the originating message, and neither field would render anything, since a draft exists only in a personal chat and Teams' visual threading (and its `replyToId` handling) is channel-only. A team-channel turn never opens a draft, so its threaded reply goes out through `send()`, which does carry the anchor. Stream opened ⇒ final `message` activity (`streamType: "final"`), replacing the gray bubble and dropping the progress text; never opened (fast answer) ⇒ one plain message. An answer past the size budget cannot close a stream on its own content, so the stream is closed on what it already streamed, that message is deleted, and the answer goes out through `send()` as split messages. The draft's state is released on the way out even when nothing reached the wire (see §7) |
| `cancel_draft()` | best-effort takedown of the bubble, a `final` message closing the stream followed by a DELETE of the message that leaves (a DELETE alone is answered `2xx` and changes nothing on screen; see "Taking an abandoned bubble down"); nothing on the wire if the stream never opened. The state goes first and unconditionally, so a takedown nobody can perform still frees the chat for the turn that superseded this one |
| `start_typing()` | one-shot `typing` activity (no `streaminfo` entity). Carries the visual feedback in group chats, where no draft bubble is available. Skipped in team channels, which have no indicator to show (see below) |
| `stop_typing()` | no-op — Teams' typing indicator expires on its own |
| everything else | trait defaults (deferred) |

`stream_mode` is live config, so an operator editing it mid-turn flips it under
an open draft. Only `send_draft` reads it: one delivery lifecycle owns every
draft, so the callbacks after it need not ask which one a handle belongs to, and
a reload cannot hand a draft to an implementation that never created its state.
The values those callbacks do read are the ones meant to be live: credentials
and `draft_update_interval_ms` are resolved from current config on every call.

### Streaming protocol detail (the gray "thinking" message)

This is Teams' native **streaming messages** feature — the same thing
OpenClaw drives through the Teams SDK's `ctx.stream`
(`reply-stream-controller.ts`). Wire format (Bot Framework REST, no SDK
needed):

1. Informative/status update: POST a `typing` activity with an
   `entities` entry `{ "type": "streaminfo", "streamType":
   "informative", "streamSequence": n }` and `text` = status line. The
   first activity's returned id becomes the `streamId`; subsequent
   activities include `"streamId"` in the entity.
2. Content chunks: `typing` activity with `"streamType": "streaming"`,
   `text` = accumulated (not delta) response text.
3. Final: a `message` activity with `"streamType": "final"` and the full
   text. Teams replaces the gray streaming bubble with a normal message;
   informative/status history is no longer shown.

Platform constraints, and how we handle them:

- Native streaming is only supported in **one-on-one chats**. Group
  chats and team channels don't open drafts at all and receive one final
  reply; a group chat shows the ordinary typing indicator meanwhile, a
  team channel shows nothing. (A message-edit fallback was tried first;
  Teams notifies on the initial placeholder and stays silent on the edit
  that carries the real answer, which is exactly backwards.)
- Teams allows **one streaming response per chat at a time**. A turn cannot
  see its neighbours, and with `interrupt_on_new_message` off (the default)
  the orchestrator neither cancels nor awaits the previous in-flight task for
  a sender, so a follow-up arriving during a slow turn runs alongside it. Both
  would open a stream in the same chat, and the second one's frames would all
  be spent on a stream Teams refuses to start. `send_draft` therefore hands
  the second turn no draft: its answer is delivered as one ordinary message,
  the same shape a group chat already gets. The draft records its conversation
  for this check, and ages out after the two-minute session limit so a draft
  that some path failed to finalize or cancel cannot cost the chat its
  streaming for the life of the process.
- Updates are rate-limited (~1/s). `draft_update_interval_ms` defaults
  to `1500`, the same headroom Microsoft's own SDK buffers to; the
  orchestrator already throttles draft flushes on this interval, so no
  extra limiter is needed.
- `streamSequence` must be monotonically increasing; kept per-draft in
  the in-memory draft state alongside the `streamId`.
- Streamed content must grow monotonically: every `streaming` frame and
  the `final` message has to contain what was streamed before it, so
  `A brown` may be followed by `A brown fox` but not by `Hello`.
  Violations are rejected with `403 ContentStreamNotAllowed` ("Request
  streamed content should contain the previously streamed content").
  A tool loop breaks this routinely rather than rarely: text the model
  emits before a tool call is streamed, and the answer it composes after
  the tool returns is frequently not a continuation of it. We do not try
  to predict the rejection, because what is streamed and what is
  finalized pass through different sanitizers and a false positive would
  discard a perfectly good bubble. Instead the rejection is handled: the
  orchestrator's existing fallback resends the answer as an ordinary
  message, and finalize takes the abandoned stream down first (see
  "Taking an abandoned bubble down" below), since Teams keeps rendering an
  opened stream until its final message arrives and the reply would
  otherwise land underneath a draft frozen on the last frame that got
  through. OpenClaw hit the same protocol edge and responded by disabling
  streaming outright (openclaw/openclaw#56040); keeping the fallback costs
  two requests and preserves the bubble for the answers that do stream
  cleanly.
- Informative updates stop rendering once content streaming begins and
  are discarded from then on, so the channel stops sending them after
  the first `streaming` frame instead of spending the stream's
  one-per-second budget on frames the client throws away.
- An informative frame must not exceed "1 kb or 1000 characters". The
  document does not say how the byte figure is measured, so status lines
  are clamped against both bounds, whichever binds first, and a shortened
  line is marked with an ellipsis. ASCII trips the character count; other
  scripts trip the byte count first.
- A stream is held to the same per-message size ceiling as an ordinary
  reply, reported as `403 ContentStreamNotAllowed` ("Message size too
  large") rather than the `413` a plain send gets. See the size section
  below for how `partial` gets an oversize answer delivered.
- A streaming session must finish inside **two minutes**, documented as a
  strict limit on completing the streaming process rather than as an idle
  timeout. Teams closes an unfinished session, labels the bubble "this
  response was stopped", and refuses every later activity on that
  `streamId`. Any turn whose tools run longer than two minutes ends up
  here, which for an agent is ordinary rather than exceptional, so the path
  is treated as expected: the finalize is rejected, the bubble is taken
  down, and the fallback delivers the answer as an ordinary message. The
  rejection arrives as either `403 ContentStreamNotAllowed` ("Content
  stream finished due to exceeded streaming time") or `429` ("API calls
  quota exceeded"); both have been observed live for the same expired
  session, so neither is treated as retryable. Whether heartbeat frames
  could hold a session open past two minutes is untested here: the runs
  that hit the limit were silent for their whole tool call, so they cannot
  tell a total-time limit from an idle one. Microsoft's wording says total,
  and nothing in the current design depends on the answer.

### Taking an abandoned bubble down

Three paths abandon an opened stream: a finalize Teams refuses, an answer
too large for a stream to close on, and `cancel_draft` when
`interrupt_on_new_message` supersedes a turn. All three have to get the
bubble off the screen, and a `DELETE` on the streaming activity does not
do it.

Teams accepts that delete and answers `2xx`, so it reads as success in
every log, but it only drops the activity on the service. The client keeps
rendering the bubble. Live, one stayed up for more than five minutes past
a successful delete, outliving the two-minute session limit, and its Stop
button reported "can't stop the response" because the stream it would have
stopped was already gone. This is consistent with the protocol as
documented: the streaming contract ends a stream through the final message,
the user's Stop button, or the two-minute limit, and lists no delete.

So a takedown closes the stream first, with a `final` message, and then
deletes the ordinary message that leaves behind. Because a final message
must contain everything already streamed, it closes on the draft's last
content frame, which is the one text Teams cannot refuse for that reason.
A draft that only ever showed status lines has no such content (informative
frames are not part of the content stream), so it closes on the
`channel-msteams-draft-cancelled` notice instead. Both requests are
`FailFast`: the caller has already decided what the conversation gets
instead, and waiting out a throttle would only delay it.

The notice is written to be read, because it is what stays on screen if the
delete does not land. That degradation is the reason the closing text is a
localized string rather than a placeholder.

A fourth case cannot take the bubble down at all: the takedown needs a
Connector context, and the failure may be the resolution of that context, when
live config no longer carries the channel's block, the conversation reference
is gone, or Entra will not mint a token. Both `finalize_draft` and
`cancel_draft` then drop the draft's local state anyway and log a `WARN`
naming the stream, since a bubble left up looks exactly like one that was taken
down. Nothing is retried: the one context that could close the stream is the
one that could not be resolved, and finalize's caller is already resending the
answer as an ordinary message, which a retry here would only delay.

### `multi_message` is not offered

`stream_mode = "multi_message"` splits an answer into one message per
paragraph, the way Discord and Matrix do. Teams refuses the value instead:
`stream_mode()` resolves it to `off`, and `listen()` names the fallback once in
the operator's log. The channel therefore offers `off` and `partial` only.

A paragraph-split implementation existed in this branch and was withdrawn on
review. What it published came from the draft boundary, which the orchestrator
sanitizes with `sanitize_streaming_draft_text` (`orchestrator/mod.rs:3668`):
that pass removes reasoning and tool-protocol envelopes, but not the two
stages the delivery path adds, tool-narration stripping and the configured
credential redaction (`redact_channel_outbound_leaks`, `:3985`). Under
`partial` the gap is transient, since the sanitized final message replaces the
bubble's text; a paragraph is a permanent message that no later reply can edit
or recall, so a credential in mid-answer text would stay in the conversation.
Reviewer finding on
[#9241](https://github.com/zeroclaw-labs/zeroclaw/pull/9241#issuecomment-5332586063).

Closing the gap belongs at the shared boundary, not here. Redacting per channel
would put the policy in two places, and the exposure is not Teams-specific:
Matrix publishes paragraphs from the same text (`matrix.rs:3415`), so a fix in
`run_draft_updater` covers every channel that delivers a draft permanently,
including this one if the mode is reintroduced. Teams declines the mode until
then rather than shipping a delivery path whose safety depends on a boundary
that is not yet safe.

The withdrawal costs Teams nothing that `partial` already provides in a
personal chat, and removes the only way to show progress inside a team channel,
where Teams has no typing indicator either. A long channel turn is therefore
silent until its reply arrives.

#### Typing indicator scope

Group chats show the ordinary typing indicator while a turn runs, regardless of
`stream_mode`. Team channels are skipped: Teams draws no typing indicator in a
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
| `stream_mode` | `StreamMode` | `Off` | `off` / `partial` (the gray native streaming bubble; 1:1 chats only, groups fall back to typing plus one final reply and team channels to the reply alone); same enum Telegram/Discord/Lark use, whose third value `multi_message` this channel refuses and reads as `off` (§"`multi_message` is not offered") |
| `draft_update_interval_ms` | u64 | `1500` | draft flush cadence; clears Teams' ~1/s streaming rate limit with the same headroom Microsoft's own SDK buffers to |
| `interrupt_on_new_message` | bool | `false` | when `true`, a newer message from the same sender in the same conversation cancels the in-flight agent run and starts a fresh response (history preserved); default queues instead. Feeds the orchestrator's `InterruptFlags`. **Resolved from the `default` alias only** and then applied to every `msteams` alias (`InterruptOnNewMessageConfig` reads `channels.msteams.get("default")`), so a value set on a non-`default` alias has no effect. Per-alias resolution is deferred (§9). |

Multiple aliases (`[channels.msteams.<alias>]`) follow the standard
HashMap pattern; each alias runs its own listener, so aliases must use
distinct ports. One documented exception to per-alias resolution:
`interrupt_on_new_message` is read from the `default` alias and applied
channel-wide (see the field note above).

## 6. Wiring checklist (mirror of the `mattermost` touchpoints)

| Location | Change |
| --- | --- |
| `crates/zeroclaw-channels/src/lib.rs` | `#[cfg(feature = "channel-msteams")] pub mod msteams;` |
| `crates/zeroclaw-channels/Cargo.toml` | `channel-msteams = ["dep:jsonwebtoken"]`; add to the aggregate feature list. `axum`, `reqwest`, `jsonwebtoken` (v10, aws-lc-rs backend) are already dependencies. |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | `pub use crate::msteams::MsTeamsChannel;`; `"msteams" =>` arm in `build_channel` + `#[cfg(not(...))]` bail arm; configured-channel collection loop; add `msteams` to the "Unknown channel" supported list; add `msteams` field to `InterruptFlags` (mechanical updates to the many test literals). |
| `crates/zeroclaw-channels/src/listing.rs` | `ChannelCompileSpec { schema_name: Some("MSTeams"), type_keys: &["msteams"], compiled: cfg!(feature = "channel-msteams") }` |
| `crates/zeroclaw-config/src/schema.rs` | `MSTeamsConfig` struct + `pub msteams: HashMap<String, MSTeamsConfig>` on the channels struct; add to the `channel.*` allowlist const, `ChannelInfo` list, `is_any_enabled`, row iterator, `Configurable` registration list, `ChannelConfig` impl. |
| `crates/zeroclaw-api/src/attribution.rs` | `ChannelKind` variant `#[strum(serialize = "msteams")] MsTeams` |
| `crates/zeroclaw-api/src/channel.rs` | add `supports_draft_updates_for(&self, msg)` to the `Channel` trait **with a default implementation** delegating to `supports_draft_updates()`, so no other channel changes behavior. Teams overrides it because its draft support depends on conversation type; the orchestrator's two draft/typing decision sites call the per-message form. |
| `crates/zeroclaw-channels/src/paced_channel.rs` | forward `supports_draft_updates_for` to the wrapped channel, so the pacing wrapper does not flatten the per-message answer back to the capability-wide one. |
| `.github/workflows/ci.yml` | dedicated `test-msteams` lane (`cargo nextest run -p zeroclaw-channels --features channel-msteams -E 'test(msteams)'`), added to the `gate` job's `needs` so it is a required check. Necessary because the default lanes never compile the feature. |
| `src/channels/` re-export | **Deliberately absent.** `mattermost` has a `src/channels/mattermost.rs` shim, but `src/channels/mod.rs` declares only `matrix` and `telegram`, so that file and most of its neighbours are orphans left behind by the crate split and are never compiled. A Teams copy would be dead code. |
| `Cargo.toml` (workspace root), `Containerfile`, `dev/ci/docker-tags.toml`, `setup.bat` | wherever `channel-mattermost` appears in feature lists, that is the `channels-full` bundle, the `all-features` container tag, and the installer's `all` preset, but deliberately **not** the lean `dist` selection. Consequence: the prebuilt release binaries and the `minimal` / `default-features` / `dist` container tags do **not** carry Teams, while the published `all-features` tag does; operators on a lean artifact build from source with `--features channel-msteams` (or `channels-full`). The user guide states this explicitly. |
| `docs/book/src/channels/msteams.md` + `SUMMARY.md` + `overview.md` | user-facing setup guide (separate docs PR) |

## 7. Single-source-of-truth compliance (AGENTS.md)

Pre-edit ritual answers for every state-bearing field:

| Field | Verdict |
| --- | --- |
| `app_id` / `app_password` / `tenant_id` | Source of truth is `Config` (`channels.msteams.<alias>`). The channel does NOT copy them into struct fields; it resolves through a `&Config`-backed resolver/closure at use time, following the `peer_resolver` pattern in `mattermost.rs`. |
| Sender allowlist | Source of truth is `Config.peer_groups` (no per-channel `allow_from` field — that would duplicate the peer-group registry). Resolved via the `channel_external_peers` closure at message time, never cached. |
| Connector OAuth token cache | Source of truth is **created here** (issued by Entra at runtime). A time-bounded materialized credential, not a copy of config state. `tokio::sync::RwLock` with expiry. Bounded by the credentials as well as by the clock: the entry records a SHA-256 fingerprint of the `app_id`/`app_password` pair Entra minted it for, and is served back only to that pair. Without it, a same-tenant secret rotation or bot-identity swap would keep posting under the retired credential for up to the token's remaining hour, since the provider is cached per tenant and the credentials are passed per call. The fingerprint is stored instead of the pair so the cache can reject a mismatch without holding the secret twice. |
| JWKS cache | Source of truth is Microsoft's JWKS endpoint; the cached copy is a runtime materialized view. Two independent bounds with separate timestamps: the last *attempt* spaces fetches at least 60s apart regardless of outcome, and the last *success* caps how long a key set may be served at 24h. A stale cache whose mandatory refresh fails or is rate-limited serves nothing. |
| ConversationReference map | Source of truth is **created here** (delivered by Teams per activity; exists nowhere else in the codebase). In-memory `RwLock<HashMap<String, ConversationReference>>`. |
| `bot_identity` (id/name) | Source of truth is the platform (first inbound `activity.recipient`). `OnceCell`, same as `mattermost.rs::bot_identity`. |
| Draft stream state (`streamId`, `streamSequence` per in-flight draft) | Source of truth is **created here** (assigned by Teams / incremented locally per protocol). Ephemeral per-draft map, removed on finalize/cancel, and removed there whether or not anything reached the wire: nothing revisits a handle the orchestrator has fallen back on, and removal happens nowhere else, so a preflight failure that returned early would otherwise leave the entry holding this chat's one stream slot until it aged out. Since an entry is what makes `send_draft` refuse a second concurrent stream, an abandoned turn must not cost the chat the turn that replaced it. |
| Draft update pacing (last update per recipient) | Source of truth is **created here** (when this channel last edited a draft). Keyed by recipient rather than by handle, because the interval it enforces is a per-conversation Connector limit rather than a per-draft one. Only the clear that actually removed a draft drops the key: finalize clears twice around its closing request, and the next turn can open a draft in between, whose floor a second clear would otherwise discard. |
| Effective `stream_mode` | Source of truth is `Config`; the accessor resolves it per call and is the one place a refused `multi_message` becomes `off`, so no caller can act on the raw value. Nothing is cached, so a reload takes effect on the next call. One delivery lifecycle owns every draft, which is why no parallel record of "which path owns this handle" exists to keep in sync. |

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
- Streaming (`partial`): informative → streaming → final activity sequence
  has monotonic `streamSequence` and consistent `streamId` (wiremock);
  `stream_mode = Off` ⇒ `supports_draft_updates()` is false; group chats and
  team channels do not open a draft.
- Refused `multi_message`: the effective mode reads as `off`, the channel
  reports no draft support (capability-wide and per message, from a personal
  chat, the one conversation type that would otherwise stream), and
  `send_draft` hands out no handle, so delivery goes through `send()`.
- Outbound tag strip: a message that is nothing but a tool-call envelope is
  never posted, while prose that merely talks about the tags is sent verbatim.
- Typing: `start_typing()` POSTs a bare `typing` activity.
- Outbound chunking: an in-budget reply is a single unchanged activity; an
  oversize reply splits into chunks that each fit the budget, concatenate
  back to the original, and prefer paragraph/line boundaries. A split reply
  paces its chunks; an unsplit one does not wait. In `partial`, a frame past
  the budget is never posted, an oversize finalize takes the bubble down and
  arrives as ordinary split messages, and an informative line is clamped to
  both documented bounds while a short one passes through untouched.
- Bubble takedown: a cancelled draft closes its stream with a `final`
  message before the delete, on the content it streamed when there is any
  and on the notice when there is not; a refused finalize and an oversize
  answer do the same. A takedown Teams refuses is reported rather than
  swallowed, so a stranded bubble is distinguishable from a removed one.
- Rate limits: a `429` on a content-bearing send is retried and the message
  still lands; a conversation that stays throttled fails after the attempt
  budget rather than retrying forever; a throttled streaming frame is skipped
  without retrying, so the token loop is not stalled. `Retry-After` in
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
lanes do not do, so they run in the dedicated `test-msteams` CI lane (a
required check via the `gate` job).

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
- Per-alias `interrupt_on_new_message` resolution (today the `default`
  alias's value applies channel-wide)
- Code-fence reopening when outbound chunking has to hard-cut inside a
  fenced block that is itself larger than the per-message budget
