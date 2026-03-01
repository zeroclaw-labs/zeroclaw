# Channels Reference

This document is the canonical reference for channel configuration in ZeroClaw.

For encrypted Matrix rooms, also read the dedicated runbook:
- [Matrix E2EE Guide](./matrix-e2ee-guide.md)

## Quick Paths

- Need a full config reference by channel: jump to [Per-Channel Config Examples](#4-per-channel-config-examples).
- Need a no-response diagnosis flow: jump to [Troubleshooting Checklist](#6-troubleshooting-checklist).
- Need Matrix encrypted-room help: use [Matrix E2EE Guide](./matrix-e2ee-guide.md).
- Need Nextcloud Talk bot setup: use [Nextcloud Talk Setup](./nextcloud-talk-setup.md).
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

One ZeroClaw runtime can serve multiple channels at once: if you configure several
channel sub-tables, `zeroclaw channel start` launches all of them in the same process.
Channel startup is best-effort: a single channel init failure is reported and skipped,
while remaining channels continue running.

## In-Chat Runtime Commands

When running `zeroclaw channel start` (or daemon mode), runtime commands include:

Telegram/Discord sender-scoped model routing:
- `/models` — show available providers and current selection
- `/models <provider>` — switch provider for the current sender session
- `/model` — show current model and cached model IDs (if available)
- `/model <model-id>` — switch model for the current sender session
- `/new` — clear conversation history and start a fresh session

Supervised tool approvals (all non-CLI channels):
- `/approve-request <tool-name>` — create a pending approval request
- `/approve-confirm <request-id>` — confirm pending request (same sender + same chat/channel only)
- `/approve-allow <request-id>` — approve the current pending runtime execution request once (no policy persistence)
- `/approve-deny <request-id>` — deny the current pending runtime execution request
- `/approve-pending` — list pending requests for your current sender+chat/channel scope
- `/approve <tool-name>` — direct one-step approve + persist (`autonomy.auto_approve`, compatibility path)
- `/unapprove <tool-name>` — revoke and remove persisted approval
- `/approvals` — inspect runtime grants, persisted approval lists, and excluded tools

Notes:

- Switching provider or model clears only that sender's in-memory conversation history to avoid cross-model context contamination.
- `/new` clears the sender's conversation history without changing provider or model selection.
- Model cache previews come from `zeroclaw models refresh --provider <ID>`.
- These are runtime chat commands, not CLI subcommands.
- Natural-language approval intents are supported with strict parsing and policy control:
  - `direct` mode (default): `授权工具 shell` grants immediately.
  - `request_confirm` mode: `授权工具 shell` creates pending request, then confirm with request ID.
  - `disabled` mode: approval-management must use slash commands.
- You can override natural-language approval mode per channel via `[autonomy].non_cli_natural_language_approval_mode_by_channel`.
- Approval commands are intercepted before LLM execution, so the model cannot self-escalate permissions through tool calls.
- You can restrict who can use approval-management commands via `[autonomy].non_cli_approval_approvers`.
- Configure natural-language approval mode via `[autonomy].non_cli_natural_language_approval_mode`.
- `autonomy.non_cli_excluded_tools` is reloaded from `config.toml` at runtime; `/approvals` shows the currently effective list.
- Default non-CLI exclusions include both `shell` and `process`; remove `process` from `[autonomy].non_cli_excluded_tools` only when you explicitly want background command execution in chat channels.
- Each incoming message injects a runtime tool-availability snapshot into the system prompt, derived from the same exclusion policy used by execution.

## Inbound Image Marker Protocol

ZeroClaw supports multimodal input through inline message markers:

- Syntax: ``[IMAGE:<source>]``
- `<source>` can be:
  - Local file path
  - Data URI (`data:image/...;base64,...`)
  - Remote URL only when `[multimodal].allow_remote_fetch = true`

Operational notes:

- Marker parsing applies to user-role messages before provider calls.
- Provider capability is enforced at runtime: if the selected provider does not support vision, the request fails with a structured capability error (`capability=vision`).
- Linq webhook `media` parts with `image/*` MIME type are automatically converted to this marker format.

## Channel Matrix

### Build Feature Toggles (`channel-matrix`, `channel-lark`)

Matrix and Lark support are controlled at compile time.

- Default builds include Lark/Feishu (`default = ["channel-lark"]`), while Matrix remains opt-in.
- For a lean local build without Matrix/Lark:

```bash
cargo check --no-default-features --features hardware
```

- Enable Matrix explicitly in a custom feature set:

```bash
cargo check --no-default-features --features hardware,channel-matrix
```

- Enable Lark explicitly in a custom feature set:

```bash
cargo check --no-default-features --features hardware,channel-lark
```

If `[channels_config.matrix]`, `[channels_config.lark]`, or `[channels_config.feishu]` is present but the corresponding feature is not compiled in, `zeroclaw channel list`, `zeroclaw channel doctor`, and `zeroclaw channel start` will report that the channel is intentionally skipped for this build. The same applies to cron delivery: setting `delivery.channel` to a feature-gated channel in a build without that feature will return an error at delivery time. For Matrix cron delivery, only plain rooms are supported; E2EE rooms require listener sessions via `zeroclaw daemon`.

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
| WhatsApp | webhook (Cloud API) or websocket (Web mode) | Cloud API: Yes (public HTTPS callback), Web mode: No |
| Nextcloud Talk | webhook (`/nextcloud-talk`) | Yes (public HTTPS callback) |
| Webhook | gateway endpoint (`/webhook`) | Usually yes |
| Email | IMAP polling + SMTP send | No |
| IRC | IRC socket | No |
| Lark | websocket (default) or webhook | Webhook mode only |
| Feishu | websocket (default) or webhook | Webhook mode only |
| DingTalk | stream mode | No |
| QQ | bot gateway | No |
| Napcat | websocket receive + HTTP send (OneBot) | No (typically local/LAN) |
| Linq | webhook (`/linq`) | Yes (public HTTPS callback) |
| iMessage | local integration | No |
| ACP | stdio (JSON-RPC 2.0) | No |
| Nostr | relay websocket (NIP-04 / NIP-17) | No |

---

## 3. Allowlist Semantics

For channels with inbound sender allowlists:

- Empty allowlist: deny all inbound messages.
- `"*"`: allow all inbound senders (use for temporary verification only).
- Explicit list: allow only listed senders.

Field names differ by channel:

- `allowed_users` (Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/Feishu/DingTalk/QQ/Napcat/Nextcloud Talk/ACP)
- `allowed_from` (Signal)
- `allowed_numbers` (WhatsApp)
- `allowed_senders` (Email/Linq)
- `allowed_contacts` (iMessage)
- `allowed_pubkeys` (Nostr)

### Group-Chat Trigger Policy (Telegram/Discord/Slack/Mattermost/Lark/Feishu)

These channels support an explicit `group_reply` policy:

- `mode = "all_messages"`: reply to all group messages (subject to channel allowlist checks).
- `mode = "mention_only"`: in groups, require explicit bot mention.
- `allowed_sender_ids`: sender IDs that bypass mention gating in groups.

Important behavior:

- `allowed_sender_ids` only bypasses mention gating.
- Sender allowlists (`allowed_users`) are still enforced first.

Example shape:

```toml
[channels_config.telegram.group_reply]
mode = "mention_only"                      # all_messages | mention_only
allowed_sender_ids = ["123456789", "987"] # optional; "*" allowed
```

---

## 4. Per-Channel Config Examples

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
stream_mode = "off"               # optional: off | partial
draft_update_interval_ms = 1000   # optional: edit throttle for partial streaming
mention_only = false              # legacy fallback; used when group_reply.mode is not set
interrupt_on_new_message = false  # optional: cancel in-flight same-sender same-chat request
ack_enabled = true                # optional: send emoji reaction acknowledgments (default: true)

[channels_config.telegram.group_reply]
mode = "all_messages"             # optional: all_messages | mention_only
allowed_sender_ids = []           # optional: sender IDs that bypass mention gate
```

Telegram notes:

- `interrupt_on_new_message = true` preserves interrupted user turns in conversation history, then restarts generation on the newest message.
- Interruption scope is strict: same sender in the same chat. Messages from different chats are processed independently.
- `ack_enabled = false` disables the emoji reaction (⚡️, 👌, 👀, 🔥, 👍) sent to incoming messages as acknowledgment.

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"   # optional
allowed_users = ["*"]
listen_to_bots = false
mention_only = false              # legacy fallback; used when group_reply.mode is not set

[channels_config.discord.group_reply]
mode = "all_messages"             # optional: all_messages | mention_only
allowed_sender_ids = []           # optional: sender IDs that bypass mention gate
```

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # optional
channel_id = "C1234567890"         # optional: single channel; omit or "*" for all accessible channels
allowed_users = ["*"]

[channels_config.slack.group_reply]
mode = "all_messages"              # optional: all_messages | mention_only
allowed_sender_ids = []            # optional: sender IDs that bypass mention gate
```

Slack listen behavior:

- `channel_id = "C123..."`: listen only on that channel.
- `channel_id = "*"` or omitted: auto-discover and listen across all accessible channels.

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"          # required for listening
allowed_users = ["*"]
mention_only = false               # legacy fallback; used when group_reply.mode is not set

[channels_config.mattermost.group_reply]
mode = "all_messages"              # optional: all_messages | mention_only
allowed_sender_ids = []            # optional: sender IDs that bypass mention gate
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
mention_only = false                       # optional: when true, only DM / @mention / reply-to-bot
```

See [Matrix E2EE Guide](./matrix-e2ee-guide.md) for encrypted-room troubleshooting.

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

ZeroClaw supports two WhatsApp backends:

- **Cloud API mode** (`phone_number_id` + `access_token` + `verify_token`)
- **WhatsApp Web mode** (`session_path`, requires build flag `--features whatsapp-web`)

Cloud API mode:

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"     # optional but recommended
allowed_numbers = ["*"]
```

WhatsApp Web mode:

```toml
[channels_config.whatsapp]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
pair_phone = "15551234567"         # optional; omit to use QR flow
pair_code = ""                     # optional custom pair code
allowed_numbers = ["*"]
```

Notes:

- Build with `cargo build --features whatsapp-web` (or equivalent run command).
- Keep `session_path` on persistent storage to avoid relinking after restart.
- Reply routing uses the originating chat JID, so direct and group replies work correctly.

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
imap_id = { enabled = true, name = "zeroclaw", version = "0.1.7", vendor = "zeroclaw-labs" }
```

`imap_id` sends RFC 2971 client metadata right after IMAP login. This is required by some providers
(for example NetEase `163.com` / `126.com`) before mailbox selection is allowed.

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

### 4.11 Lark

```toml
[channels_config.lark]
app_id = "your_lark_app_id"
app_secret = "your_lark_app_secret"
encrypt_key = ""                    # optional
verification_token = ""             # optional
allowed_users = ["*"]
mention_only = false                # legacy fallback; used when group_reply.mode is not set
use_feishu = false
receive_mode = "websocket"          # or "webhook"
port = 8081                          # required for webhook mode

[channels_config.lark.group_reply]
mode = "all_messages"               # optional: all_messages | mention_only
allowed_sender_ids = []             # optional: sender open_ids that bypass mention gate
```

### 4.12 Feishu

```toml
[channels_config.feishu]
app_id = "your_lark_app_id"
app_secret = "your_lark_app_secret"
encrypt_key = ""                    # optional
verification_token = ""             # optional
allowed_users = ["*"]
receive_mode = "websocket"          # or "webhook"
port = 8081                          # required for webhook mode

[channels_config.feishu.group_reply]
mode = "all_messages"               # optional: all_messages | mention_only
allowed_sender_ids = []             # optional: sender open_ids that bypass mention gate
```

Migration note:

- Legacy config `[channels_config.lark] use_feishu = true` is still supported for backward compatibility.
- Prefer `[channels_config.feishu]` for new setups.
- Inbound `image` messages are converted to multimodal markers (`[IMAGE:data:image/...;base64,...]`).
- If image download fails, ZeroClaw forwards fallback text instead of silently dropping the message.

### 4.13 Nostr

```toml
[channels_config.nostr]
private_key = "nsec1..."                   # hex or nsec bech32 (encrypted at rest)
# relays default to relay.damus.io, nos.lol, relay.primal.net, relay.snort.social
# relays = ["wss://relay.damus.io", "wss://nos.lol"]
allowed_pubkeys = ["hex-or-npub"]          # empty = deny all, "*" = allow all
```

Nostr supports both NIP-04 (legacy encrypted DMs) and NIP-17 (gift-wrapped private messages).
Replies automatically use the same protocol the sender used. The private key is encrypted at rest
via the `SecretStore` when `secrets.encrypt = true` (the default).

Interactive onboarding support:

```bash
zeroclaw onboard --interactive
```

The wizard now includes dedicated **Lark** and **Feishu** steps with:

- credential verification against official Open Platform auth endpoint
- receive mode selection (`websocket` or `webhook`)
- optional webhook verification token prompt (recommended for stronger callback authenticity checks)

Runtime token behavior:

- `tenant_access_token` is cached with a refresh deadline based on `expire`/`expires_in` from the auth response.
- send requests automatically retry once after token invalidation when Feishu/Lark returns either HTTP `401` or business error code `99991663` (`Invalid access token`).
- if the retry still returns token-invalid responses, the send call fails with the upstream status/body for easier troubleshooting.

### 4.14 DingTalk

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["*"]
```

### 4.15 QQ

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["*"]
receive_mode = "webhook" # webhook (default) or websocket (legacy fallback)
environment = "production" # production (default) or sandbox
```

Notes:

- `webhook` mode is now the default and serves inbound callbacks at `POST /qq`.
- Set `environment = "sandbox"` to target `https://sandbox.api.sgroup.qq.com` for unpublished bot testing.
- QQ validation challenge payloads (`op = 13`) are auto-signed using `app_secret`.
- `X-Bot-Appid` is checked when present and must match `app_id`.
- Set `receive_mode = "websocket"` to keep the legacy gateway WS receive path.

### 4.16 Napcat (QQ via OneBot)

```toml
[channels_config.napcat]
websocket_url = "ws://127.0.0.1:3001"
api_base_url = "http://127.0.0.1:3001"  # optional; auto-derived when omitted
access_token = ""                         # optional
allowed_users = ["*"]
```

Notes:

- Inbound messages are consumed from Napcat's WebSocket stream.
- Outbound sends use OneBot-compatible HTTP endpoints (`send_private_msg` / `send_group_msg`).
- Recipients:
  - `user:<qq_user_id>` for private messages
  - `group:<qq_group_id>` for group messages
- Outbound reply chaining uses incoming message ids via CQ reply tags.

### 4.17 Nextcloud Talk

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"  # optional but recommended
allowed_users = ["*"]
```

Notes:

- Inbound webhook endpoint: `POST /nextcloud-talk`.
- Signature verification uses `X-Nextcloud-Talk-Random` and `X-Nextcloud-Talk-Signature`.
- If `webhook_secret` is set, invalid signatures are rejected with `401`.
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` overrides config secret.
- See [nextcloud-talk-setup.md](./nextcloud-talk-setup.md) for a full runbook.

### 4.18 Linq

```toml
[channels_config.linq]
api_token = "linq-partner-api-token"
from_phone = "+15551234567"
signing_secret = "optional-webhook-signing-secret"  # optional but recommended
allowed_senders = ["*"]
```

Notes:

- Linq uses the Partner V3 API for iMessage, RCS, and SMS.
- Inbound webhook endpoint: `POST /linq`.
- Signature verification uses `X-Webhook-Signature` (HMAC-SHA256) and `X-Webhook-Timestamp`.
- If `signing_secret` is set, invalid or stale (>300s) signatures are rejected.
- `ZEROCLAW_LINQ_SIGNING_SECRET` overrides config secret.
- `allowed_senders` uses E.164 phone number format (e.g. `+1234567890`).

### 4.19 iMessage

```toml
[channels_config.imessage]
allowed_contacts = ["*"]
```

### 4.18 ACP

ACP (Agent Client Protocol) enables ZeroClaw to act as a client for OpenCode ACP server,
allowing remote control of OpenCode behavior through JSON-RPC 2.0 communication over stdio.

```toml
[channels_config.acp]
opencode_path = "opencode"  # optional, default: "opencode"
workdir = "/path/to/workspace"  # optional
extra_args = []  # optional additional arguments to `opencode acp`
allowed_users = ["*"]  # empty = deny all, "*" = allow all
```

Notes:
- ACP uses JSON-RPC 2.0 protocol over stdio with newline-delimited messages.
- Requires `opencode` binary in PATH or specified via `opencode_path`.
- The channel starts OpenCode subprocess via `opencode acp` command.
- Responses from OpenCode can be sent back to the originating channel when configured.

---

## 5. Validation Workflow

1. Configure one channel with permissive allowlist (`"*"`) for initial verification.
2. Run:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

1. Send a message from an expected sender.
2. Confirm a reply arrives.
3. Tighten allowlist from `"*"` to explicit IDs.

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
rg -n "Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Nostr|Webhook|Channel|ACP" /tmp/zeroclaw.log
```

### 7.2 Keyword table

| Component | Startup / healthy signal | Authorization / policy signal | Transport / failure signal |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Discord | `Discord: connected and identified` | `Discord: ignoring message from unauthorized user:` | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)` |
| Slack | `Slack channel listening on #` / `Slack channel_id not set (or '*'); listening across all accessible channels.` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` / `Slack channel discovery failed:` |
| Mattermost | `Mattermost channel listening on` | `Mattermost: ignoring message from unauthorized user:` | `Mattermost poll error:` / `Mattermost parse error:` |
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` | `Matrix sync error: ... retrying...` |
| Signal | `Signal channel listening via SSE on` | (allowlist checks are enforced by `allowed_from`) | `Signal SSE returned ...` / `Signal SSE connect error:` |
| WhatsApp (channel) | `WhatsApp channel active (webhook mode).` / `WhatsApp Web connected successfully` | `WhatsApp: ignoring message from unauthorized number:` / `WhatsApp Web: message from ... not in allowed list` | `WhatsApp send failed:` / `WhatsApp Web stream error:` |
| Webhook / WhatsApp (gateway) | `WhatsApp webhook verified successfully` | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:` |
| Email | `Email polling every ...` / `Email sent to ...` | `Blocked email from ...` | `Email poll failed:` / `Email poll task panicked:` |
| IRC | `IRC channel connecting to ...` / `IRC registered as ...` | (allowlist checks are enforced by `allowed_users`) | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / Feishu | `Lark: WS connected` / `Lark event callback server listening on` | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:` | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:` |
| DingTalk | `DingTalk: connected and listening for messages...` | `DingTalk: ignoring message from unauthorized user:` | `DingTalk WebSocket error:` / `DingTalk: message channel closed` |
| QQ | `QQ: connected and identified` | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:` | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed` |
| Nextcloud Talk (gateway) | `POST /nextcloud-talk — Nextcloud Talk bot webhook` | `Nextcloud Talk webhook signature verification failed` / `Nextcloud Talk: ignoring message from unauthorized actor:` | `Nextcloud Talk send failed:` / `LLM error for Nextcloud Talk message:` |
| iMessage | `iMessage channel listening (AppleScript bridge)...` | (contact allowlist enforced by `allowed_contacts`) | `iMessage poll error:` |
| ACP | `ACP channel started` | `ACP: ignoring message from unauthorized user:` | `ACP process exited unexpectedly:` / `ACP JSON-RPC timeout:` / `ACP process spawn failed:` |
| Nostr | `Nostr channel listening as npub1...` | `Nostr: ignoring NIP-04 message from unauthorized pubkey:` / `Nostr: ignoring NIP-17 message from unauthorized pubkey:` | `Failed to decrypt NIP-04 message:` / `Failed to unwrap NIP-17 gift wrap:` / `Nostr relay pool shut down` |

### 7.3 Runtime supervisor keywords

If a specific channel task crashes or exits, the channel supervisor in `channels/mod.rs` emits:

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

These messages indicate automatic restart behavior is active, and you should inspect preceding logs for root cause.
