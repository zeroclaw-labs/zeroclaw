# Matrix

Run ZeroClaw in Matrix rooms, including end-to-end encrypted (E2EE) rooms.

Common failure mode this guide targets:

> "Matrix is configured correctly, checks pass, but the bot does not respond."

## Fast FAQ

If Matrix appears connected but there's no reply, validate these first:

1. Sender is allowed by `allowed_users` (for testing: `["*"]`).
2. Bot account has joined the exact target room.
3. Token belongs to the same bot account (`whoami` check — see §4C).
4. Encrypted room has usable device identity (`device_id`) and key sharing.
5. Daemon was restarted after config changes.

## 1. Requirements

Before testing message flow:

1. The bot account is joined to the target room.
2. The access token belongs to the same bot account.
3. `room_id` is correct:
   - preferred: canonical room ID (`!room:server`)
   - supported: room alias (`#alias:server`) — ZeroClaw resolves it
4. `allowed_users` allows the sender (`["*"]` for open testing).
5. For E2EE rooms, the bot device has received encryption keys for the room.

## 2. Configuration

All config management goes through `zeroclaw config` or `zeroclaw onboard`. Do not hand-edit `~/.zeroclaw/config.toml`.

Easiest: run the wizard and let it prompt for every Matrix field:

```bash
zeroclaw onboard --channels-only
```

Or set individual fields after onboarding:

```bash
zeroclaw config set channels.matrix.homeserver https://matrix.example.com
zeroclaw config set channels.matrix.room-id '!room:matrix.example.com'
zeroclaw config set channels.matrix.access-token           # prompts, input masked
zeroclaw config set channels.matrix.user-id @bot:matrix.example.com
zeroclaw config set channels.matrix.device-id ABCDEF1234
zeroclaw config set channels.matrix.allowed-users '["*"]'   # open for testing
```

Required: `homeserver`, `access-token`, `room-id`. Strongly recommended for E2EE: `user-id` and `device-id` (see §4H for how to obtain them). For the full field index, see the [Config reference](../reference/config.md).

### About `user-id` and `device-id`

- ZeroClaw attempts to read identity from Matrix `/_matrix/client/v3/account/whoami`.
- If `whoami` doesn't return `device_id`, set `device-id` manually — critical for E2EE session restore.

## 3. Quick validation

```bash
zeroclaw onboard --channels-only
zeroclaw service restart        # or `zeroclaw daemon` to run foreground
```

Send a plain-text message in the configured Matrix room. Confirm:

- ZeroClaw logs show the Matrix listener starting with no repeated sync/auth errors.
- In an encrypted room, the bot can read and reply to encrypted messages from allowed users.

## 4. Troubleshooting "no response"

Work through in order.

### A. Room and membership

- Confirm the bot account has joined the room.
- If using an alias (`#...`), verify it resolves to the expected canonical room.

### B. Sender allowlist

- If `allowed_users = []`, all inbound messages are denied.
- For diagnosis, temporarily open it:
  ```bash
  zeroclaw config set channels.matrix.allowed-users '["*"]'
  zeroclaw service restart
  ```
- Tighten to explicit user IDs once the flow works.

### C. Token and identity

> **About `$MATRIX_TOKEN` in the snippets below.** Secrets in ZeroClaw are encrypted at rest and intentionally **not** retrievable via `zeroclaw config get` — it prints `[masked]` for any secret field. You have two options:
>
> 1. **Get a fresh token** from the password-login curl command in §4H Option 2. Export the `access_token` it returns. Good for validation and recovery paths — doesn't affect what's in your config.
> 2. **Keep a copy** of the token when you first paste it into `zeroclaw onboard` or `zeroclaw config set channels.matrix.access-token`. A one-time side-effect — write it to a scratch note if you want to run these curl checks later.
>
> The non-secret fields *are* retrievable:
>
> ```bash
> MATRIX_HOMESERVER=$(zeroclaw config get channels.matrix.homeserver)
> MATRIX_USER=$(zeroclaw config get channels.matrix.user-id)
> ```

With `MATRIX_TOKEN` set, validate the token server-side:

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "$MATRIX_HOMESERVER/_matrix/client/v3/account/whoami"
```

- Returned `user_id` must match the bot account.
- If `device_id` is missing from the response, set it manually (see §H).
- Rotate the access token without re-running onboard:
  ```bash
  zeroclaw config set channels.matrix.access-token    # prompts, masked
  zeroclaw service restart
  ```

### D. E2EE-specific checks

- The bot device must have received room keys from trusted devices.
- If keys haven't been shared to this device, encrypted events cannot be decrypted.
- Verify device trust and key sharing from a trusted Matrix session.
- `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found` — key backup recovery isn't enabled on this device yet. Non-fatal for message flow; still worth completing (see §I).
- If recipients see bot messages as "unverified", verify/sign the bot device from a trusted Matrix session and keep `device-id` stable across restarts.

### E. Log levels

ZeroClaw suppresses `matrix_sdk`, `matrix_sdk_base`, and `matrix_sdk_crypto` to `warn` by default — they're noisy at `info`. Restore SDK output for debugging:

```bash
RUST_LOG=info,matrix_sdk=info,matrix_sdk_base=info,matrix_sdk_crypto=info zeroclaw daemon
```

### F. Message formatting (Markdown)

- ZeroClaw sends Matrix replies as markdown-capable `m.room.message` text content.
- Matrix clients that support `formatted_body` render emphasis, lists, and code blocks.
- If formatting appears as plain text: check client capability first, then confirm ZeroClaw is running a build with markdown-enabled Matrix output.

### G. Fresh start test

After config changes, restart the daemon and send a new message. Old timeline history won't be replayed.

### H. Finding your `device_id`

ZeroClaw needs a stable `device_id` for E2EE session restore. Without it, a new device is registered every restart, breaking key sharing and device verification.

#### Option 1 — `whoami` (easiest)

```bash
curl -sS -H "Authorization: Bearer $MATRIX_TOKEN" \
  "https://your.homeserver/_matrix/client/v3/account/whoami"
```

Response includes `device_id` if the token is bound to a device session:

```json
{"user_id": "@bot:example.com", "device_id": "ABCDEF1234"}
```

If `device_id` is missing, the token was created without a device login (e.g. via the admin API). Use Option 2.

#### Option 2 — Password login (fresh device)

```bash
curl -sS -X POST "https://matrix.org/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"YOUR_BOT_USERNAME"},"password":"YOUR_PASSWORD","device_id":"NEW_DEVICE_ID"}'
```

Response:

```json
{"user_id": "@bot:example.com", "access_token": "syt_...", "device_id": "NEWDEVICE"}
```

Apply both values:

```bash
zeroclaw config set channels.matrix.access-token    # paste the new access_token (masked)
zeroclaw config set channels.matrix.device-id NEWDEVICE
zeroclaw config set channels.matrix.user-id @bot:example.com
zeroclaw service restart
```

#### Option 3 — From Element or another Matrix client

1. Log in as the bot account in Element.
2. Settings → Sessions.
3. Copy the Device ID for the active session.
4. Apply:

```bash
zeroclaw config set channels.matrix.device-id ABCDEF1234
zeroclaw service restart
```

Keep `device-id` stable — changing it forces a new device registration, which breaks existing key sharing and verification.

### H (continued). Crypto-store deletion recovery

**Symptom:** `Matrix one-time key upload conflict detected; stopping sync to avoid infinite retry loop` and the channel becomes unavailable.

**Cause:** The local crypto store was deleted while the old device still had one-time keys registered on the homeserver. The SDK can't upload new keys because the old keys still exist server-side, causing an infinite OTK conflict loop.

#### Fix — fresh login

A fresh login creates a new device with a new `device_id`, sidestepping the OTK conflict entirely (no UIA-gated device deletion required).

1. Stop ZeroClaw.

   ```bash
   zeroclaw service stop
   ```

2. Get a fresh access token and `device_id`:

   ```bash
   curl -sS -X POST "https://matrix.org/_matrix/client/v3/login" \
     -H "Content-Type: application/json" \
     -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"YOUR_BOT_USERNAME"},"password":"YOUR_PASSWORD","device_id":"NEW_DEVICE_ID"}'
   ```

   Save the returned `access_token` and `device_id`.

3. Delete the local crypto store:

   ```bash
   rm -rf ~/.zeroclaw/state/matrix/
   ```

4. Apply the new credentials:

   ```bash
   zeroclaw config set channels.matrix.access-token <new_token>
   zeroclaw config set channels.matrix.device-id <new_device_id>
   ```

5. Restart:

   ```bash
   zeroclaw service start
   ```

#### What to expect on first restart

- `Our own device might have been deleted` — harmless; old device is gone.
- `Failed to decrypt a room event` — old messages from before the reset; unrecoverable.
- `Matrix E2EE recovery successful` — room keys restored from server backup (only if `recovery_key` is set; see §I).
- New messages decrypt and work normally.

**Prevention:** Don't delete the local state directory without planning a fresh login. If you need a fresh start, get new credentials first, then delete the store, then update config.

### I. Recovery key (recommended for E2EE)

A recovery key lets ZeroClaw automatically restore room keys and cross-signing secrets from server-side backup. Device resets, crypto-store deletions, and fresh installs all recover automatically — no emoji verification, no manual key sharing.

#### Step 1 — Get your recovery key from Element

1. Log into the bot account in Element (web or desktop).
2. Settings → Security & Privacy → Encryption → Secure Backup.
3. If backup is already set up, your recovery key was shown when you first enabled it. If you saved it, use that.
4. If backup isn't set up, click "Set up Secure Backup" → "Generate a Security Key". Save the key — it looks like `EsTj 3yST y93F SLpB ...`.
5. Log out of Element.

#### Step 2 — Add the recovery key to ZeroClaw

Either path works. The onboarding wizard is easier for fresh installs; `zeroclaw config set` is preferred for existing installs.

**Option A — during onboarding:**

```bash
zeroclaw onboard --channels-only
```

When prompted:

```
E2EE recovery key (or Enter to skip): EsTj 3yST y93F SLpB jJsz ...
```

Input is masked. The key is encrypted at rest.

**Option B — existing installs:**

```bash
zeroclaw config set channels.matrix.recovery-key    # input masked
zeroclaw service restart
```

Encrypted at rest immediately.

#### Step 3 — Restart

```bash
zeroclaw service restart
```

On startup you should see:

```
Matrix E2EE recovery successful — room keys and cross-signing secrets restored from server backup.
```

From now on, even if the local crypto store is deleted, ZeroClaw recovers automatically on next startup.

## 5. Debug logging

Matrix-channel-specific diagnostics:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug zeroclaw daemon
```

Surfaces:

- Session restore confirmation
- Each sync cycle completion
- OTK conflict flag state
- Health check results
- Transient vs. fatal sync error classification

For SDK-level detail as well:

```bash
RUST_LOG=zeroclaw::channels::matrix=debug,matrix_sdk_crypto=debug zeroclaw daemon
```

## 6. Operational notes

- Keep Matrix tokens out of logs and screenshots.
- Start with permissive `allowed_users`, tighten to explicit user IDs once verified.
- Prefer canonical room IDs in production to avoid alias drift.
- **Threading:** ZeroClaw always replies in a thread rooted at the user's original message. Each thread maintains its own isolated conversation context. The main room timeline is unaffected — threads don't share context with each other or with the room. In encrypted rooms, threading works identically; the SDK decrypts events transparently before thread context is evaluated.

## See also

- [Network deployment](../ops/network-deployment.md)
- [Config reference](../reference/config.md) — generated from the live schema
- [Channels overview](./overview.md)
