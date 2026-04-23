# Matrix

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
- To update the access token without re-running onboard:
  ```bash
  zeroclaw config set channels.matrix.access-token
  ```

### D. E2EE-specific checks

- The bot device must receive room keys from trusted devices.
- If keys are not shared to this device, encrypted events cannot be decrypted.
- Verify device trust and key sharing in your Matrix client/admin workflow.
- If logs show `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`, key backup recovery is not enabled on this device yet. This warning is usually non-fatal for live message flow, but you should still complete key backup/recovery setup.
- If recipients see bot messages as "unverified", verify/sign the bot device from a trusted Matrix session and keep `channels_config.matrix.device_id` stable across restarts.

### E. Log levels

ZeroClaw suppresses `matrix_sdk`, `matrix_sdk_base`, and `matrix_sdk_crypto` to `warn` by default because they are extremely noisy at `info`. To restore SDK-level output for debugging:

```bash
RUST_LOG=info,matrix_sdk=info,matrix_sdk_base=info,matrix_sdk_crypto=info zeroclaw daemon
```

### F. Message formatting (Markdown)

- ZeroClaw sends Matrix text replies as markdown-capable `m.room.message` text content.
- Matrix clients that support `formatted_body` should render emphasis, lists, and code blocks.
- If formatting appears as plain text, check client capability first, then confirm ZeroClaw is running a build that includes markdown-enabled Matrix output.

### G. Fresh start test

After updating config, restart daemon and send a new message (not just old timeline history).

### H. Finding your `device_id`

ZeroClaw needs a stable `device_id` for E2EE session restore. Without it, a new device is registered on every restart, breaking key sharing and device verification.

#### Option 1: From `whoami` (easiest)

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://your.homeserver/_matrix/client/v3/account/whoami"
```

Response includes `device_id` if the token is bound to a device session:

```json
{"user_id": "@bot:example.com", "device_id": "ABCDEF1234"}
```

If `device_id` is missing, the token was created without a device login (e.g., via admin API). Use Option 2 instead.

#### Option 2: From a password login

```bash
curl -sS -X POST "https://your.homeserver/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{"type": "m.login.password", "user": "@bot:example.com", "password": "...", "initial_device_display_name": "ZeroClaw"}'
```

Response:

```json
{"user_id": "@bot:example.com", "access_token": "syt_...", "device_id": "NEWDEVICE"}
```

Use both the returned `access_token` and `device_id` in your config. This creates a proper device session.

#### Option 3: From Element or another Matrix client

1. Log in as the bot account in Element
2. Go to Settings → Sessions
3. Copy the Device ID for the active session

**Once you have it**, set both in `config.toml`:

```toml
[channels_config.matrix]
user_id = "@bot:example.com"
device_id = "ABCDEF1234"
```

Keep `device_id` stable — changing it forces a new device registration, which breaks existing key sharing and device verification.

### H. One-time key (OTK) upload conflict

**Symptom:** ZeroClaw logs `Matrix one-time key upload conflict detected; stopping sync to avoid infinite retry loop.` and the Matrix channel becomes unavailable.

**Cause:** The bot's local crypto store was reset (e.g., deleted data directory, reinstalled) without deregistering the old device on the homeserver. The homeserver still has old one-time keys for this device, and the SDK fails to upload new ones.

#### Fix

1. Stop ZeroClaw.

2. Deregister the stale device. From a session with admin access to the bot account:

```bash
# List devices
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://your.homeserver/_matrix/client/v3/devices"

# Delete the stale device (requires UIA — interactive auth)
curl -sS -X DELETE -H "Authorization: Bearer $MATRIX_TOKEN" \
  -H "Content-Type: application/json" \
  "https://your.homeserver/_matrix/client/v3/devices/STALE_DEVICE_ID" \
  -d '{"auth": {"type": "m.login.password", "user": "@bot:example.com", "password": "..."}}'
```

3. Delete the local crypto store. The log message includes the store path, typically:

```
~/.zeroclaw/state/matrix/
```

Delete this directory.

4. Re-login to get a fresh `device_id` and `access_token` (see section 4G, Option 2).

5. Update `config.toml` with the new `access_token` and `device_id`.

6. Restart ZeroClaw.

**Prevention:** Do not delete the local state directory without also deregistering the device. If you need a fresh start, always deregister first.

### I. Recovery key (recommended for E2EE)

A recovery key lets ZeroClaw automatically restore room keys and cross-signing secrets from server-side backup. This means device resets, crypto store deletions, and fresh installs recover automatically — no emoji verification, no manual key sharing.

#### Step 1: Get your recovery key from Element

1. Log into the bot account in Element (web or desktop)
2. Go to Settings → Security & Privacy → Encryption → Secure Backup
3. If backup is already set up, your recovery key was shown when you first enabled it. If you saved it, use that.
4. If backup is not set up, click "Set up Secure Backup" and choose "Generate a Security Key". Save the key — it looks like `EsTj 3yST y93F SLpB ...`
5. Log out of Element when done

#### Step 2: Add the recovery key to ZeroClaw

Option A — during onboarding:

```bash
zeroclaw onboard
# or
zeroclaw onboard --channels-only
```

When configuring the Matrix channel, the wizard prompts:

```
E2EE recovery key (or Enter to skip): EsTj 3yST y93F SLpB jJsz ...
```

Paste the recovery key (input is masked). It will be encrypted and stored in `config.toml` as `channels_config.matrix.recovery_key`.

Option B — via the secret CLI (recommended for existing installs):

```bash
zeroclaw config set channels.matrix.recovery-key
```

Input is masked. The value is encrypted at rest immediately.

Option C — edit `config.toml` directly:

```toml
[channels_config.matrix]
recovery_key = "EsTj 3yST y93F SLpB jJsz ..."
```

If `secrets.encrypt = true` (the default), the value will be encrypted on next config save. Note: until a save is triggered, the value remains in plaintext. Using Option A or B is preferred.

#### Step 3: Restart ZeroClaw

On startup you should see:

```
Matrix E2EE recovery successful — room keys and cross-signing secrets restored from server backup.
```

From now on, even if the local crypto store is deleted, ZeroClaw will recover automatically on next startup.

---

## 5. Debug Logging

For detailed E2EE diagnostics, run ZeroClaw with debug-level logging for the Matrix channel:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug zeroclaw daemon
```

This surfaces:
- Session restore confirmation
- Each sync cycle completion
- OTK conflict flag state
- Health check results
- Transient vs. fatal sync error classification

For even more detail from the Matrix SDK itself:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug,matrix_sdk_crypto=debug zeroclaw daemon
```

---

## 6. Operational Notes

- Keep Matrix tokens out of logs and screenshots.
- Start with permissive `allowed_users`, then tighten to explicit user IDs.
- Prefer canonical room IDs in production to avoid alias drift.
- **Threading behavior:** ZeroClaw always replies in a thread rooted at the user's original message. Each thread maintains its own isolated conversation context. The main room timeline is unaffected — threads do not share context with each other or with the room. In encrypted rooms, threading works identically — the SDK decrypts events transparently before thread context is evaluated.

---

## 7. Related Docs

- [Channels Reference](../reference/api/channels-reference.md)
- [Operations log keyword appendix](../reference/api/channels-reference.md#7-operations-appendix-log-keywords-matrix)
- [Network Deployment](../ops/network-deployment.md)
- [Agnostic Security](./agnostic-security.md)
- [Reviewer Playbook](../contributing/reviewer-playbook.md)
