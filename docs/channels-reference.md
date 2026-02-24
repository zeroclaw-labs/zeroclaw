# Channels Reference

This document is the canonical reference for channel configuration in ZeroClaw.

For encrypted Matrix rooms, also read the dedicated runbook:
- [Matrix E2EE Guide](./matrix-e2ee-guide.md)

## Quick Paths

- Need a full config reference by channel: jump to [Per-Channel Config Examples](#4-per-channel-config-examples).
- Need a no-response diagnosis flow: jump to [Troubleshooting Checklist](#6-troubleshooting-checklist).
- Need Matrix encrypted-room help: use [Matrix E2EE Guide](./matrix-e2ee-guide.md).
- Need deployment/network assumptions (polling vs webhook): use [Network Deployment](./network-deployment.md).

## FAQ: Matrix setup passes but no reply

This is the most common symptom (same class as issue #499). Check these in order:

1. **Allowlist mismatch**: `allowed_users` does not include the sender (or is empty).
2. **Wrong room target**: bot is not joined to the configured `room_id` / alias target room.
3. **Token/account mismatch**: token is valid but belongs to another Matrix account.
4. **E2EE device identity gap**: `whoami` does not return `device_id` and config does not provide one.
5. **Key sharing/trust gap**: room keys were not shared to the bot device, so encrypted events cannot be decrypted.
6. **Stale runtime state**: config changed but `zeroclaw daemon` was not restarted.

---

## 1. Configuration Namespace

All channel settings live under `channels_config` in `~/.zeroclaw/config.toml`.

```toml
[channels_config]
cli = true
```

Each channel is enabled by creating its sub-table (for example, `[channels_config.telegram]`).

## In-Chat Runtime Model Switching (Telegram / Discord)

When running `zeroclaw channel start` (or daemon mode), Telegram and Discord now support sender-scoped runtime switching:

- `/models` — show available providers and current selection
- `/models <provider>` — switch provider for the current sender session
- `/model` — show current model and cached model IDs (if available)
- `/model <model-id>` — switch model for the current sender session

Notes:

- Switching clears only that sender's in-memory conversation history to avoid cross-model context contamination.
- Model cache previews come from `zeroclaw models refresh --provider <ID>`.
- These are runtime chat commands, not CLI subcommands.

## Channel Matrix

---

## 2. Delivery Modes at a Glance

| Channel | Receive mode | Public inbound port required? |
|---|---|---|
| CLI | local stdin/stdout | No |
| Telegram | polling | No |
| Discord | gateway/websocket | No |
| Slack | events API | No (token-based channel flow) |
| Mattermost | polling | No |
| Matrix | sync API (supports E2EE) | No |
| Signal | signal-cli HTTP bridge | No (local bridge endpoint) |
| WhatsApp | webhook | Yes (public HTTPS callback) |
| Webhook | gateway endpoint (`/webhook`) | Usually yes |
| Email | IMAP polling + SMTP send | No |
| IRC | IRC socket | No |
| Lark/Feishu | websocket (default) or webhook | Webhook mode only |
| DingTalk | stream mode | No |
| QQ | bot gateway | No |
| iMessage | local integration | No |

---

## 3. Allowlist Semantics

For channels with inbound sender allowlists:

- Empty allowlist: deny all inbound messages.
- `"*"`: allow all inbound senders (use for temporary verification only).
- Explicit list: allow only listed senders.

Field names differ by channel:

- `allowed_users` (Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/DingTalk/QQ)
- `allowed_from` (Signal)
- `allowed_numbers` (WhatsApp)
- `allowed_senders` (Email)
- `allowed_contacts` (iMessage)

---

## 4. Per-Channel Config Examples

### 4.1 Telegram

**Setup**: Create a bot via [@BotFather](https://t.me/BotFather) on Telegram. Copy the bot token.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `bot_token` | String | Yes | — | Telegram Bot API token from @BotFather |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User ID allowlist. `"*"` = allow all |
| `stream_mode` | String | No | `"off"` | Streaming mode: `"off"` or `"partial"` |
| `draft_update_interval_ms` | u64 | No | `1000` | Min ms between message edits (rate limit) |
| `mention_only` | bool | No | `false` | Only respond to @-mentions in groups |
| `voice.enabled` | bool | No | `false` | Enable voice message STT/TTS |
| `voice.api_key` | String | No | — | OpenAI-compatible API key for Whisper/TTS |
| `voice.api_base_url` | String | No | `https://api.openai.com/v1` | STT/TTS API base URL |
| `voice.stt_model` | String | No | `whisper-1` | Speech-to-text model |
| `voice.tts_model` | String | No | `tts-1` | Text-to-speech model |
| `voice.tts_voice` | String | No | `alloy` | Voice identifier for TTS |
| `voice.respond_with_voice` | bool | No | `true` | Send voice reply alongside text |
| `voice.language` | String | No | — | ISO-639-1 language hint (e.g., `"en"`) |
| `voice.max_duration_secs` | u64 | No | `120` | Max voice message duration in seconds |

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["your-user-id"]
mention_only = false

[channels_config.telegram.voice]
enabled = true
api_key = "sk-..."
tts_voice = "alloy"
```

### 4.2 Discord

**Setup**: Create a bot at [Discord Developer Portal](https://discord.com/developers/applications). Enable Gateway intents: GUILDS, GUILD_MESSAGES, MESSAGE_CONTENT, DIRECT_MESSAGES (intents: 37377).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `bot_token` | String | Yes | — | Discord bot token |
| `guild_id` | String | No | — | Guild ID filter (DMs pass through regardless) |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User ID allowlist. `"*"` = allow all |
| `listen_to_bots` | bool | No | `false` | Process messages from other bots |
| `mention_only` | bool | No | `false` | Only respond to @-mentions in guilds |

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"
allowed_users = ["your-user-id"]
listen_to_bots = false
mention_only = false
```

### 4.3 Slack

**Setup**: Create a Slack App at [api.slack.com](https://api.slack.com/apps). Add bot scopes: `chat:write`, `channels:history`, `channels:read`. Polls conversations.history every 3 seconds.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `bot_token` | String | Yes | — | Slack bot token (`xoxb-...`) |
| `app_token` | String | No | — | App-level token (`xapp-...`) |
| `channel_id` | String | No | — | Channel ID to listen on (format: `C1234567`) |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User ID allowlist. `"*"` = allow all |

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."
channel_id = "C1234567890"
allowed_users = ["U1234567890"]
```

### 4.4 Mattermost

**Setup**: Create a bot in Mattermost admin at `/admin/integrations/bots`. Requires self-hosted or cloud Mattermost instance.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `url` | String | Yes | — | Mattermost server URL |
| `bot_token` | String | Yes | — | Bot token |
| `channel_id` | String | No | — | Channel ID to listen on |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User ID allowlist. `"*"` = allow all |
| `thread_replies` | bool | No | `false` | Reply in threads |
| `mention_only` | bool | No | `false` | Only respond to @-mentions |

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"
allowed_users = ["user-id"]
thread_replies = true
```

### 4.5 Matrix

**Setup**: Create a Matrix bot account. Get access token from login. Supports encrypted rooms via matrix-sdk.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `homeserver` | String | Yes | — | Matrix homeserver URL |
| `access_token` | String | Yes | — | Matrix access token |
| `user_id` | String | No | auto-detected | Matrix user ID (`@bot:matrix.org`) |
| `device_id` | String | No | auto-detected | Device ID for E2EE |
| `room_id` | String | Yes | — | Room ID (`!abc:matrix.org`) or alias |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User ID allowlist. `"*"` = allow all |

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_..."
user_id = "@zeroclaw:matrix.example.com"
device_id = "DEVICEID123"
room_id = "!room:matrix.example.com"
allowed_users = ["@you:matrix.example.com"]
```

See [Matrix E2EE Guide](./matrix-e2ee-guide.md) for encrypted-room troubleshooting.

### 4.6 Signal

**Setup**: Requires running [signal-cli](https://github.com/AsamK/signal-cli) HTTP daemon. Listens via SSE at `/api/v1/events`, sends via JSON-RPC at `/api/v1/rpc`.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `http_url` | String | Yes | — | signal-cli HTTP daemon URL |
| `account` | String | Yes | — | E.164 phone number (`+1234567890`) |
| `group_id` | String | No | — | `None` = all, `"dm"` = DMs only, or specific group ID |
| `allowed_from` | Vec\<String\> | No | `[]` (deny all) | Allowed phone numbers (E.164). `"*"` = allow all |
| `ignore_attachments` | bool | No | `false` | Skip attachment-only messages |
| `ignore_stories` | bool | No | `false` | Skip story messages |

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"
allowed_from = ["+1987654321"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

**Setup**: Create a Meta Business App at [developers.facebook.com](https://developers.facebook.com). Add WhatsApp product. Configure webhook to `https://your-domain/whatsapp`. Requires HTTPS (use a tunnel).

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `access_token` | String | Yes | — | Meta Business Suite access token |
| `phone_number_id` | String | Yes | — | Meta Business API phone number ID |
| `verify_token` | String | Yes | — | Webhook verify token (you define this) |
| `app_secret` | String | No | — | App secret for HMAC signature verification |
| `allowed_numbers` | Vec\<String\> | No | `[]` (deny all) | E.164 phone numbers. `"*"` = allow all |

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"
allowed_numbers = ["+1234567890"]
```

### 4.8 Webhook (Gateway)

**Setup**: Generic webhook receiver integrated into the gateway. Not a traditional channel.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `port` | u16 | Yes | — | Port to listen on |
| `secret` | String | No | — | HMAC verification secret |

```toml
[channels_config.webhook]
port = 8080
secret = "optional-shared-secret"
```

### 4.9 Email

**Setup**: Requires IMAP and SMTP credentials. Gmail requires app-specific passwords.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `imap_host` | String | Yes | — | IMAP server hostname |
| `imap_port` | u16 | No | `993` | IMAP port (993 for TLS) |
| `imap_folder` | String | No | `"INBOX"` | IMAP folder to poll |
| `smtp_host` | String | Yes | — | SMTP server hostname |
| `smtp_port` | u16 | No | `465` | SMTP port (465 for TLS) |
| `smtp_tls` | bool | No | `true` | Use TLS for SMTP |
| `username` | String | Yes | — | Email auth username |
| `password` | String | Yes | — | Email auth password |
| `from_address` | String | No | — | Sender address for replies |
| `poll_interval_secs` | u64 | No | `60` | Seconds between IMAP polls |
| `allowed_senders` | Vec\<String\> | No | `[]` (deny all) | Allowed sender emails. `"*"` = allow all |

```toml
[channels_config.email]
imap_host = "imap.gmail.com"
imap_port = 993
smtp_host = "smtp.gmail.com"
smtp_port = 465
smtp_tls = true
username = "bot@gmail.com"
password = "app-specific-password"
from_address = "bot@gmail.com"
poll_interval_secs = 60
allowed_senders = ["you@example.com"]
```

### 4.10 IRC

**Setup**: Connects via TLS. Supports NickServ, SASL (IRCv3), and bouncer (ZNC) passwords.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `server` | String | Yes | — | IRC server hostname |
| `port` | u16 | No | `6697` | IRC port (6697 for TLS) |
| `nickname` | String | Yes | — | Bot nickname |
| `username` | String | No | nickname | IRC username |
| `channels` | Vec\<String\> | Yes | — | Channels to join (`["#general"]`) |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | Nicknames (case-insensitive). `"*"` = allow all |
| `server_password` | String | No | — | Server/bouncer password |
| `nickserv_password` | String | No | — | NickServ IDENTIFY password |
| `sasl_password` | String | No | — | SASL PLAIN password |
| `verify_tls` | bool | No | `true` | Verify TLS certificate |

```toml
[channels_config.irc]
server = "irc.libera.chat"
port = 6697
nickname = "zeroclaw-bot"
channels = ["#zeroclaw"]
allowed_users = ["yournick"]
verify_tls = true
```

### 4.11 Lark / Feishu

**Setup**: Create app at Lark/Feishu developer console. Supports WebSocket (no public URL needed) and webhook modes.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `app_id` | String | Yes | — | App ID from developer console |
| `app_secret` | String | Yes | — | App secret |
| `encrypt_key` | String | No | — | Webhook message decryption key |
| `verification_token` | String | No | — | Webhook validation token |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User/union IDs. `"*"` = allow all |
| `use_feishu` | bool | No | `false` | Use Feishu (China) endpoints |
| `receive_mode` | String | No | `"websocket"` | `"websocket"` or `"webhook"` |
| `port` | u16 | No | — | HTTP port for webhook mode only |

```toml
[channels_config.lark]
app_id = "cli_xxx"
app_secret = "xxx"
allowed_users = ["user-id"]
use_feishu = false
receive_mode = "websocket"
```

### 4.12 DingTalk

**Setup**: Register app at DingTalk Open Platform developer console. Chinese enterprise messaging.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `client_id` | String | Yes | — | Client ID (AppKey) |
| `client_secret` | String | Yes | — | Client Secret (AppSecret) |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | Staff IDs. `"*"` = allow all |

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["staff-id"]
```

### 4.13 QQ

**Setup**: Register at QQ Bot console (Tencent). Supports C2C and group messages.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `app_id` | String | Yes | — | App ID from QQ Bot console |
| `app_secret` | String | Yes | — | App secret |
| `allowed_users` | Vec\<String\> | No | `[]` (deny all) | User IDs. `"*"` = allow all |

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["user-id"]
```

### 4.14 iMessage

**Setup**: macOS/iOS only. Requires iCloud authentication and system access.

| Field | Type | Required | Default | Description |
|-------|------|----------|---------|-------------|
| `allowed_contacts` | Vec\<String\> | No | `[]` (deny all) | Phone numbers or emails. `"*"` = allow all |

```toml
[channels_config.imessage]
allowed_contacts = ["+1234567890"]
```

---

## 5. Validation Workflow

1. Configure one channel with permissive allowlist (`"*"`) for initial verification.
2. Run:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

3. Send a message from an expected sender.
4. Confirm a reply arrives.
5. Tighten allowlist from `"*"` to explicit IDs.

---

## 6. Troubleshooting Checklist

If a channel appears connected but does not respond:

1. Confirm the sender identity is allowed by the correct allowlist field.
2. Confirm bot account membership/permissions in target room/channel.
3. Confirm tokens/secrets are valid (and not expired/revoked).
4. Confirm transport mode assumptions:
   - polling/websocket channels do not need public inbound HTTP
   - webhook channels do need reachable HTTPS callback
5. Restart `zeroclaw daemon` after config changes.

For Matrix encrypted rooms specifically, use:
- [Matrix E2EE Guide](./matrix-e2ee-guide.md)

---

## 7. Operations Appendix: Log Keywords Matrix

Use this appendix for fast triage. Match log keywords first, then follow the troubleshooting steps above.

### 7.1 Recommended capture command

```bash
RUST_LOG=info zeroclaw daemon 2>&1 | tee /tmp/zeroclaw.log
```

Then filter channel/gateway events:

```bash
rg -n "Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Webhook|Channel" /tmp/zeroclaw.log
```

### 7.2 Keyword table

| Component | Startup / healthy signal | Authorization / policy signal | Transport / failure signal |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Discord | `Discord: connected and identified` | `Discord: ignoring message from unauthorized user:` | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)` |
| Slack | `Slack channel listening on #` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` |
| Mattermost | `Mattermost channel listening on` | `Mattermost: ignoring message from unauthorized user:` | `Mattermost poll error:` / `Mattermost parse error:` |
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` | `Matrix sync error: ... retrying...` |
| Signal | `Signal channel listening via SSE on` | (allowlist checks are enforced by `allowed_from`) | `Signal SSE returned ...` / `Signal SSE connect error:` |
| WhatsApp (channel) | `WhatsApp channel active (webhook mode).` | `WhatsApp: ignoring message from unauthorized number:` | `WhatsApp send failed:` |
| Webhook / WhatsApp (gateway) | `WhatsApp webhook verified successfully` | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:` |
| Email | `Email polling every ...` / `Email sent to ...` | `Blocked email from ...` | `Email poll failed:` / `Email poll task panicked:` |
| IRC | `IRC channel connecting to ...` / `IRC registered as ...` | (allowlist checks are enforced by `allowed_users`) | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / Feishu | `Lark: WS connected` / `Lark event callback server listening on` | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:` | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:` |
| DingTalk | `DingTalk: connected and listening for messages...` | `DingTalk: ignoring message from unauthorized user:` | `DingTalk WebSocket error:` / `DingTalk: message channel closed` |
| QQ | `QQ: connected and identified` | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:` | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed` |
| iMessage | `iMessage channel listening (AppleScript bridge)...` | (contact allowlist enforced by `allowed_contacts`) | `iMessage poll error:` |

### 7.3 Runtime supervisor keywords

If a specific channel task crashes or exits, the channel supervisor in `channels/mod.rs` emits:

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

These messages indicate automatic restart behavior is active, and you should inspect preceding logs for root cause.

