# Mattermost

Mattermost integration speaks the native REST API v4. Self-hosted, air-gapped, and sovereign-communication-friendly — your agent's conversation history stays on infrastructure you control.

## Prerequisites

- **Mattermost server.** Self-hosted or cloud. Admin access to the target team.
- **Bot account.** Create via **Main Menu → Integrations → Bot Accounts**. Username like `zeroclaw-bot`. Enable `post:all` and `channel:read` (or whichever scopes you want). Save the **access token**.
- **Channel IDs** to monitor. Click channel header → **View Info** to get each ID.

## Configuration

```toml
[channels.mattermost]
enabled = true
server_url = "https://mattermost.example.com"
access_token = "..."                          # bot account token
team_id = "..."                               # required when bot has multi-team access
allowed_channels = ["7j8k9l..."]              # channel IDs
allowed_users = ["user-id-1", "user-id-2"]    # empty = allow everyone in allowed_channels
mention_only = false                          # true = only respond when @-mentioned
thread_replies = true                         # reply in thread by default
```

Full field reference: [Config](../reference/config.md#channels-mattermost).

## Threading

- **Message inside an existing thread** → reply always lands in that thread.
- **New top-level message**: if `thread_replies = true` (default), the reply creates a new thread rooted on the user's post. If `false`, the reply goes to channel root.

Threading keeps conversations readable in busy channels.

## Mention-only mode

`mention_only = true` adds a second-stage filter after `allowed_users`:

- Messages without `@bot_username` are ignored
- Messages with the mention are processed
- The mention token is stripped before passing content to the model

Useful in high-traffic channels to skip all the "hey does anyone" chatter the bot wasn't meant to see.

## File uploads

Mattermost attachments on inbound messages are stored under `<workspace>/attachments/mattermost/<channel>/`. The agent gets file paths in its context and can read them via `file_read`.

Outbound file uploads are not yet supported — the agent replies with links or inline content only.

## Self-hosting notes

- **TLS**: terminate at your reverse proxy; ZeroClaw makes plain HTTPS requests
- **Webhook vs. WebSocket**: the integration uses WebSocket for inbound real-time events and REST for outbound posts; only the ZeroClaw → Mattermost direction matters for network configuration
- **Rate limits**: Mattermost self-hosted defaults are generous; the bot's draft-update cadence is capped by `draft_update_interval_ms` (default 500)

## Streaming

Mattermost supports draft updates (edits in place) on streaming replies. Multi-message streaming is not enabled — long replies come as one message, sent on stream completion.

## Security

A Mattermost bot token grants the scopes you configured. Treat it as privileged:

- Store in the encrypted secrets backend, not inline in the config
- Rotate if leaked (regenerate in the bot account's settings page)
- Combine with `allowed_users` / `allowed_channels` to limit blast radius even if the token leaks

See [Security → Overview](../security/overview.md) for the broader policy model.
