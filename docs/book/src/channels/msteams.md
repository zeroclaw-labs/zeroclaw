# Microsoft Teams

Azure Bot Service / Bot Framework integration. Teams delivers activities by
POSTing to an HTTPS endpoint you host; ZeroClaw runs that listener inside the
channel and replies through the Bot Connector API. Feature flag:
`channel-msteams`.

Requires an Azure Bot resource (free F0 tier works) with a single-tenant
Entra app registration. No Microsoft Graph permissions are needed for
messaging.

## Who can talk to the agent

{{#peer-group msteams}}

Allowlist entries match the sender's **Entra (Azure AD) object ID** — stable
across chats, visible under **Microsoft Entra ID → Users → (user) → Object
ID** — or the raw Teams channel-scoped `29:…` ID carried on each message.
Matching is case-insensitive.

## Quickstart

1. Create the Azure Bot + app registration and note the **App ID**, **client
   secret**, and **Tenant ID** (see [Setup](#setup)).
2. Configure the channel:

```toml
[channels.msteams.default]
enabled = true
app_id = "<Azure Bot app (client) ID>"
app_password = "<client secret>"      # secret — use your secrets backend
tenant_id = "<Entra tenant ID>"
port = 3978                           # inbound listener port
# path = "/api/messages"              # webhook route (default)
```

3. Point a public HTTPS domain (reverse proxy, tunnel, etc.) at the listener
   port and register `https://<domain>/api/messages` as the bot's **messaging
   endpoint** in Azure.
4. Add the sender's Entra object ID to the channel's peer group.
5. DM the bot or @-mention it in a team channel.

## Configuration

`app_password` is a secret:

{{#secret-config channels.msteams.<alias>.app_password}}

### Field reference

{{#config-fields channels.msteams}}

Multiple aliases (`[channels.msteams.<alias>]`) each run their own listener
and must use distinct ports.

## Inbound authentication

Every activity POST from Teams carries a Bot Framework service JWT. The
listener validates the RS256 signature against the issuer's published JWKS
(fetched via OpenID discovery, cached, refreshed on key rotation), the
audience (must equal `app_id`), the issuer, and expiry — all **before** the
request body is parsed. Requests that fail any check are rejected with 401.

## Message gating

Inbound messages pass three gates, in order:

1. `allow_dms` — when `false`, personal (1:1) chat messages are dropped
   entirely.
2. `mention_only` — group-chat and team-channel messages must @-mention the
   bot (default on). Personal chats always bypass this gate; a DM is
   definitionally addressed to the bot.
3. **Peer-group allowlist** — the sender must match the channel's peer group
   (empty group = deny everyone, `"*"` = allow everyone).

`<at>…</at>` mention tags and HTML entities are stripped from the text before
it reaches the agent.

## Streaming replies

Set `stream_mode = "partial"` for progressive responses:

- **Personal chats** use Teams' native streaming protocol: a gray
  in-progress bubble shows status lines ("thinking", tool activity) and
  accumulating response text, then is replaced by the final message. Status
  history disappears once the final message lands — this matches the
  built-in Copilot experience.
- **Group chats and team channels** don't support native streaming; the
  channel posts an initial message and edits it as content accumulates.

Updates are throttled by `draft_update_interval_ms` (default 1000 ms —
Teams rate-limits streaming updates to roughly one per second).

`stream_mode = "multi_message"` sends the response as separate messages at
paragraph boundaries instead.

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
2. Teams expects the endpoint to answer within ~15 seconds. The listener
   acknowledges immediately and runs the agent turn asynchronously, so slow
   model calls do not cause redelivery.
3. The bot's identity (`28:…` ID and display name) is learned from the first
   inbound activity; `health_check` reports ready once the listener socket is
   bound.
4. Media attachments, Adaptive Cards, reactions, and message deletion are not
   supported yet.

## See also

- [Channels overview](./overview.md)
- [Peer Groups](./peer-groups.md)
- [Reference: config schema](../reference/config.md)
