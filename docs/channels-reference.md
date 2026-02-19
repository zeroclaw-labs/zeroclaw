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

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
```

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"   # optional
allowed_users = ["*"]
listen_to_bots = false
mention_only = false
```

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # optional
channel_id = "C1234567890"         # optional
allowed_users = ["*"]
```

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"          # required for listening
allowed_users = ["*"]
```

### 4.5 Matrix

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_..."
user_id = "@zeroclaw:matrix.example.com"   # optional, recommended for E2EE
device_id = "DEVICEID123"                  # optional, recommended for E2EE
room_id = "!room:matrix.example.com"       # or room alias (#ops:matrix.example.com)
allowed_users = ["*"]
```

See [Matrix E2EE Guide](./matrix-e2ee-guide.md) for encrypted-room troubleshooting.

Notes:
- Outbound Matrix replies are emitted as markdown-capable `m.room.message` text content so common clients can render lists, emphasis, and code blocks.
- If you still see `matrix_sdk_crypto::backups` warnings, follow the backup/recovery section in the Matrix E2EE guide.

### 4.6 Signal

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"                    # optional: "dm" / group id / omitted
allowed_from = ["*"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"     # optional but recommended
allowed_numbers = ["*"]
```

### 4.8 Webhook Channel Config (Gateway)

`channels_config.webhook` enables webhook-specific gateway behavior.

```toml
[channels_config.webhook]
port = 8080
secret = "optional-shared-secret"
```

Run with gateway/daemon and verify `/health`.

### 4.9 Email

```toml
[channels_config.email]
imap_host = "imap.example.com"
imap_port = 993
imap_folder = "INBOX"
smtp_host = "smtp.example.com"
smtp_port = 465
smtp_tls = true
username = "bot@example.com"
password = "email-password"
from_address = "bot@example.com"
poll_interval_secs = 60
allowed_senders = ["*"]
```

### 4.10 IRC

```toml
[channels_config.irc]
server = "irc.libera.chat"
port = 6697
nickname = "zeroclaw-bot"
username = "zeroclaw"              # optional
channels = ["#zeroclaw"]
allowed_users = ["*"]
server_password = ""                # optional
nickserv_password = ""              # optional
sasl_password = ""                  # optional
verify_tls = true
```

### 4.11 Lark / Feishu

```toml
[channels_config.lark]
app_id = "cli_xxx"
app_secret = "xxx"
encrypt_key = ""                    # optional
verification_token = ""             # optional
allowed_users = ["*"]
use_feishu = false
receive_mode = "websocket"          # or "webhook"
port = 8081                          # required for webhook mode
```

### 4.12 DingTalk

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["*"]
```

### 4.13 QQ

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["*"]
```

### 4.14 iMessage

```toml
[channels_config.imessage]
allowed_contacts = ["*"]
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
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` / `Matrix room-key backup is enabled for this device.` / `Matrix device '...' is verified for E2EE.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` / `Matrix room-key backup is not enabled for this device...` / `Matrix device '...' is not verified...` | `Matrix sync error: ... retrying...` |
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
