# Design: Microsoft Teams Bot Channel (`channel-msteams`)

- Status: implemented
- Date: 2026-07-17 (revised after PR review)
- Scope: plain-text send/receive, inbound JWT validation, @mention
  gating, DM policy, sender allowlist, streaming draft updates (the gray
  "thinking" message that resolves into the final reply), paragraph-split
  `multi_message` delivery, typing indicators, and outbound chunking for
  Teams' per-message size limit
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
    Teams native streaming and its 1:1-only limitation
  - [Format your bot messages](https://learn.microsoft.com/en-us/microsoftteams/platform/bots/how-to/format-your-bot-messages):
    the per-activity size limit that drives outbound chunking

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
| Outbound auth | OAuth2 client-credentials against Entra, scope `https://api.botframework.com/.default`, token cached until expiry. |
| Feature flag | `channel-msteams` in `zeroclaw-channels` |
| DM policy | Configurable via `allow_dms` (default `true`). When `false`, inbound personal-chat messages are dropped. |
| Streaming replies | Implemented via the existing draft pipeline (`send_draft`/`update_draft`/`finalize_draft`). `partial` uses Teams' native streaming protocol, which the platform allows in 1:1 chats only — group chats and team channels show a typing indicator and receive one final reply instead. `multi_message` delivers the answer as separate paragraph-sized messages in every conversation type. (A message-edit fallback for groups was implemented and then rejected; see §3.) |
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
- The JWKS cache is bounded on both sides: an unknown `kid` triggers at most
  one re-fetch per 60s (so a flood of garbage tokens cannot drive outbound
  fetches), and the cache is force-refreshed at 24h so a key the issuer has
  *withdrawn* stops being trusted without waiting for a restart.
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
  └─ POST {service_url}/v3/conversations/{conversation_id}/activities
     one request per chunk, in order
     body: { "type": "message", "text": ... }
     header: Authorization: Bearer <connector token>
```

`service_url` is taken from the stored ConversationReference (Teams
sends it on every inbound activity); it is never hardcoded.

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
lives in `send()`, every `stream_mode` is covered, including each
`multi_message` paragraph.

Splitting here rather than asking the model to shorten its answer is
deliberate: the ceiling is a hard, deterministic transport constraint counted
in UTF-16, which a model cannot estimate reliably, and mechanical chunking is
lossless and costs no extra round-trip. This matches every other channel in
the repo (Discord 2 000, Telegram 4 096, Slack 40 000, Lark card ~28 KB).

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
| `listen()` | axum server on `0.0.0.0:{port}`, route `POST {path}` |
| `send()` | proactive Connector API POST |
| `self_handle()` | bot id from `activity.recipient.id` (set on first inbound) — self-loop guard |
| `self_addressed_mention()` | `<at>BotName</at>` form for the per-channel system prompt |
| `is_direct_message()` | `conversationType == "personal"` |
| `health_check()` | true once listener is bound |
| `supports_draft_updates()` | `true` when `stream_mode != Off` (i.e. `partial` or `multi_message`) |
| `supports_draft_updates_for()` | per-message refinement of the above: `off` ⇒ false; `partial` ⇒ personal (1:1) chats only, because Teams' native streaming is 1:1-only (group chats and team channels get a typing indicator plus one final reply, and Teams would otherwise notify on the placeholder rather than the answer); `multi_message` ⇒ true in every conversation type, since paragraph delivery is just a sequence of ordinary sends |
| `supports_multi_message_streaming()` | `true` when `stream_mode == MultiMessage` |
| `multi_message_delay_ms()` | the configured `multi_message_delay_ms` (default 800) |
| `send_draft()` | `partial` (1:1 only): register a lazy local draft handle; **no activity is POSTed** and the orchestrator's placeholder text is dropped, so the Teams stream opens on the first real update (mirrors OpenClaw's lazy `HttpStream`) and the gray bubble never flashes "...". `multi_message`: register per-recipient paragraph state (`sent_len`, `thread_ts`); nothing is POSTed until the first complete paragraph arrives |
| `update_draft_progress()` | `partial`: informative update (`streamType: "informative"`) — the gray status text ("thinking…", tool status); opens the stream if it's the draft's first content. `multi_message`: no-op, since there is no bubble to annotate and status lines must not be delivered as chat messages |
| `update_draft()` | `partial`: content chunk (`streamType: "streaming"`, accumulated text); opens the stream if it's the draft's first content. `multi_message`: flush every paragraph that has fully arrived as its own message, paced by `multi_message_delay_ms` |
| `finalize_draft()` | `partial`: stream opened ⇒ final `message` activity (`streamType: "final"`), replacing the gray bubble and dropping the progress text; never opened (fast answer) ⇒ one plain message. `multi_message`: flush any remaining paragraphs, send the trailing text (which has no closing blank line) as a last message, and drop the per-recipient state |
| `cancel_draft()` | `partial`: best-effort DELETE of the streaming activity; nothing on the wire if the stream never opened. `multi_message`: drop the per-recipient state — paragraphs already sent stay delivered, since Teams cannot recall them |
| `start_typing()` | one-shot `typing` activity (no `streaminfo` entity). Carries the visual feedback in group chats and team channels, where no draft bubble is available |
| `stop_typing()` | no-op — Teams' typing indicator expires on its own |
| everything else | trait defaults (deferred) |

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
  chats and team channels don't open drafts at all: they show the
  ordinary typing indicator and receive one final reply. (A message-edit
  fallback was tried first; Teams notifies on the initial placeholder
  and stays silent on the edit that carries the real answer, which is
  exactly backwards.)
- Updates are rate-limited (~1/s). `draft_update_interval_ms` defaults
  to `1000`; the orchestrator already throttles draft flushes on this
  interval, so no extra limiter is needed.
- `streamSequence` must be monotonically increasing; kept per-draft in
  the in-memory draft state alongside the `streamId`.

### `multi_message` delivery (the alternative to the gray bubble)

`stream_mode = "multi_message"` skips the streaming protocol entirely and
delivers the answer as a sequence of ordinary messages, mirroring what the
same setting does on Discord and Matrix:

- Split points are blank lines (`\n\n`) that are **not** inside a fenced code
  block, so a code block is never cut in half. Text that has no complete
  paragraph yet is held back rather than split mid-sentence.
- Per-recipient state — the `sent_len` byte offset into the accumulated
  response, plus the `thread_ts` anchor so team-channel paragraphs stay
  in-thread — lives in an in-memory map keyed by recipient and is dropped on
  finalize/cancel.
- `multi_message_delay_ms` (default 800) paces consecutive sends, so the
  conversation reads like a person typing several messages in a row.
- A failed paragraph send is logged and swallowed; the finalize pass carries
  whatever remains, so one transient failure cannot truncate the answer.
- Because these are ordinary sends, `multi_message` works in personal chats,
  group chats, and team channels alike — unlike `partial`.

Group chats and team channels also show the ordinary typing indicator while a
turn runs, regardless of `stream_mode`.

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
| `stream_mode` | `StreamMode` | `Off` | `off` / `partial` (the gray native streaming bubble; 1:1 chats only, groups and team channels fall back to typing plus one final reply) / `multi_message` (paragraph-split messages, every conversation type); same enum Telegram/Discord/Lark use |
| `draft_update_interval_ms` | u64 | `1000` | draft flush cadence; also satisfies Teams' ~1/s streaming rate limit |
| `multi_message_delay_ms` | u64 | `800` | pause between consecutive paragraph sends when `stream_mode = "multi_message"` |
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
| `src/channels/msteams.rs` + `src/channels/mod.rs` | `pub use zeroclaw_channels::msteams::*;` re-export |
| `Cargo.toml` (workspace root), `Containerfile`, `dev/ci/docker-tags.toml`, `setup.bat` | wherever `channel-mattermost` appears in feature lists — that is, the `channels-full` bundle and the `all-features` image, but deliberately **not** the lean `dist` set. Consequence: prebuilt release binaries and Docker images do **not** carry Teams; operators build from source with `--features channel-msteams` (or `channels-full`). The user guide states this explicitly. |
| `docs/book/src/channels/msteams.md` + `SUMMARY.md` + `overview.md` | user-facing setup guide (separate docs PR) |

## 7. Single-source-of-truth compliance (AGENTS.md)

Pre-edit ritual answers for every state-bearing field:

| Field | Verdict |
| --- | --- |
| `app_id` / `app_password` / `tenant_id` | Source of truth is `Config` (`channels.msteams.<alias>`). The channel does NOT copy them into struct fields; it resolves through a `&Config`-backed resolver/closure at use time, following the `peer_resolver` pattern in `mattermost.rs`. |
| Sender allowlist | Source of truth is `Config.peer_groups` (no per-channel `allow_from` field — that would duplicate the peer-group registry). Resolved via the `channel_external_peers` closure at message time, never cached. |
| Connector OAuth token cache | Source of truth is **created here** (issued by Entra at runtime). A time-bounded materialized credential, not a copy of config state. `tokio::sync::OnceCell`/`RwLock` with expiry. |
| JWKS cache | Source of truth is Microsoft's JWKS endpoint; the cached copy is a runtime materialized view. Bounded on both sides: at most one re-fetch per 60s when a token names an unknown `kid`, and a forced refresh at 24h so a withdrawn key cannot remain trusted until the next process restart. |
| ConversationReference map | Source of truth is **created here** (delivered by Teams per activity; exists nowhere else in the codebase). In-memory `RwLock<HashMap<String, ConversationReference>>`. |
| `bot_identity` (id/name) | Source of truth is the platform (first inbound `activity.recipient`). `OnceCell`, same as `mattermost.rs::bot_identity`. |
| Draft stream state (`streamId`, `streamSequence` per in-flight draft) | Source of truth is **created here** (assigned by Teams / incremented locally per protocol). Ephemeral per-draft map, removed on finalize/cancel. |
| `multi_message` per-recipient state (`sent_len`, `thread_ts`) | Source of truth is **created here** (how much of an in-flight response has already been delivered; exists nowhere else). Ephemeral map keyed by recipient, dropped on finalize/cancel. `thread_ts` is *borrowed* from the triggering message rather than re-derived. |

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
- Activity deserialization: personal vs channel conversation, mention
  entities, `;messageid=` suffix normalization.
- Text cleanup: the bot's own mention is removed while other mentions are
  unwrapped to display names (non-bot mentions survive into the prompt);
  HTML entity decoding.
- Gating: `allow_dms` on/off; `mention_only` on/off × personal/channel;
  peer-group allowlist filtering.
- Streaming (`partial`): informative → streaming → final activity sequence
  has monotonic `streamSequence` and consistent `streamId` (wiremock);
  `stream_mode = Off` ⇒ `supports_draft_updates()` is false; group chats and
  team channels do not open a draft.
- Streaming (`multi_message`): drafts are supported in every conversation
  type; paragraphs are emitted in order and the tail is flushed on finalize;
  per-recipient state is cleared afterwards.
- Typing: `start_typing()` POSTs a bare `typing` activity.
- Outbound chunking: an in-budget reply is a single unchanged activity; an
  oversize reply splits into chunks that each fit the budget, concatenate
  back to the original, and prefer paragraph/line boundaries.
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
