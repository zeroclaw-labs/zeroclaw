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
