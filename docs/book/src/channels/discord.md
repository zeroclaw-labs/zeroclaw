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

{{#config-where channels discord}}

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

## Running Discord as a plugin (experimental)

Everything above uses the Discord channel compiled into ZeroClaw. Discord is
also available as an installable WASM channel plugin, the `discord` package
from the plugin registry. The plugin runs inside the plugin sandbox and can
reach only the hosts you grant it. It is experimental and handles text
messages only; the built-in channel remains the default.

> Give each bot token to the built-in channel or the plugin, never both. Discord
> delivers every event to each connection made with the token, so the two
> would both answer every message. Before you move a bot to the plugin, remove
> its `[channels.discord.<alias>]` table. The plugin never reads that table.

### What you need

- A `zeroclaw` binary whose plugin host compiles `.wasm` components, that is,
  one built with the `plugins-wasm-cranelift` feature (see
  [build features](../developing/plugin-protocol.md#build-features)). On a
  binary without the plugin host, the `zeroclaw plugin` command does not
  exist. `zeroclaw plugin install` checks that the package loads on this host
  before it installs anything.
- The bot from the [Quickstart](#quickstart): its token, the **Message Content
  Intent** switched on, and the bot invited to your server.
- The numeric user ID of each person allowed to talk to the agent. Enable
  **Developer Mode** (User Settings -> Advanced), right-click the user, and
  click **Copy User ID**.

### 1. Install the package and bind an instance

Pick an alias for the instance, here `main`, and install the package bound to
it:

```sh
zeroclaw plugin install discord --channel-alias main --egress declared
```

The command downloads `discord` from the registry, verifies it, writes a
`[channels.plugin.main]` binding, and creates the instance's config row.
`--egress declared` grants the hosts the package declares: `discord.com` for
the REST API, and `*.discord.gg` for the gateway WebSocket, since Discord hands
out its gateway and resume hosts under that domain. With `--egress none`
instead, the instance has no network reach until you grant it.

If the package is already installed, bind the alias on its own:

```sh
zeroclaw plugin bind discord --channel-alias main --egress declared
```

[Binding a channel instance](../plugins/index.md#binding-a-channel-instance)
describes both commands and the report they print.

The instance keeps its settings under a key derived from the package, the
`channel` capability, and the alias. For the alias `main` the key is
`zpi1_WyJkaXNjb3JkIiwiY2hhbm5lbCIsIm1haW4iXQ`, and
`zeroclaw plugin info discord` prints the key of every bound alias.

### 2. Set the bot token

The token is a secret. Leave the value off the command line and
`zeroclaw config set` prompts for it without echo and stores it encrypted:

```sh
zeroclaw config set plugins.entries.zpi1_WyJkaXNjb3JkIiwiY2hhbm5lbCIsIm1haW4iXQ.config.bot_token
```

The other settings sit beside the token under `.config.` and are optional:
`guild_ids` and `channel_ids` narrow where the bot listens (see
[Narrowing where the bot listens](#narrowing-where-the-bot-listens)), and
`mention_only` and `listen_to_bots` take `true` or `false`. Set a list as JSON
text:

```sh
zeroclaw config set plugins.entries.zpi1_WyJkaXNjb3JkIiwiY2hhbm5lbCIsIm1haW4iXQ.config.channel_ids '["123456789012345678"]'
```

### 3. Turn it on and route it to an agent

Binding never turns on the plugin system and never assigns the instance to an
agent. Turn the plugin system on:

```sh
zeroclaw config set plugins.enabled true
```

Then add `plugin.main` to the `channels` list of the agent that should answer,
keeping the channels it already has, and admit your users with a peer group on
the same channel:

```toml
[agents.assistant]
channels = ["plugin.main"]

[peer_groups.discord_plugin]
channel = "plugin.main"
external_peers = ["111111111111111111"]
```

The plugin reports each sender by numeric user ID, and a plugin channel
matches senders exactly, so list user IDs, not usernames. Restart ZeroClaw
(`zeroclaw service restart`) to start the instance.

### 4. Check that it runs

- `zeroclaw plugin info discord` reports whether the package loads on this
  host, prints the instance key, and lists anything the instance still needs
  before it can start.
- At startup, the daemon's channel list includes `plugin.main`, and the
  gateway's `/health` snapshot lists a `channel:plugin.main` component. The
  component reads `error` when the plugin's poll fails, and otherwise follows
  the plugin's own health check, which ZeroClaw asks for about every 30
  seconds. What that check covers is up to the plugin, so the proof that the
  bot is connected to Discord is a message from an admitted user reaching the
  agent and the agent's reply appearing in Discord.
- A message from an admitted user is logged as `channel inbound message` with
  `zeroclaw.channel = plugin.main`. A message from anyone else is dropped and
  logged with `error_key = plugin_channel_sender_denied`.
- A connection to a host outside the grant is refused, and the refusal is
  logged with `error_key = plugin_egress_denied`, the refused host, and the
  `zeroclaw config set` command that grants it.

### How the plugin differs from the built-in channel

| | Built-in channel | `discord` plugin |
|---|---|---|
| Configuration | `[channels.discord.<alias>]` | `[channels.plugin.<alias>]` and the instance's `plugins.entries` row |
| Channel name in logs, peer groups, and sessions | `discord.<alias>` | `plugin.<alias>` |
| Network reach | No per-host grant | Only the hosts in the instance's `egress_hosts` |
| Features | Everything on this page | Text messages only: no attachments, threads, slash commands, reactions, streaming, typing indicators, or archive search |

Conversations are kept per channel name, so a bot moved from `discord.main` to
`plugin.main` starts new conversations.

## See also

- [Who can talk to the agent](#who-can-talk-to-the-agent) (peer groups)
- [Plugins](../plugins/index.md) (installing and binding channel plugins)
- [Slack](./slack.md)
- [Channels overview](./overview.md)
