# Channels Reference

This document is the canonical reference for channel configuration in ZeroClaw.

## Quick Paths

- Need a full config reference by channel: jump to [Per-Channel Config Examples](#4-per-channel-config-examples).
- Need a no-response diagnosis flow: jump to [Troubleshooting Checklist](#6-troubleshooting-checklist).
- Need deployment/network assumptions (polling vs webhook): use [Network Deployment](../../ops/network-deployment.md).

## In-Chat Runtime Model Switching (Telegram)

When running `zeroclaw channel start` (or daemon mode), Telegram supports sender-scoped runtime switching:

- `/models` — show available providers and current selection
- `/models <provider>` — switch provider for the current sender session
- `/model` — show current model and cached model IDs (if available)
- `/model <model-id>` — switch model for the current sender session
- `/new` — clear conversation history and start a fresh session

Notes:

- Switching provider or model clears only that sender's in-memory conversation history to avoid cross-model context contamination.
- `/new` clears the sender's conversation history without changing provider or model selection.
- Model cache previews come from `zeroclaw models refresh --provider <ID>`.
- These are runtime chat commands, not CLI subcommands.

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

---

## 1. Configuration Namespace

All channel settings live under `channels_config` in `~/.zeroclaw/config.toml`.

```toml
[channels_config]
cli = true
```

Each channel is enabled by creating its sub-table (for example, `[channels_config.telegram]`).

---

## 2. Delivery Modes at a Glance

| Channel | Receive mode | Public inbound port required? |
|---|---|---|
| CLI | local stdin/stdout | No |
| Telegram | polling | No |
| Slack | events API | No (token-based channel flow) |

---

## 3. Allowlist Semantics

For channels with inbound sender allowlists:

- Empty allowlist: deny all inbound messages.
- `"*"`: allow all inbound senders (use for temporary verification only).
- Explicit list: allow only listed senders.

Field names differ by channel:

- `allowed_users` (Telegram/Slack)

---

## 4. Per-Channel Config Examples

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
stream_mode = "off"               # optional: off | partial
draft_update_interval_ms = 1000   # optional: edit throttle for partial streaming
mention_only = false              # optional: require @mention in groups
interrupt_on_new_message = false  # optional: cancel in-flight same-sender same-chat request
```

Telegram notes:

- `interrupt_on_new_message = true` preserves interrupted user turns in conversation history, then restarts generation on the newest message.
- Interruption scope is strict: same sender in the same chat. Messages from different chats are processed independently.

### 4.2 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # optional
channel_id = "C1234567890"         # optional: single channel; omit or "*" for all accessible channels
channel_ids = ["C1234567890"]      # optional: explicit channel list; takes precedence over channel_id
allowed_users = ["*"]
```

Slack listen behavior:

- `channel_ids = ["C123...", "D456..."]`: listen only on the listed channels/DMs.
- `channel_id = "C123..."`: listen only on that channel.
- `channel_id = "*"` or omitted: auto-discover and listen across all accessible channels.

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
5. Restart `zeroclaw daemon` after config changes.

---

## 7. Operations Appendix: Log Keywords

### 7.1 Recommended capture command

```bash
RUST_LOG=info zeroclaw daemon 2>&1 | tee /tmp/zeroclaw.log
```

Then filter channel/gateway events:

```bash
rg -n "Telegram|Slack|Channel" /tmp/zeroclaw.log
```

### 7.2 Keyword table

| Component | Startup / healthy signal | Authorization / policy signal | Transport / failure signal |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Slack | `Slack channel listening on #` / `Slack channel_id not set (or '*'); listening across all accessible channels.` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` / `Slack channel discovery failed:` |

### 7.3 Runtime supervisor keywords

If a specific channel task crashes or exits, the channel supervisor in `channels/mod.rs` emits:

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

These messages indicate automatic restart behavior is active, and you should inspect preceding logs for root cause.
