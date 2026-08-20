# Microsoft Teams

Azure Bot Service / Bot Framework integration. Teams delivers activities by
POSTing to an HTTPS endpoint you host; ZeroClaw runs that listener inside the
channel and replies through the Bot Connector API. Feature flag:
`channel-msteams`.

Requires an Azure Bot resource (free F0 tier works) with a single-tenant
Entra app registration. No Microsoft Graph permissions are needed for
messaging.

> **Build note.** The Teams channel is opt-in. It is **not** in the `default`
> feature set, and not in the lean `dist` selection used for the prebuilt
> release binaries and the `minimal`, `default-features`, and `dist` container
> tags, so those artifacts cannot run it. It **is** part of the `channels-full`
> bundle, so the published `all-features` container tag and the installer's
> `all` preset do include it. On any other artifact, build from source with
> `cargo build --release --features channel-msteams` for the default set plus
> this channel, or `cargo build --release --features channels-full` for every
> channel. Note that `dist` above names a container tag and a release
> selection, not a Cargo feature, so it cannot be passed to `--features`.

## Who can talk to the agent

{{#peer-group msteams}}

Allowlist entries match the sender's **Entra (Azure AD) object ID** (stable
across chats, visible under **Microsoft Entra ID → Users → (user) → Object
ID**) or the raw Teams channel-scoped `29:…` ID carried on each message.
Matching is case-insensitive.

## Quickstart

1. Create the Azure Bot + app registration and note the **App ID**, **client
   secret**, and **Tenant ID** (see [Setup](#setup)).
2. Open zerocode's [Config pane](../zerocode/config.md) and fill in the Teams
   channel: `enabled`, `app_id`, `app_password`, `tenant_id`, and `port`. Each
   field carries its own description and validation there, and the secret is
   handled as one rather than typed into a file.
3. In the same pane, add the peer group that says who may talk to the bot and
   bind the channel to an agent. An empty allowlist **denies everyone**, so
   this step is required rather than optional.
4. Point a public HTTPS domain (reverse proxy, tunnel, etc.) at the listener
   port and register `https://<domain>/api/messages` as the bot's **messaging
   endpoint** in Azure. With Docker Compose, publish host port `3978` to the
   container's `port`.
5. Restart the daemon so the listener binds on its port (`docker compose
   restart zeroclaw` or equivalent) and confirm `zeroclaw status` shows
   Microsoft Teams as configured.
6. DM the bot or @-mention it in a team channel.

## What the settings persist to

The blocks below are what the Config pane writes, shown so you can see the
result of a given control. Editing them by hand is the fallback for headless
hosts and scripted provisioning; it takes a daemon restart to apply, and the
secret then lives in whatever wrote the file.

Three pieces are required. An empty peer-group allowlist **denies everyone**,
so the channel alone is not enough.

```toml
# 1) Channel credentials and listener
[channels.msteams.default]
enabled = true
app_id = "<Azure Bot app (client) ID>"
app_password = "<client secret>"      # secret — use your secrets backend
tenant_id = "<Entra tenant ID>"
port = 3978                           # inbound Bot Framework listener
# path = "/api/messages"              # webhook route (default)

# Optional behaviour
# allow_dms = true                    # false = ignore personal (1:1) chats
# mention_only = true                 # group/channel must @-mention the bot
# stream_mode = "partial"             # gray "thinking" bubble in 1:1 chats
# draft_update_interval_ms = 1500
# interrupt_on_new_message = false

# 2) Who may talk to the bot (Entra object ID preferred)
[peer_groups.msteams-ops]
channel = "msteams.default"           # or "msteams" for every alias
agents = ["default"]                  # your agent alias
external_peers = [
  "00000000-0000-0000-0000-xxxxxxxxxxxx",
  # "*"                               # temporary: allow anyone (debug only)
]

# 3) Bind the channel to an agent (alongside any other channels)
[agents.default]
channels = ["msteams.default"]
```

Find a user's Object ID under **Microsoft Entra ID → Users → (user) → Object
ID**. The channel also accepts the Teams-scoped `29:…` id, but that value is
less stable across conversations.

A hand-edited file needs a reload/restart before the daemon picks up the new
blocks.

### Docker Compose

The shipped `docker-compose.yml` needs two changes for Teams. Its image does not
carry `channel-msteams`, and it publishes the gateway port only:

```yaml
services:
  zeroclaw:
    # Replaces the `image:` line: no published tag carries the Teams channel,
    # so the binary is built here with the feature added to the default set.
    build:
      context: .
      args:
        ZEROCLAW_CARGO_FLAGS: >-
          --no-default-features
          --features acp-bridge,agent-runtime,channel-acp-server,channel-discord,channel-email,channel-filesystem,channel-lark,channel-matrix,channel-msteams,channel-telegram,channel-webhook,gateway,observability-prometheus,schema-export,whatsapp-web
    ports:
      # Gateway dashboard, unchanged.
      - "${HOST_PORT:-127.0.0.1:42617}:${ZEROCLAW_GATEWAY_PORT:-42617}"
      # The activity listener, matching `port` in the channel block. Keep it on
      # loopback and terminate TLS in your own reverse proxy: Azure requires an
      # HTTPS messaging endpoint, and this listener speaks plain HTTP.
      - "127.0.0.1:3978:3978"
```

The two ports serve different things: the gateway answers dashboard and API
traffic, and this one receives Bot Framework activities.

## Configuration

`app_password` is a secret:

{{#secret-config channels.msteams.<alias>.app_password}}

### Field reference

{{#config-fields channels.msteams}}

Multiple aliases (`[channels.msteams.<alias>]`) each run their own listener
and must use distinct ports.

An enabled channel missing `app_id`, `tenant_id`, or `app_password` refuses to
start and names the field in the daemon log. All three are load-bearing: the
first two authenticate inbound activities, and Entra mints every outbound
Connector token from the secret, so a channel without it would bind and report
itself ready while every reply failed.

## Inbound authentication

Every activity POST from Teams carries a Bot Framework service JWT. The
listener validates the RS256 signature against the issuer's published JWKS
(fetched via OpenID discovery, cached, refreshed on key rotation), the
audience (must equal `app_id`), the issuer, and expiry, all **before** the
request body is parsed. Requests that fail any check are rejected with 401.

## Message gating

Inbound messages pass three gates, in order:

1. `allow_dms`: when `false`, personal (1:1) chat messages are dropped
   entirely.
2. `mention_only`: group-chat and team-channel messages must @-mention the
   bot (default on). Personal chats always bypass this gate; a DM is
   definitionally addressed to the bot.
3. **Peer-group allowlist**: the sender must match the channel's peer group
   (empty group = deny everyone, `"*"` = allow everyone).

The bot's own `<at>…</at>` mention is removed from the text before it reaches
the agent, and HTML entities are decoded. Mentions of **other** users are
unwrapped to their display name (so "@ZeroClaw ask @Alice" reaches the model as
"ask Alice") rather than dropped.

## Streaming replies

Set `stream_mode = "partial"` for progressive responses:

- **Personal chats** use Teams' native streaming protocol: a gray
  in-progress bubble shows status lines ("thinking", tool activity) and
  accumulating response text, then is replaced by the final message. Status
  history disappears once the final message lands; this matches the
  built-in Copilot experience. The stream opens lazily on the first real
  status line or content chunk, so the bubble never flashes a `...`
  placeholder; answers that finish before any intermediate update arrive as
  a single plain message.
- **Group chats and team channels** don't support native streaming and receive
  one final reply. This avoids a notification for an initial placeholder (such
  as `...`) while the completed answer is only an edit. A group chat shows the
  ordinary typing indicator while the turn runs; a team channel shows nothing,
  because Teams has no typing indicator in a channel for anyone, bot or human.

Teams allows one streaming response per chat at a time. If a second question
arrives while the first is still being answered (which happens when
`interrupt_on_new_message` is off), the second answer is delivered as one
ordinary message instead of a second gray bubble.

Two turns running at once share one conversation history, which is worth
knowing before leaving `interrupt_on_new_message` off in a chat that uses
`partial`. The later turn builds its prompt while the earlier question is still
unanswered, so the agent tends to answer both in the later reply, and the
earlier turn then delivers its own answer to the same question again. Turning
`interrupt_on_new_message` on avoids this: the follow-up cancels the in-flight
turn, whose bubble is removed, and only the newer question is answered. The
cost is that any message sent during a slow turn discards it.

Personal-chat updates are throttled by `draft_update_interval_ms` (default
1500 ms, the same headroom Microsoft's own SDK keeps over the one request per
second Teams allows on its streaming API). An update that arrives early is
skipped, which costs nothing because each one carries the full response so far.
Status lines stop once the answer itself starts streaming, because Teams
discards informative updates from that point on.

Teams also requires the streamed text to grow monotonically: each update, and
the final message, must contain what was streamed before it. An agent that runs
tools mid-answer can compose a final response that is not a continuation of the
text streamed earlier, and Teams then refuses the final message. When that
happens the answer is delivered in full as an ordinary message and the
in-progress bubble is removed, so the reply is never left sitting underneath a
frozen draft. The only visible difference is that the bubble disappears instead
of turning into the answer.

The same thing happens to any turn that takes longer than two minutes, which is
Teams' hard limit on a streaming session. Teams stops the bubble at that point
and labels it "this response was stopped"; the answer then arrives as an
ordinary message once the agent finishes, and the stopped bubble is removed.
Expect this on turns with slow tools.

Removing a bubble takes two steps, because Teams only ends a streaming
response when the bot sends its closing message: the bubble is closed first
and the resulting message is then deleted. If the delete does not go through,
what stays in the conversation is a short line saying the response was
superseded, rather than a bubble frozen mid-answer whose Stop button no longer
works.

`stream_mode = "multi_message"` is **not supported on Teams**. Setting it logs a
warning at startup and behaves as `off`: paragraph delivery would publish each
paragraph as a permanent message drawn from mid-turn draft text, which the
outbound credential-redaction pass does not cover, and Teams cannot recall a
message once sent. That draft boundary is shared with Discord and Matrix and is
being addressed there; until it is, Teams offers `off` and `partial` only.

Group chats show the ordinary typing indicator while the turn runs, at any
setting. Team channels show no indicator at all, because Teams has none in a
channel for anyone, so a long turn there is silent until the reply arrives.

`interrupt_on_new_message` is resolved from the `default` alias and applied to
every `msteams` alias: a value set only on a non-`default` alias is not honored,
and enabling it on `default` turns it on for all Teams conversations.

## Long messages

Teams rejects any single activity larger than ~100 KB with a `413`
(`MessageSizeTooBig`). Outbound replies that exceed a conservative size budget
are split into ordered chunks, preferring paragraph, then line, then word
boundaries, so a long response is delivered in full rather than dropped. A reply
that fits the budget is sent unchanged as a single message.

Streaming is held to the same ceiling, and a stream cannot be split, since
every update has to contain the text before it and the bubble closes with a
single message. So in `partial` an answer that outgrows the budget stops
updating the bubble, the bubble is removed, and the answer arrives as ordinary
split messages. Status lines have their own, much smaller limit (1000
characters) and are shortened with a trailing ellipsis if they exceed it.

## Proxying

`proxy_url` covers every call the channel makes outbound, not just the replies:
fetching Microsoft's signing keys (which authenticates incoming activities) and
requesting the Entra access token go through it as well. It falls back to the
global `[proxy]` settings when unset. Inbound traffic is unaffected, since Teams
connects to the endpoint you registered rather than the other way round.

## Rate limits

Teams allows a bot 7 sends per second in one conversation, with tighter budgets
over longer windows (8 per 2 seconds, 60 per 30 seconds, 1800 per hour). That is
separate from the 1 request per second its streaming API allows. Pacing is
handled for you: the chunks of a split reply are spaced 500 ms apart, so a long
answer cannot trip a window on its own.

If Teams does throttle an ordinary reply, it is retried a few times, honoring
the service's `Retry-After` hint, so a brief burst is waited out instead of
failing the reply. A conversation that stays throttled ends in a logged error
rather than an unbounded wait. Throttled streaming frames and typing indicators
are skipped instead of retried, since the next update supersedes them and
waiting would stall the response. A throttled attempt to close the streaming
bubble is not retried either: the answer is sent as an ordinary message
straight away, which is both faster and the only thing that can work once the
two-minute streaming window has passed.

## Threading

Team-channel messages that arrive inside a thread (conversation IDs carrying
a `;messageid=` suffix) are answered in that thread. Top-level team-channel
messages are answered as a thread rooted on the triggering message. Personal
and group chats are flat.

## Setup

Operator-side, all in the Azure portal:

1. **Create the bot**: Azure portal → **Create a resource** → **Azure Bot**.
   Choose *single tenant* and let it create a new app registration (or reuse
   one). The F0 pricing tier is free.
2. **Get credentials**: on the bot's app registration, copy the
   **Application (client) ID** (`app_id`) and **Directory (tenant) ID**
   (`tenant_id`), then create a **client secret** (`app_password`) under
   **Certificates & secrets**.
3. **Set the messaging endpoint**: Azure Bot → **Configuration** →
   `https://<your-domain>/api/messages`. The domain must terminate TLS and
   forward to the channel's `port`.
4. **Enable the Teams channel**: Azure Bot → **Channels** → add
   **Microsoft Teams**.
5. **Install the bot in Teams**: create a minimal Teams app manifest whose
   `bots[0].botId` is the App ID (the *Developer Portal* app in Teams does
   this interactively), then sideload/install it. Personal scope is enough
   for DMs; add team scope to @-mention it in channels.

## Operational notes

1. **Conversation references are in-memory.** Outbound delivery (proactive
   sends, cron delivery, replies) requires the conversation's `serviceUrl`,
   which Teams supplies on inbound activities. After a daemon restart, each
   peer must message the bot once before it can reach them again.
2. **Outbound calls require TLS.** The connector token is equivalent to the
   bot's password, so it is only sent to an `https` service URL (plain `http`
   is accepted for a loopback address, which local test mocks use). Teams
   always supplies an HTTPS `serviceUrl`; if a send fails with a non-HTTPS
   destination error, something between the bot and Teams is rewriting it.
3. Teams expects the endpoint to answer within ~15 seconds. The listener
   acknowledges immediately and runs the agent turn asynchronously, so slow
   model calls do not cause redelivery.
4. The bot's identity (`28:…` ID and display name) is learned from the first
   inbound activity; `health_check` reports ready once the listener socket is
   bound.
5. Media attachments, Adaptive Cards, reactions, and message deletion are not
   supported yet.

## See also

- [Channels overview](./overview.md)
- [Peer Groups](./peer-groups.md)
- [Reference: config schema](../reference/config.md)
