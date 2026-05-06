# Mattermost

Mattermost integration speaks the native REST API v4. Self-hosted, air-gapped, and sovereign-communication-friendly — your agent's conversation history stays on infrastructure you control.

## Prerequisites

- **Mattermost server.** Self-hosted or cloud. Admin access to the target team.
- **Bot account.** Create via **Main Menu → Integrations → Bot Accounts**. Username like `zeroclaw-bot`. Enable `post:all` and `channel:read` (or whichever scopes you want). Save the **bot access token**.
- **Channel ID** (optional) to restrict the bot to a single channel. Click channel header → **View Info** to get the ID.

## Configuration

```toml
[channels.mattermost]
enabled = true
url = "https://mattermost.example.com"
bot_token = "..."                             # bot account token
channel_id = "7j8k9l..."                      # optional: restrict bot to a single channel
allowed_users = ["user-id-1", "user-id-2"]    # empty = deny all
thread_replies = true                         # reply in thread by default
mention_only = false                          # true = only respond when @-mentioned
interrupt_on_new_message = false              # cancel in-flight reply when a newer message arrives
proxy_url = ""                                # optional per-channel proxy override
```

Full field reference: [Config](../reference/config.md#channelsmattermost).

## Threading

- **Message inside an existing thread** → reply always lands in that thread.
- **New top-level message**: if `thread_replies = true` (default), the reply creates a new thread rooted on the user's post. If `false`, the reply goes to channel root.

Threading keeps conversations readable in busy channels.

## Mention-only mode

`mention_only = true` adds a second-stage filter after `allowed_users` and `channel_id`:

- Messages without `@bot_username` are ignored
- Messages with the mention are processed
- The mention token is stripped before passing content to the model

Useful in high-traffic channels to skip all the "hey does anyone" chatter the bot wasn't meant to see.

## Interrupt on new message

When `interrupt_on_new_message = true`, a newer message from the same sender in the same channel cancels the in-flight reply and starts a fresh response with preserved history. Use it when users tend to amend their request mid-flight.

## File uploads

Mattermost attachments on inbound messages are stored under `<workspace>/attachments/mattermost/<channel>/`. The agent gets file paths in its context and can read them via `file_read`.

Outbound file uploads are not yet supported — the agent replies with links or inline content only.

## Self-hosting notes

- **TLS**: terminate at your reverse proxy; ZeroClaw makes plain HTTPS requests
- **Webhook vs. WebSocket**: the integration uses WebSocket for inbound real-time events and REST for outbound posts; only the ZeroClaw → Mattermost direction matters for network configuration
- **Per-channel proxy**: set `proxy_url` to override the global `[proxy]` setting for Mattermost only (`http://`, `https://`, `socks5://`, `socks5h://`)

## Security

A Mattermost bot token grants the scopes you configured. Treat it as privileged:

- Store in the encrypted secrets backend, not inline in the config
- Rotate if leaked (regenerate in the bot account's settings page)
- Combine with `allowed_users` and `channel_id` to limit blast radius even if the token leaks

See [Security → Overview](../security/overview.md) for the broader policy model.
