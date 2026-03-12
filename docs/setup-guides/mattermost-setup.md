# Mattermost Integration Guide

ZeroClaw supports native integration with Mattermost via its REST API v4. This integration is ideal for self-hosted, private, or air-gapped environments where sovereign communication is a requirement.

## Prerequisites

1.  **Mattermost Server**: A running Mattermost instance (self-hosted or cloud).
2.  **Bot Account**:
    - Go to **Main Menu > Integrations > Bot Accounts**.
    - Click **Add Bot Account**.
    - Set a username (e.g., `zeroclaw-bot`).
    - Enable **post:all** and **channel:read** permissions (or appropriate scopes).
    - Save the **Access Token**.
3.  **Channel ID**:
    - Open the Mattermost channel you want the bot to monitor.
    - Click the channel header and select **View Info**.
    - Copy the **ID** (e.g., `7j8k9l...`).

## Configuration

Add the following to your `config.toml` under the `[channels_config]` section:

```toml
[channels_config.mattermost]
url = "https://mm.your-domain.com"
bot_token = "your-bot-access-token"
channel_id = "your-channel-id"
allowed_users = ["user-id-1", "user-id-2"]
thread_replies = true
mention_only = true
```

### Configuration Fields

| Field | Description |
|---|---|
| `url` | The base URL of your Mattermost server. |
| `bot_token` | The Personal Access Token for the bot account. |
| `channel_id` | (Optional) The ID of the channel to listen to. Required for `listen` mode. |
| `allowed_users` | (Optional) A list of Mattermost User IDs permitted to interact with the bot. Use `["*"]` to allow everyone. |
| `thread_replies` | (Optional) Whether top-level user messages should be answered in a thread. Default: `true`. Existing thread replies always remain in-thread. |
| `mention_only` | (Optional) When `true`, only messages that explicitly mention the bot username (for example `@zeroclaw-bot`) are processed. Default: `false`. |

## Threaded Conversations

ZeroClaw supports Mattermost threads in both modes:
- If a user sends a message in an existing thread, ZeroClaw always replies within that same thread.
- If `thread_replies = true` (default), top-level messages are answered by threading on that post.
- If `thread_replies = false`, top-level messages are answered at channel root level.

## Mention-Only Mode

When `mention_only = true`, ZeroClaw applies an extra filter after `allowed_users` authorization:

- Messages without an explicit bot mention are ignored.
- Messages with `@bot_username` are processed.
- The `@bot_username` token is stripped before sending content to the model.

This mode is useful in busy shared channels to reduce unnecessary model calls.

## Security Note

Mattermost integration is designed for **sovereign communication**. By hosting your own Mattermost server, your agent's communication history remains entirely within your own infrastructure, avoiding third-party cloud logging.
