# ZeroClaw Channels Reference

This reference maps channel capabilities, config blocks, allowlist behavior, and setup paths.

Last verified: **February 18, 2026**.

## Quick Commands

```bash
zeroclaw channel list
zeroclaw channel start
zeroclaw channel doctor
zeroclaw channel bind-telegram <IDENTITY>
```

## Channel Matrix

| Channel | Config section | Access control field | Setup path |
|---|---|---|---|
| `CLI` | n/a (always enabled) | n/a | Built-in |
| `Telegram` | `[channels_config.telegram]` | `allowed_users` | `zeroclaw onboard` |
| `Discord` | `[channels_config.discord]` | `allowed_users` | `zeroclaw onboard` |
| `Slack` | `[channels_config.slack]` | `allowed_users` | `zeroclaw onboard` |
| `Mattermost` | `[channels_config.mattermost]` | `allowed_users` | Manual config |
| `Webhook` | `[channels_config.webhook]` | n/a (`secret` optional) | `zeroclaw onboard` or manual |
| `iMessage` | `[channels_config.imessage]` | `allowed_contacts` | `zeroclaw onboard` (macOS) |
| `Matrix` | `[channels_config.matrix]` | `allowed_users` | `zeroclaw onboard` |
| `Signal` | `[channels_config.signal]` | `allowed_from` | Manual config |
| `WhatsApp` | `[channels_config.whatsapp]` | `allowed_numbers` | `zeroclaw onboard` |
| `Email` | `[channels_config.email]` | `allowed_senders` | Manual config |
| `IRC` | `[channels_config.irc]` | `allowed_users` | `zeroclaw onboard` |
| `Lark` | `[channels_config.lark]` | `allowed_users` | Manual config |
| `DingTalk` | `[channels_config.dingtalk]` | `allowed_users` | `zeroclaw onboard` |
| `QQ` | `[channels_config.qq]` | `allowed_users` | `zeroclaw onboard` |

## Deny-by-Default Rules

For channel allowlists, the runtime behavior is intentionally strict:

- Empty allowlist (`[]`) means **deny all**.
- Wildcard (`["*"]`) means **allow all**.
- Explicit IDs are exact matches unless channel-specific docs state otherwise.

### Telegram pairing bootstrap

Telegram has a secure bootstrap flow:

- Keep `allowed_users = []` to start in pairing mode.
- Run `zeroclaw channel bind-telegram <IDENTITY>` to add one identity safely.
- After binding, restart long-running channel processes if needed (`daemon` / `channel start`).

## Minimal Config Examples

### Telegram

```toml
[channels_config.telegram]
bot_token = "123456:ABCDEF"
allowed_users = []
```

### WhatsApp

```toml
[channels_config.whatsapp]
access_token = "EAABx..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
allowed_numbers = ["+1234567890"]
```

### Signal

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
allowed_from = ["+1987654321"]
ignore_attachments = true
ignore_stories = true
```

### Lark

```toml
[channels_config.lark]
app_id = "cli_xxx"
app_secret = "xxx"
allowed_users = ["ou_abc"]
receive_mode = "websocket"   # or "webhook"
# port = 3100                  # required only when receive_mode = "webhook"
```

## Operational Notes

- `zeroclaw channel add/remove` is intentionally not a full config mutator yet; use `zeroclaw onboard` or edit `~/.zeroclaw/config.toml`.
- `zeroclaw channel doctor` validates configured channel health and prints timeout/unhealthy status.
- If `webhook` is configured, doctor guidance points to gateway health check (`GET /health`).

## Related Docs

- [README.md (Channel allowlists)](../README.md#channel-allowlists-deny-by-default)
- [network-deployment.md](network-deployment.md)
- [mattermost-setup.md](mattermost-setup.md)
- [commands-reference.md](commands-reference.md)
