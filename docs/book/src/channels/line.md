# LINE

ZeroClaw supports LINE via the Messaging API — receiving messages through an embedded webhook server and replying via the Reply API (with Push API fallback when the reply token has expired).

## Prerequisites

1. A [LINE Developers Console](https://developers.line.biz) account.
2. A public HTTPS endpoint reachable from LINE's servers (or ngrok for local development).
3. ZeroClaw built with the `channel-line` feature:

```bash
cargo build --release --features channel-line
```

---

## 1. Create a LINE Bot

1. Log in to the [LINE Developers Console](https://developers.line.biz).
2. Create a **Provider** (or use an existing one).
3. Create a new **Messaging API** channel under that Provider.
4. From the channel settings, collect two values:
   - **Channel Access Token** — Messaging API tab → **Issue** a long-lived token.
   - **Channel Secret** — Basic settings tab.

---

## 2. Configure ZeroClaw

Add the following to your `zeroclaw.toml`:

```toml
[channels_config.line]
enabled = true
channel_access_token = "your-channel-access-token"
channel_secret = "your-channel-secret"

# DM (1:1 chat) access policy. Default: pairing.
# open      — respond to everyone
# pairing   — require one-time /bind <code> handshake on first contact
# allowlist — respond only to LINE user IDs listed in allowed_users
dm_policy = "pairing"

# Group / multi-person chat policy. Default: mention.
# open     — respond to every message
# mention  — respond only when @mentioned
# disabled — ignore all group messages
group_policy = "mention"

# TCP port the embedded webhook server listens on. Default: 8443.
webhook_port = 8443

# Optional: restrict DMs to specific LINE user IDs (used with dm_policy = allowlist).
# allowed_users = ["Uabc123", "Udef456"]

# Optional: per-channel proxy (overrides global [proxy] if set).
# proxy_url = "socks5://127.0.0.1:1080"
```

### Using environment variables instead of config file

If you prefer not to store credentials in the config file, omit the token fields and export them as environment variables instead:

```bash
export LINE_CHANNEL_ACCESS_TOKEN="your-channel-access-token"
export LINE_CHANNEL_SECRET="your-channel-secret"
```

Environment variables take precedence over empty config fields.

---

## 3. Expose the Webhook Endpoint

LINE delivers messages by posting to your webhook URL. The embedded server listens on the configured `webhook_port`.

**For local development (ngrok):**

```bash
ngrok http 8443
```

Copy the `https://` URL ngrok provides (e.g. `https://abc123.ngrok.io`).

**For production:** expose port 8443 (or the port you configured) behind an HTTPS reverse proxy (nginx, Caddy, etc.) or deploy directly on a server with a TLS certificate.

---

## 4. Register the Webhook in LINE Developers Console

1. Go to your channel → **Messaging API** tab → **Webhook settings**.
2. Set **Webhook URL** to `https://your-domain.com/webhook`.
3. Toggle **Use webhook** to on.
4. Click **Verify** — LINE will send a test request. ZeroClaw must be running for verification to succeed.

---

## 5. Start ZeroClaw

```bash
./target/release/zeroclaw --config zeroclaw.toml
```

Or via daemon mode:

```bash
zeroclaw daemon
```

**Startup log signal:**

```
LINE webhook server listening on 0.0.0.0:8443
```

---

## 6. Access Policies

### DM (1:1 chat) — `dm_policy`

| Value | Behaviour |
|---|---|
| `pairing` (default) | The bot ignores all DMs until the user sends `/bind <code>`. A pairing code is displayed in the ZeroClaw log at startup. |
| `open` | The bot responds to every DM immediately. |
| `allowlist` | The bot responds only to LINE user IDs listed in `allowed_users`. |

**Pairing workflow:**

1. ZeroClaw prints a pairing code in the log at startup.
2. The user opens a LINE DM with the bot and sends `/bind <code>`.
3. ZeroClaw confirms the pairing; subsequent DMs are accepted.

### Group / multi-person chat — `group_policy`

| Value | Behaviour |
|---|---|
| `mention` (default) | The bot responds only when explicitly @mentioned. |
| `open` | The bot responds to every message in the group. |
| `disabled` | The bot ignores all group messages entirely. |

---

## 7. Audio / Voice Message Transcription (optional)

When transcription is enabled, LINE `audio` message events are automatically downloaded from the LINE Content API and transcribed before being passed to the model.

```toml
[transcription]
enabled = true
default_provider = "openai"   # openai | local_whisper | deepgram | assemblyai | google
api_key = "sk-..."
model = "whisper-1"
```

For local transcription without a cloud API:

```toml
[transcription]
enabled = true
default_provider = "local_whisper"

[transcription.local_whisper]
url = "http://localhost:8080/v1/transcribe"
max_audio_bytes = 26214400   # 25 MB
timeout_secs = 300
```

The maximum accepted audio size is 25 MB. Larger files are silently skipped with a log warning.

---

## 8. Troubleshooting

| Symptom | Likely cause | Action |
|---|---|---|
| LINE Verify fails | ZeroClaw not running, or port not reachable | Confirm the process is up and the port is accessible from the internet |
| Bot does not reply to DMs | `dm_policy = pairing` and user has not run `/bind` | User must send `/bind <code>` first, or switch to `dm_policy = open` |
| Bot does not reply in groups | `group_policy = mention` and message has no @mention | @mention the bot, or switch to `group_policy = open` |
| Reply arrives as a push message | Reply token expired (~30 s window) | Expected fallback behaviour — no action required |
| Audio messages ignored | `[transcription]` not configured | Add `[transcription]` block with `enabled = true` |

### Log keywords

| Signal | Log message |
|---|---|
| Startup healthy | `LINE webhook server listening on 0.0.0.0:<port>` |
| Signature rejected | `LINE: invalid X-Line-Signature` |
| Unauthorized DM | `LINE: DM from <userId> rejected by policy` |
| Pairing required | `LINE: unpaired user <userId>; ignoring until /bind` |
| Audio ignored (no transcription) | `LINE: audio message ignored (transcription not configured)` |
| Audio transcription failed | `LINE: transcription failed for <messageId>:` |

---

## See also

- [Channels Reference](../reference/api/channels-reference.md) — delivery modes and allowlist semantics for all channels
- [Config Reference](../reference/api/config-reference.md) — full config field index
- [LINE Developers Documentation](https://developers.line.biz/en/docs/messaging-api/)
