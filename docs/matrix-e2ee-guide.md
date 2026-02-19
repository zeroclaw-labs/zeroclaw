# Matrix E2EE Guide

This guide explains how to run ZeroClaw reliably in Matrix rooms, including end-to-end encrypted (E2EE) rooms.

It focuses on the common failure mode reported by users:

> “Matrix is configured correctly, checks pass, but the bot does not respond.”

## 0. Fast FAQ (#499-class symptom)

If Matrix appears connected but there is no reply, validate these first:

1. Sender is allowed by `allowed_users` (for testing: `["*"]`).
2. Bot account has joined the exact target room.
3. Token belongs to the same bot account (`whoami` check).
4. Encrypted room has usable device identity (`device_id`) and key sharing.
5. Daemon is restarted after config changes.

---

## 1. Requirements

Before testing message flow, make sure all of the following are true:

1. The bot account is joined to the target room.
2. The access token belongs to the same bot account.
3. `room_id` is correct:
   - preferred: canonical room ID (`!room:server`)
   - supported: room alias (`#alias:server`) and ZeroClaw will resolve it
4. `allowed_users` allows the sender (`["*"]` for open testing).
5. For E2EE rooms, the bot device has received encryption keys for the room.

---

## 2. Configuration

Use `~/.zeroclaw/config.toml`:

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_your_token"

# Optional but recommended for E2EE stability:
user_id = "@zeroclaw:matrix.example.com"
device_id = "DEVICEID123"

# Room ID or alias
room_id = "!xtHhdHIIVEZbDPvTvZ:matrix.example.com"
# room_id = "#ops:matrix.example.com"

# Use ["*"] during initial verification, then tighten.
allowed_users = ["*"]
```

### About `user_id` and `device_id`

- ZeroClaw attempts to read identity from Matrix `/_matrix/client/v3/account/whoami`.
- If `whoami` does not return `device_id`, set `device_id` manually.
- These hints are especially important for E2EE session restore.

---

## 3. Quick Validation Flow

1. Run channel setup and daemon:

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

2. Send a plain text message in the configured Matrix room.

3. Confirm ZeroClaw logs contain Matrix listener startup and no repeated sync/auth errors.

4. In an encrypted room, verify the bot can read and reply to encrypted messages from allowed users.

---

## 4. Troubleshooting “No Response”

Use this checklist in order.

### A. Room and membership

- Ensure the bot account has joined the room.
- If using alias (`#...`), verify it resolves to the expected canonical room.

### B. Sender allowlist

- If `allowed_users = []`, all inbound messages are denied.
- For diagnosis, temporarily set `allowed_users = ["*"]`.

### C. Token and identity

- Validate token with:

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://matrix.example.com/_matrix/client/v3/account/whoami"
```

- Check that returned `user_id` matches the bot account.
- If `device_id` is missing, set `channels_config.matrix.device_id` manually.

### D. E2EE-specific checks

- The bot device must receive room keys from trusted devices.
- If keys are not shared to this device, encrypted events cannot be decrypted.
- Verify device trust and key sharing in your Matrix client/admin workflow.
- If logs show `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`, key backup recovery is not enabled on this device yet. This warning is usually non-fatal for live message flow, but you should still complete key backup/recovery setup.
- If recipients see bot messages as "unverified", verify/sign the bot device from a trusted Matrix session and keep `channels_config.matrix.device_id` stable across restarts.

### E. Message formatting (Markdown)

- ZeroClaw sends Matrix text replies as markdown-capable `m.room.message` text content.
- Matrix clients that support `formatted_body` should render emphasis, lists, and code blocks.
- If formatting appears as plain text, check client capability first, then confirm ZeroClaw is running a build that includes markdown-enabled Matrix output.

### F. Fresh start test

After updating config, restart daemon and send a new message (not just old timeline history).

---

## 5. Operational Notes

- Keep Matrix tokens out of logs and screenshots.
- Start with permissive `allowed_users`, then tighten to explicit user IDs.
- Prefer canonical room IDs in production to avoid alias drift.

---

## 6. Related Docs

- [Channels Reference](./channels-reference.md)
- [Operations log keyword appendix](./channels-reference.md#7-operations-appendix-log-keywords-matrix)
- [Network Deployment](./network-deployment.md)
- [Agnostic Security](./agnostic-security.md)
- [Reviewer Playbook](./reviewer-playbook.md)
