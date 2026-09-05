# Mattermost

REST v4 polling and WebSocket client. By default the bot polls channels every 3 seconds for new posts; set `listen_mode = "websocket"` for near-real-time event delivery over a persistent WebSocket connection. Reply posts always go out via `POST /api/v4/posts` regardless of listen mode.

## Who can talk to the agent

{{#peer-group mattermost}}

To allowlist a specific human, copy their user ID from **System Console → User
Management**. Mattermost matches the user **UUID**, not a username, and does
not resolve usernames at message-receive time.

## Quickstart

Configure a Mattermost channel (`url` plus a `bot_token` secret, see [Authentication](#authentication)) through one of the surfaces below. That alone gives you:

1. Auto-discovery of every channel the bot can read across every team it belongs to.
2. DM and group-DM channels auto-discovered and polled alongside team channels.
3. New DMs (created after the bot starts) picked up at the next 60-second discovery refresh.
4. `mention_only` bypassed inside DM and group-DM channels (so 1:1 conversations don't need the bot to be @-mentioned).

To restrict the bot, narrow with `channel_ids`, `team_ids`, or `discover_dms`.

## Configuration

`bot_token` and `password` are secrets:

{{#secret-config channels.mattermost.<alias>.bot_token}}

### Field reference

{{#config-fields channels.mattermost}}

## Channel discovery

There are two scoping modes.

1. **Auto-discovery** (when `channel_ids` is empty or `["*"]`). On startup and every 60 seconds thereafter, the bot calls `GET /api/v4/users/me/channels`, filters the result by `team_ids` (public/private channels) and `discover_dms` (DMs/group DMs), and polls each surviving channel. New DMs created mid-runtime appear at the next refresh.
2. **Explicit** (when `channel_ids` is a non-empty list of IDs other than `*`). On startup the bot calls `GET /api/v4/channels/{id}` for each entry to learn its `type` (so it knows which are DMs for the `mention_only` bypass), then polls exactly those channels forever. No periodic re-discovery.

In both modes each channel has its own `since` cursor: the bot tracks the highest `create_at` it has processed per channel and passes that as `since=<ms>` on the next `GET /api/v4/channels/{id}/posts` call. Cursors do not leak across channels, so a slow-moving channel doesn't suppress posts on a busy one.

## WebSocket mode

Set `listen_mode = "websocket"` to switch from REST polling to a persistent WebSocket connection (`wss://<server>/api/v4/websocket`). The WebSocket mode:

- Delivers new posts in near-real-time (no 3-second poll delay).
- Reduces HTTP load on the Mattermost server (one connection vs. N polls/3s).
- Returns failed sessions to the shared channel supervisor, which reconnects with bounded exponential backoff using the configured `reliability.channel_initial_backoff_secs` and `reliability.channel_max_backoff_secs` values.
- Requires Mattermost v4.0+ (the `/api/v4/websocket` endpoint).

Channel discovery, `mention_only`, `thread_replies`, audio transcription, and peer-group authorization work identically in both modes.

**Trade-offs:**

- WebSocket mode must maintain a persistent TCP+TLS connection.
- During a reconnect window, messages posted to a channel may be missed because this listener does not yet request Mattermost connection resume/replay. Polling catches up via `since=` cursors.
- Polling mode is more resilient to transient network interruptions at the cost of constant HTTP traffic.

To roll back, set `listen_mode = "polling"` (or remove the field; polling is the default).

## Tool approval over chat (`approval_timeout_secs`)

When a tool needs approval (it is in `always_ask`, or the risk profile does not auto-approve it), the agent posts the request into the channel the message came from and waits. There are two ways to answer, and which ones you get depends on `listen_mode`.

**Reply with the token.** This works in every listen mode:

```
APPROVAL REQUIRED [a1b2c3]
Tool: shell
Args: command: pwd

Reply: "a1b2c3 yes", "a1b2c3 no", or "a1b2c3 always"
```

```
a1b2c3 yes
a1b2c3 no
a1b2c3 always
```

A reply that matches a pending token is consumed as a decision and never reaches the model.

**Tap a reaction.** This one requires `listen_mode = "websocket"`. The bot puts ✅ and ❌ on its own prompt post; tapping one answers it. Reactions reach the bot as `reaction_added` WebSocket events, and the polling listener reads posts and never sees them, so **under `listen_mode = "polling"` no emoji are seeded**: a button that silently does nothing on a security prompt is worse than no button. If you want one-tap approvals, set `listen_mode = "websocket"` (see [WebSocket mode](#websocket-mode)).

`always` has no emoji on purpose. It grants a session-scoped allowlist entry rather than permitting a single call, and a mis-tap next to ✅ is too cheap a way to widen a session, so escalating to `always` stays a typed decision.

Mattermost does not offer interactive message *buttons* here. Those post to an integration URL, which requires an inbound HTTP endpoint the bot has no way to expose from a polling or WebSocket client; reactions are the one-tap affordance that works over the connection the bot already holds.

**Who may answer, and where.** The token is a correlator, not a password: it travels in plaintext into the channel, so every member can read it. Two conditions therefore both have to hold. The answering user must be in the alias's [peer group](./peer-groups.md), the same authority that decides whose messages the agent will act on. And the answer must arrive in the channel the prompt was posted into: one bot serves many channels, so a token carried into a different room is not an answer there, even from an authorized user. Binding is per channel rather than per thread, so replying in the channel instead of inside the prompt's thread is fine. A reply or tap that fails either check is logged at `WARN` and ignored, and the prompt stays open so an authorized operator can still answer it in the right place.

**Everyone in the channel can read the tool arguments.** The prompt includes the tool name and a summary of its arguments. Route approvals to a channel whose membership you are comfortable showing those to.

`approval_timeout_secs` bounds the wait. **The default is 300 seconds, and `0` denies immediately** rather than disabling approval, so a zero is a way to refuse every gated tool, not a way to wait forever. On timeout the request is denied and the token is discarded, so a late reply or tap cannot approve a call nobody is waiting on any more. A timeout or an unreachable prompt is recorded as the runtime denying on its own authority, not as a human refusing.

Decisions are final: removing a reaction does not retract an answer, and the first valid answer retires the prompt for both paths.

If the bot cannot place the emoji, most often a permissions problem, it logs `failed to seed Mattermost approval reaction` at `WARN` and the prompt still works by reply.

## Direct messages

Mattermost classifies channels by `type`:

| `type` | meaning |
|---|---|
| `O` | Public team channel. |
| `P` | Private team channel. |
| `G` | Group direct message (multi-user DM). |
| `D` | Direct message (1:1). |

`G` and `D` are treated identically by ZeroClaw: both carry no `team_id`, both are gated by `discover_dms`, and both implicitly bypass `mention_only` (a private conversation has no ambient noise to filter against).

Authorization for DM senders still goes through the channel's peer-group resolver, same as any other channel. `discover_dms` is a knob, not a security boundary; peer groups decide who is allowed to address the agent.

## Threading

1. Inbound post is inside an existing thread (`root_id` is set) → the reply always lands in that thread, regardless of `thread_replies`.
2. Inbound post is top-level and `thread_replies = true` (default) → the reply opens a thread rooted on the inbound post.
3. Inbound post is top-level and `thread_replies = false` → the reply is posted at channel root.

### Context management

{{#thread-context channel="Mattermost" prop="thread_replies" path="channels.mattermost.<alias>.thread_replies"}}

## Authentication

Two paths:

1. **Bot token** (preferred). Create at **System Console → Integrations → Bot Accounts**, copy the access token, store it in `bot_token`. Tokens survive password rotations and are easier to revoke.
2. **Login flow**. Set `login_id` (email or username) and `password`. The bot calls `POST /api/v4/users/login` on startup and caches the returned session token in memory. No persistence to disk.

`bot_token` wins when both are set.

## Voice messages

When `[transcription]` is configured and an inbound post has an audio attachment (mime `audio/*` or extension `ogg`/`mp3`/`m4a`/`wav`/`opus`/`flac`) with no text body, the audio is downloaded via `GET /api/v4/files/{file_id}` and routed through the configured transcription provider. The transcript is prefixed `[Voice]` and becomes the message content. Attachments larger than 25 MB or longer than `transcription.max_duration_secs` are dropped with a WARN.

## Setup

1. In Mattermost: **System Console → Integrations → Bot Accounts → Add Bot Account**. Set a username (e.g. `zeroclaw`), enable the scopes you want.
2. Copy the access token. Store it in your ZeroClaw secrets backend.
3. Invite the bot to whichever teams you want it active in. For DM auto-discovery, no extra invites needed: any user can DM the bot.
4. Create the `mattermost.<alias>` channel referencing the token through the gateway, zerocode, or `zeroclaw config set`.
5. Bind the channel to an agent in `[agents.<alias>]` via `channels = ["mattermost.<alias>"]`.

## Operational notes

1. Poll cadence is 3 seconds per channel. N discovered channels = N HTTP calls every 3 seconds against the Mattermost server. Self-hosted defaults handle this easily; if you're on a shared cloud tenant with tight rate limits, consider scoping with `channel_ids` or `team_ids`.
2. The bot identity is fetched once via `GET /api/v4/users/me` and cached for the process lifetime. Username changes require a restart.
3. The session token from the password login flow is in-memory only. A restart re-logs in.

## See also

- [Channels overview](./overview.md)
- [Peer Groups](./peer-groups.md)
- [Reference: config schema](../reference/config.md)
