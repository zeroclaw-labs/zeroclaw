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

Add the following to your `config.toml` under the `[channels]` section:

```toml
[channels.mattermost]
url = "https://mm.your-domain.com"
bot_token = "your-bot-access-token"
channel_id = "your-channel-id"
allowed_users = ["user-id-1", "user-id-2"]
```

### Configuration Fields

| Field | Description |
|---|---|
| `url` | The base URL of your Mattermost server. |
| `bot_token` | The Personal Access Token for the bot account. |
| `channel_id` | (Optional) The ID of the channel to listen to. Required for `listen` mode. |
| `allowed_users` | (Optional) A list of Mattermost User IDs permitted to interact with the bot. Use `["*"]` to allow everyone. |

## Threaded Conversations

ZeroClaw automatically supports Mattermost threads. 
- If a user sends a message in a thread, ZeroClaw will reply within that same thread.
- If a user sends a top-level message, ZeroClaw will start a thread by replying to that post.

## Security Note

Mattermost integration is designed for **sovereign communication**. By hosting your own Mattermost server, your agent's communication history remains entirely within your own infrastructure, avoiding third-party cloud logging.
