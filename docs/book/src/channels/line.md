# LINE

ZeroClaw supports LINE via the Messaging API, receiving messages through an embedded webhook server and replying via the Reply API (with Push API fallback when the reply token has expired).

## Who can talk to the agent

{{#peer-group line}}

LINE layers `dm_policy` and `group_policy` on top of one alias-wide peer set;
see [Access Policies](#6-access-policies) below. Every enabled group policy
requires a peer. For DMs, the peer set is enforced by `allowlist` and supplies
the identities already accepted by `pairing`.

## Prerequisites

1. A [LINE Developers Console](https://developers.line.biz) account.
2. A public HTTPS endpoint reachable from LINE's servers (or ngrok for local development).
3. ZeroClaw built with LINE channel support enabled (the `channel-line` feature on the `zeroclaw-channels` crate).

---

## 1. Create a LINE Bot

1. Log in to the [LINE Developers Console](https://developers.line.biz).
2. Create a **Provider** (or use an existing one).
3. Create a new **Messaging API** channel under that Provider.
4. From the channel settings, collect two values:
   - **Channel Access Token**: Messaging API tab → **Issue** a long-lived token.
   - **Channel Secret**: Basic settings tab.

---

## 2. Configure ZeroClaw

{{#config-fields channels.line}}

Configure the LINE channel under `[channels.line.<alias>]` with at minimum `channel_access_token` and `channel_secret`. The `dm_policy` / `group_policy` user-facing semantics are covered in §6 below.

### Using environment variables instead of config file

If you prefer not to store credentials in the config file, omit the token fields and export them as environment variables instead:

<div class="os-tabs-src">

#### sh

```sh
export LINE_CHANNEL_ACCESS_TOKEN="your-channel-access-token"
export LINE_CHANNEL_SECRET="your-channel-secret"
```

</div>

Environment variables take precedence over empty config fields.

---

## 3. Expose the Webhook Endpoint

LINE delivers messages by posting to your webhook URL. The embedded server listens on the configured `webhook_port`.

**For local development (ngrok):**

<div class="os-tabs-src">

#### sh

```sh
ngrok http 8443
```

</div>

Copy the `https://` URL ngrok provides (e.g. `https://abc123.ngrok.io`).

**For production:** expose port 8443 (or the port you configured) behind an HTTPS reverse proxy (nginx, Caddy, etc.) or deploy directly on a server with a TLS certificate.

---

## 4. Register the Webhook in LINE Developers Console

1. Go to your channel → **Messaging API** tab → **Webhook settings**.
2. Set **Webhook URL** to `https://your-domain.com/line/webhook`.
3. Toggle **Use webhook** to on.
4. Click **Verify**, LINE will send a test request. ZeroClaw must be running for verification to succeed.

---

## 5. Start ZeroClaw

<div class="os-tabs-src">

#### sh

```sh
zeroclaw daemon
```

</div>

**Startup log signal:**

```
LINE: webhook server listening on http://0.0.0.0:8443/line/webhook
```

---

## 6. Access Policies

### DM (1:1 chat): `dm_policy`

| Value | Behaviour |
|---|---|
| `pairing` (default) | The bot ignores all DMs until the user sends `/bind <code>`. A pairing code is displayed in the ZeroClaw log at startup. |
| `open` | The bot responds to every DM immediately. |
| `allowlist` | The bot responds only to LINE user IDs in the agent's peer set (see [Who can talk to the agent](#who-can-talk-to-the-agent)). |

**Pairing workflow:**

1. ZeroClaw prints a pairing code in the log at startup.
2. The user opens a LINE DM with the bot and sends `/bind <code>`.
3. ZeroClaw confirms the pairing; subsequent DMs are accepted.

### Group / multi-person chat: `group_policy`

`group_policy` decides when the bot is being addressed, not who may address it.
Every enabled mode also requires the sender to be in the peer set, so a member
of a joined group who is not a peer cannot drive the agent.

| Value | Behaviour |
|---|---|
| `mention` (default) | The bot responds only when explicitly @mentioned, and only to a peer. |
| `open` | The bot responds without needing a mention, still only to a peer. |
| `disabled` | The bot ignores all group messages entirely. |

`external_peers = ["*"]` is channel-wide, not group-scoped. It accepts every
group or room sender and every DM sender for that LINE alias. In
`dm_policy = "pairing"`, a non-empty peer set also means no startup pairing
guard is issued. Use the wildcard only when both the public-room and public-DM
effects are intended. Pairing remains a DM-only handshake and is never offered
inside a group or room.

---

## 7. Audio / Voice Message Transcription (optional)

When transcription is enabled (via the global `[transcription]` config, see [Config reference](../reference/config.md)), LINE `audio` message events are automatically downloaded from the LINE Content API and transcribed before being passed to the model.

The maximum accepted audio size is 25 MB. Larger files are silently skipped with a log warning.

---

## 8. Troubleshooting

| Symptom | Likely cause | Action |
|---|---|---|
| LINE Verify fails | ZeroClaw not running, or port not reachable | Confirm the process is up and the port is accessible from the internet |
| Bot does not reply to DMs | `dm_policy = pairing` and user has not run `/bind` | User must send `/bind <code>` first, or switch to `dm_policy = open` |
| Bot does not reply in groups | Message has no required @mention, or the sender is absent from the peer set | @mention the bot when required and add the sender to the alias-wide peer set |
| Reply arrives as a push message | Reply token expired (~30 s window) | Expected fallback behaviour, no action required |
| Audio messages ignored | `[transcription]` not configured | Add `[transcription]` block with `enabled = true` |

### Log keywords

| Condition | Log message |
|---|---|
| Startup healthy | `LINE: webhook server listening on http://0.0.0.0:<port>/line/webhook` |
| Signature rejected | `LINE: invalid X-Line-Signature` |
| Unauthorized DM | `LINE: DM from <userId> rejected by policy` |
| Unauthorized group or room sender | `ignoring group message from unauthorized sender` |
| Pairing required | `LINE: unpaired user <userId>; ignoring until /bind` |
| Audio ignored (no transcription) | `LINE: audio message ignored (transcription not configured)` |
| Audio transcription failed | `LINE: transcription failed for <messageId>:` |

---

## See also

- [Config reference](../reference/config.md): full config field index
- [LINE Developers Documentation](https://developers.line.biz/en/docs/messaging-api/)
