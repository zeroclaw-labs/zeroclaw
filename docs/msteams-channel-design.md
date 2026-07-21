# Design: Microsoft Teams Bot Channel (`channel-msteams`)

- Status: proposed
- Date: 2026-07-17
- Scope: MVP — plain-text send/receive, inbound JWT validation, @mention
  gating, DM policy, sender allowlist, streaming draft updates (the gray
  "thinking" message that resolves into the final reply)
- Reference implementations studied:
  - OpenClaw `extensions/msteams/` (TypeScript, Bot Framework model) —
    primary architectural reference
  - `osodevops/ms-teams-cli` (Rust, Graph API delegated-auth model) —
    Rust-level reference for OAuth token flows only; its auth model is
    explicitly NOT suitable for unattended bot messaging (its own
    `docs/auth.md` says bot mode is the correct direction for that)

## 1. Problem

ZeroClaw has 30+ channels but no Microsoft Teams support. The
`microsoft365` module in `zeroclaw-tools` is a Tool (Graph API for
mail/calendar), not a Channel — it cannot receive or send Teams chat
messages as a bot.

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
| Streaming replies | Implemented in MVP via the existing draft pipeline (`send_draft`/`update_draft`/`finalize_draft`), using Teams' native streaming protocol in 1:1 chats and message-edit fallback in group chats/channels. |
| MVP exclusions | Media attachments, Adaptive Cards, SSO, polls, file consent, reactions, message delete |

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
  ├─ validate JWT: signature via JWKS, aud == app_id, issuer check,
  │  expiry (reject 401 before touching the body)
  ├─ only activity.type == "message" produces a ChannelMessage
  ├─ record ConversationReference (service_url, conversation.id,
  │  conversation.conversationType, from.id/name) in the in-memory map
  ├─ text cleanup: strip <at>…</at> mention tags, decode HTML entities
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
  → `jwks_uri` → JWKS (cache with refresh; keys rotate)
- Expected `aud`: the configured `app_id`
- Expected `iss`: `https://api.botframework.com`

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
  └─ POST {service_url}/v3/conversations/{conversation_id}/activities
     body: { "type": "message", "text": ... }
     header: Authorization: Bearer <connector token>
```

`service_url` is taken from the stored ConversationReference (Teams
sends it on every inbound activity); it is never hardcoded.

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
                   <at> tag stripping, HTML entity decoding,
                   conversation-id normalization, mention detection
  conversation.rs  in-memory ConversationReference store
```

### `Channel` trait mapping (MVP)

| Method | Behavior |
| --- | --- |
| `name()` | `"msteams"` |
| `listen()` | axum server on `0.0.0.0:{port}`, route `POST {path}` |
| `send()` | proactive Connector API POST |
| `self_handle()` | bot id from `activity.recipient.id` (set on first inbound) — self-loop guard |
| `self_addressed_mention()` | `<at>BotName</at>` form for the per-channel system prompt |
| `is_direct_message()` | `conversationType == "personal"` |
| `health_check()` | true once listener is bound |
| `supports_draft_updates()` | `true` when `stream_mode == Partial` |
| `supports_draft_updates_for()` | additionally requires a personal (1:1) conversation — group chats and team channels never open drafts (Teams would notify on the placeholder, not the edited answer) |
| `send_draft()` | register a lazy local draft handle only; **no activity is POSTed** and the orchestrator's placeholder text is dropped. The Teams stream opens on the first real update (mirrors OpenClaw's lazy `HttpStream`), so the gray bubble never flashes "..." |
| `update_draft_progress()` | informative update (`streamType: "informative"`) — the gray status text ("thinking…", tool status). Opens the stream if it's the draft's first content. |
| `update_draft()` | content chunk (`streamType: "streaming"`, accumulated text). Opens the stream if it's the draft's first content. |
| `finalize_draft()` | stream opened: final `message` activity (`streamType: "final"`) — the gray bubble is replaced by the normal message and the progress text disappears. Never opened (fast answer): one plain message. |
| `cancel_draft()` | best-effort DELETE of the streaming activity; nothing on the wire if the stream never opened |
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
| `stream_mode` | `StreamMode` | `Off` | `off` / `partial` (progressive draft updates — the gray streaming bubble in 1:1, message edits in groups) / `multi_message`; same enum Telegram/Discord/Lark use |
| `draft_update_interval_ms` | u64 | `1000` | draft flush cadence; also satisfies Teams' ~1/s streaming rate limit |
| `interrupt_on_new_message` | bool | `false` | when `true`, a newer message from the same sender in the same conversation cancels the in-flight agent run and starts a fresh response (history preserved); default queues instead. Feeds the orchestrator's `InterruptFlags`. |

Multiple aliases (`[channels.msteams.<alias>]`) follow the standard
HashMap pattern; each alias runs its own listener, so aliases must use
distinct ports.

## 6. Wiring checklist (mirror of the `mattermost` touchpoints)

| Location | Change |
| --- | --- |
| `crates/zeroclaw-channels/src/lib.rs` | `#[cfg(feature = "channel-msteams")] pub mod msteams;` |
| `crates/zeroclaw-channels/Cargo.toml` | `channel-msteams = ["dep:jsonwebtoken"]`; add to the aggregate feature list. `axum`, `reqwest`, `jsonwebtoken` (v10, aws-lc-rs backend) are already dependencies. |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | `pub use crate::msteams::MsTeamsChannel;`; `"msteams" =>` arm in `build_channel` + `#[cfg(not(...))]` bail arm; configured-channel collection loop; add `msteams` to the "Unknown channel" supported list; add `msteams` field to `InterruptFlags` (mechanical updates to the many test literals). |
| `crates/zeroclaw-channels/src/listing.rs` | `ChannelCompileSpec { schema_name: Some("MSTeams"), type_keys: &["msteams"], compiled: cfg!(feature = "channel-msteams") }` |
| `crates/zeroclaw-config/src/schema.rs` | `MSTeamsConfig` struct + `pub msteams: HashMap<String, MSTeamsConfig>` on the channels struct; add to the `channel.*` allowlist const, `ChannelInfo` list, `is_any_enabled`, row iterator, `Configurable` registration list, `ChannelConfig` impl. |
| `crates/zeroclaw-api/src/attribution.rs` | `ChannelKind` variant `#[strum(serialize = "msteams")] MsTeams` |
| `src/channels/msteams.rs` + `src/channels/mod.rs` | `pub use zeroclaw_channels::msteams::*;` re-export |
| `Cargo.toml` (workspace root), `Containerfile`, `dev/ci/docker-tags.toml`, `setup.bat` | wherever `channel-mattermost` appears in feature lists |
| `docs/book/src/channels/msteams.md` + `SUMMARY.md` + `overview.md` | user-facing setup guide (separate docs PR) |

## 7. Single-source-of-truth compliance (AGENTS.md)

Pre-edit ritual answers for every state-bearing field:

| Field | Verdict |
| --- | --- |
| `app_id` / `app_password` / `tenant_id` | Source of truth is `Config` (`channels.msteams.<alias>`). The channel does NOT copy them into struct fields; it resolves through a `&Config`-backed resolver/closure at use time, following the `peer_resolver` pattern in `mattermost.rs`. |
| Sender allowlist | Source of truth is `Config.peer_groups` (no per-channel `allow_from` field — that would duplicate the peer-group registry). Resolved via the `channel_external_peers` closure at message time, never cached. |
| Connector OAuth token cache | Source of truth is **created here** (issued by Entra at runtime). A time-bounded materialized credential, not a copy of config state. `tokio::sync::OnceCell`/`RwLock` with expiry. |
| JWKS cache | Source of truth is Microsoft's JWKS endpoint; cached copy with refresh-on-rotation is a runtime materialized view. |
| ConversationReference map | Source of truth is **created here** (delivered by Teams per activity; exists nowhere else in the codebase). In-memory `RwLock<HashMap<String, ConversationReference>>`. |
| `bot_identity` (id/name) | Source of truth is the platform (first inbound `activity.recipient`). `OnceCell`, same as `mattermost.rs::bot_identity`. |
| Draft stream state (`streamId`, `streamSequence` per in-flight draft) | Source of truth is **created here** (assigned by Teams / incremented locally per protocol). Ephemeral per-draft map, removed on finalize/cancel. |

## 8. Testing plan

Unit tests (no live Azure):

- JWT validation: expired token, wrong `aud`, wrong issuer, bad
  signature, malformed header → all rejected with 401; valid token
  accepted (test keys generated in-test).
- Activity deserialization: personal vs channel conversation, mention
  entities, `;messageid=` suffix normalization.
- Text cleanup: `<at>` stripping, HTML entity decoding.
- Gating: `allow_dms` on/off; `mention_only` on/off × personal/channel;
  peer-group allowlist filtering.
- Streaming: informative → streaming → final activity sequence has
  monotonic `streamSequence` and consistent `streamId` (wiremock);
  group-chat drafts use PUT edits instead of streaminfo entities;
  `stream_mode = Off` ⇒ `supports_draft_updates()` is false.
- Self-loop guard: activity where `from.id == recipient.id` is dropped.
- `send()`: wiremock stub of `login.microsoftonline.com` token endpoint
  + Connector `/v3/conversations/.../activities`; assert bearer header,
  payload shape, token reuse before expiry (pattern: `webhook.rs`
  tests).
- Unknown-conversation send → clear error (no stored reference).

Manual validation (operator): sideload manifest, DM the bot, @mention it
in a team channel, confirm replies and threading.

## 9. PR split (one concern per PR)

1. **PR1 — plumbing**: `MSTeamsConfig`, `ChannelKind::MsTeams`, feature
   flag, all orchestrator/listing/lib wiring with a stub channel that
   bails "not implemented". Noisy but mechanical (`InterruptFlags` test
   literals).
2. **PR2 — auth**: `auth.rs` (JWKS validation + connector token) with
   unit tests.
3. **PR3 — channel logic**: `listen()`, `send()`, `activity.rs`,
   `conversation.rs`; end-to-end against wiremock.
4. **PR4 — streaming**: draft-pipeline implementation (`send_draft` /
   `update_draft` / `update_draft_progress` / `finalize_draft` /
   `cancel_draft`), streaminfo protocol + group-chat edit fallback,
   `stream_mode` / `draft_update_interval_ms` wiring.
5. **PR5 — docs**: `docs/book/src/channels/msteams.md` setup guide,
   overview/SUMMARY updates.

Risk tier: **Medium** (channel crate behavior change, no security-
boundary weakening; inbound auth is new code and gets focused review in
PR2).

## 10. Deferred (post-MVP)

- ConversationReference persistence (survive restarts)
- Multi-tenant bot support
- Media attachments (inbound download allowlist, outbound upload)
- Adaptive Cards, polls, approval prompts (`request_approval`)
- Reactions, message delete (`redact_message`)
- Graph API enrichment (member lookup for allowlist UPN resolution —
  OpenClaw's `resolve-allowlist.ts` equivalent)
