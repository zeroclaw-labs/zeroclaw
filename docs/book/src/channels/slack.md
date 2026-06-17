# Slack

Run your ZeroClaw agent as a Slack bot. This guide walks you through it step by
step. By the end you'll have a bot in your workspace that answers when people
message it or @-mention it.

## Who can talk to the agent

{{#peer-group slack}}

## Quickstart

Slack needs two tokens: a **bot token** (what the bot speaks with) and an **app
token** (lets the bot connect without you hosting a public URL). Both come from
the same app page.

### 1. Create the Slack app

1. Go to [api.slack.com/apps](https://api.slack.com/apps) and click
   **Create New App -> From scratch**.
2. Name it, pick your workspace, and **Create App**.

### 2. Add the permissions the bot needs

1. In the left sidebar, open **OAuth & Permissions**.
2. Under **Scopes -> Bot Token Scopes**, add: `app_mentions:read`,
   `channels:history`, `chat:write`, and `channels:read`. (Add `im:history`
   and `im:write` too if you want direct messages.)

### 3. Turn on Socket Mode and get the app token

1. In the left sidebar, open **Socket Mode** and toggle it **on**.
2. Slack prompts you to create an **app-level token**. Name it, give it the
   `connections:write` scope, and **Generate**.
3. Copy the token that starts with `xapp-`. This is your `app_token`.

> Socket Mode lets the bot hold an outbound connection to Slack, so you don't
> need a public webhook URL or any port forwarding. This is the easy path.

### 4. Install the app and get the bot token

1. Open **Install App** in the sidebar and click **Install to Workspace**,
   then **Allow**.
2. Back on **OAuth & Permissions**, copy the **Bot User OAuth Token** that
   starts with `xoxb-`. This is your `bot_token`.

### 5. Tell ZeroClaw about both tokens

Both tokens are secrets, so set them through a surface that encrypts them:

{{#config-where channels slack}}

{{#secret-config channels.slack.<alias>.bot_token}}

Set `app_token` the same way (it's the `xapp-` token from step 3).

**Environment-variable alternative.** Both tokens can be supplied from the
daemon's environment instead of the config file: `bot_token` is resolved from
`ZEROCLAW_SLACK_BOT_TOKEN`, then `SLACK_BOT_TOKEN`; `app_token` from
`ZEROCLAW_SLACK_APP_TOKEN`, then `SLACK_APP_TOKEN`. A value in the config file
takes precedence over the environment. This lets you omit `bot_token` from
`config.toml` entirely (e.g. for secret managers that inject env vars) without
the config failing to load.

### 6. Invite the bot and test

In Slack, go to a channel and type `/invite @YourBotName`. Then send a message
or @-mention the bot. Start ZeroClaw (`zeroclaw service restart` or
`zeroclaw daemon`) and it should reply. If not, see
[Troubleshooting](#troubleshooting).

## Configuration

The full field list, derived from the live schema. For a basic Socket Mode bot
you only set `bot_token` and `app_token`.

{{#config-fields channels.slack}}

## Socket Mode vs HTTP

When `app_token` is set, the bot uses **Socket Mode**: it dials out to Slack,
so no public URL is required. This is the recommended setup and what the
quickstart above uses. Without an `app_token`, Slack must reach your bot over
HTTP, which means hosting a public events endpoint, more setup and more to
secure.

## Threads and context

{{#thread-context channel="Slack" prop="thread_replies" path="channels.slack.<alias>.thread_replies"}}

`strict_mention_in_thread` tightens this further: when `true`, the bot only
answers inside a thread if a message there @-mentions it, instead of replying to
every message in a thread it's part of.

## Mentions and formatting

- `mention_only`: when `true`, the bot only answers messages that @-mention it,
  keeping it quiet in busy channels.
- `use_markdown_blocks`: render replies with Slack Block Kit formatting for
  richer layout. Turn off for plain text.

## Streaming

{{#streaming channel="Slack" mode="stream_drafts" path="channels.slack.<alias>.stream_drafts"}}

`draft_update_interval_ms` controls how often the streaming draft is edited
(raise it if Slack rate-limits the edits), and `cancel_reaction` sets an emoji
users can react with to cancel an in-flight reply.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Bot connects but never replies | Bot not invited to the channel | `/invite @YourBot` in the channel |
| "not_authed" / "invalid_auth" at startup | Wrong or missing `bot_token` | Recopy the `xoxb-` token (step 4) |
| Bot never connects | Missing `app_token` or Socket Mode off | Turn on Socket Mode and set the `xapp-` token (step 3) |
| Bot ignores most messages | `mention_only = true` | @-mention the bot, or set it to `false` |
| Replies have no formatting | `use_markdown_blocks = false` | Set it to `true` |

## See also

- [Who can talk to the agent](#who-can-talk-to-the-agent) (peer groups)
- [Discord](./discord.md)
- [Channels overview](./overview.md)
