# Discord

Run your ZeroClaw agent as a Discord bot. This guide walks you through it
click by click, no prior bot experience needed. By the end you'll have a bot
sitting in your server that replies when people talk to it.

## Who can talk to the agent

{{#peer-group discord}}

## Quickstart

Five steps: make the bot, copy its token, turn on two switches, invite it, and
start ZeroClaw.

### 1. Create the bot

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications).
2. Click **New Application**, give it a name, and **Create**.
3. In the left sidebar, click **Bot**.
4. Click **Reset Token**, then **Copy**. This long string is your
   `bot_token`. Keep it somewhere safe for step 3, you cannot see it again
   later (only reset it).

> The bot token is a password for your bot. Anyone who has it can control your
> bot. Never paste it into a public chat, screenshot, or commit it to git.

### 2. Turn on the two switches the bot needs

Still on the **Bot** page, scroll to **Privileged Gateway Intents** and toggle
**both** of these on:

- **Message Content Intent** so the bot can read what people type.
- **Server Members Intent** so it can see who is in the server.

Click **Save Changes**. If you skip this, the bot connects but never sees any
messages, which is the single most common "my bot does nothing" cause.

### 3. Tell ZeroClaw about the bot

Put the token from step 1 into your config. The token is a secret, so set it
through a surface that encrypts it rather than typing it into `config.toml`:

{{#config-where channels}}

{{#secret-config channels.discord.<alias>.bot_token}}

### 4. Invite the bot to your server

1. Back in the Developer Portal, open **OAuth2 -> URL Generator**.
2. Under **Scopes**, check **bot**.
3. Under **Bot Permissions**, check at least **Send Messages**, **Read Message
   History**, and **View Channels**.
4. Copy the URL at the bottom, open it in your browser, pick your server, and
   **Authorize**.

The bot now shows up in your member list (offline until you start ZeroClaw).

### 5. Start and test

Start ZeroClaw (`zeroclaw service restart` or `zeroclaw daemon`), then send a
message in a channel the bot can see. It should reply. If it doesn't, jump to
[Troubleshooting](#troubleshooting).

## Configuration

The full field list, derived from the live schema. Most have sensible
defaults; for a basic bot you only ever set `bot_token`.

{{#config-fields channels.discord}}

## Narrowing where the bot listens

By default the bot listens in every server it's invited to and every channel it
can see. To scope it down:

- `guild_ids`: limit the bot to specific servers (guilds). Empty means all.
- `channel_ids`: limit it to specific channels. Empty means all visible.

To find an ID, enable **Developer Mode** in Discord (User Settings -> Advanced),
then right-click a server or channel and **Copy ID**.

## Threads and context

{{#thread-context channel="Discord"}}

## Archive and search

Set `archive = true` and the channel opens a sidecar `discord.db` memory store,
records every message it sees, and registers a `discord_search` tool the agent
can use to look up past conversation. Leave it off if you don't need history
search; the bot still replies normally either way.

## Streaming

{{#streaming channel="Discord" mode="stream_mode" path="channels.discord.<alias>.stream_mode"}}

## Replies that feel natural

- `mention_only`: when `true`, the bot only answers messages that @-mention it,
  so it stays quiet in busy channels.
- `reply_min_interval_secs`: a minimum gap between replies to the same person,
  useful if instant responses feel robotic.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Bot is online but never replies | Message Content Intent is off | Turn it on in the Developer Portal (step 2) and restart |
| Bot replies nowhere | Not invited, or missing View Channels / Send Messages | Re-run the invite (step 4) with the right permissions |
| Bot ignores most messages | `mention_only = true` | @-mention the bot, or set it to `false` |
| "Invalid token" at startup | Token mistyped or reset | Reset the token in the portal, set it again (step 3) |

## See also

- [Who can talk to the agent](#who-can-talk-to-the-agent) (peer groups)
- [Slack](./slack.md)
- [Channels overview](./overview.md)
