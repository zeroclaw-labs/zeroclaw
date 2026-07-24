# Matrix

Run ZeroClaw in Matrix rooms, including end-to-end encrypted (E2EE) rooms.

## Who can talk to the agent

{{#peer-group matrix}}

Common failure mode this guide targets:

> "Matrix is configured correctly, checks pass, but the bot does not respond."

## Fast FAQ

If Matrix appears connected but there's no reply, validate these first:

1. Sender is in the agent's peer set (for testing: `external_peers = ["*"]`).
2. Bot account has joined the exact target room.
3. Credentials belong to the bot account (`whoami` check on the token path, see [§5C](#c-token-and-identity)).
4. Encrypted room can be decrypted: `recovery_key` set (recommended) or keys shared to the bot device.
5. Daemon was restarted after config changes.

## 1. Requirements

Before testing message flow:

1. The bot account is joined to the target room.
2. Credentials authenticate the bot account: either `user_id` + `password` (recommended, see [§2](#2-configuration)) or an `access_token` (token path, [§3](#3-token-path-alternative-obtaining-access_token-and-device_id)).
3. `allowed_rooms` includes the target room (or is empty to allow all rooms the bot has joined). Entries are matched literally against the canonical room ID (`!room:server`) of each incoming message, so list canonical room IDs here: ZeroClaw does **not** resolve a `#alias:server` entry for this allowlist. (Aliases are resolved only for outbound delivery targets such as cron `delivery.to`.) Find a room's canonical ID in its client (in Element: Room settings → Advanced → Internal room ID).
4. A peer group authorizes the sender (`external_peers = ["*"]` for open testing, see [§6](#b-sender-allowlist-peer-groups)).
5. For E2EE rooms, the bot can decrypt: a `recovery_key` (recommended) restores keys automatically, or keys are shared to the bot device manually.

## 2. Configuration

{{#config-fields channels.matrix}}

Matrix is configured as a `[channels.matrix.<alias>]` block. Set it through any of these surfaces:

{{#config-where channels matrix}}

### Recommended setup: password + recovery key

The official, lowest-friction way to run Matrix is to let ZeroClaw log in
fresh and manage its own device identity:

- **Omit `device_id`.** Let the homeserver assign one at login. ZeroClaw
  saves the assigned id to `session.json` and reuses it on every restart, so
  there is no value for you to look up, copy, or keep in sync. Pinning a
  `device_id` by hand is the single most common source of broken key sharing.
- **Omit `access_token`.** When it is unset, ZeroClaw falls back to password
  login. A fresh login is also what the auto-recovery path ([§8](#8-auto-recovery-from-corrupted-local-state)) uses, so the
  bot self-heals from corrupted local state without operator action.
- **Set `password`.** With `access_token` absent, `user_id` + `password`
  perform the login.
- **Set `recovery_key`.** This restores room keys from server-side backup and
  cross-signs the freshly registered device automatically on every startup:
  no emoji verification, no manual key sharing, no bootstrap. See [§5I](#i-recovery-key-recommended-for-e2ee) for how
  to get it from Element.

So a complete recommended block sets `homeserver`, `user_id`, `password`, and
`recovery_key`, and leaves `access_token` and `device_id` unset.

The `access_token` + `device_id` path ([§3](#3-token-path-alternative-obtaining-access_token-and-device_id)) still works and is documented in
full for operators who must reuse a pre-existing token, but it requires you to
keep a stable `device_id` yourself, so prefer password + recovery key unless
you have a specific reason not to.

{{#secret-config channels.matrix.<alias>.password}}

{{#secret-config channels.matrix.<alias>.access_token}}

`homeserver` is required. For the recommended setup, also set `user_id`,
`password`, and `recovery_key`. `access_token` and `device_id` are only needed
for the token-based path in [§3](#3-token-path-alternative-obtaining-access_token-and-device_id); `allowed_rooms` optionally restricts which
rooms the bot answers in. Authorize senders with a [peer group](#who-can-talk-to-the-agent). Full field index: [config reference](../reference/config.md#channels).

> **Don't have a `recovery_key` yet?** See [§5I](#i-recovery-key-recommended-for-e2ee): it walks through generating one
> in Element. Going the token route instead? See [§3](#3-token-path-alternative-obtaining-access_token-and-device_id) for the password-login API
> call that mints an `access_token` plus a stable `device_id` in one shot. To
> look up `device_id` for a token you already have, see [§5H](#h-finding-device_id-for-an-existing-token).

### About `user_id` and `device_id`

- For the recommended password + recovery-key setup, set `user_id` and leave
  `device_id` unset: the homeserver assigns and ZeroClaw persists it.
- ZeroClaw reads identity from Matrix `/_matrix/client/v3/account/whoami`.
- Only on the `access_token` path do you set `device_id` manually: a token
  login carries a device the server already minted, and ZeroClaw needs that
  exact id for E2EE session restore (see [§5H](#h-finding-device_id-for-an-existing-token) to find it).

### Threads and context

{{#thread-context channel="Matrix" prop="reply_in_thread" path="channels.matrix.<alias>.reply_in_thread"}}

## 3. Token path (alternative): obtaining `access_token` and `device_id`

> [!IMPORTANT]
> This section is for the `access_token` path only. If you followed the
> recommended password + recovery-key setup in [§2](#2-configuration), you can skip it: you do not
> need an access token or a hand-managed `device_id`.

Use this path when you must reuse a pre-existing token (for example one copied
from another deployment). Element doesn't expose the token directly, so the
canonical way to mint one is a one-shot password-login API call that returns
both the access token and a stable device ID together. The token login carries
a device, so on this path `device_id` is required and must stay stable.

If your operator account already has a token, skip to [§4](#4-quick-validation). If you only need to look up the `device_id` for an existing token, see [§5H](#h-finding-device_id-for-an-existing-token) Option 1 (`whoami`) or Option 2 (Element).

### Step 1: Mint a token via password login

Run this once. Replace `your.homeserver`, the bot username, password, and pick any short `device_id` string (alphanumeric, no spaces; this is the *server-side* device label that ZeroClaw will reuse on every restart):

<div class="os-tabs-src">

#### sh

```sh
curl -sS -X POST "https://your.homeserver/_matrix/client/v3/login" \
  -H "Content-Type: application/json" \
  -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"YOUR_BOT_USERNAME"},"password":"YOUR_PASSWORD","device_id":"NEW_DEVICE_ID"}'
```

</div>

Response:

```json
{"user_id": "@bot:example.com", "access_token": "syt_...", "device_id": "NEWDEVICE"}
```

### Step 2: Apply both values to ZeroClaw

Put `access_token`, `device_id`, and `user_id` from the response into your `[channels.matrix.<alias>]` block (see [§2](#2-configuration) for where to set them), then restart: `zeroclaw service restart`.

### Notes

- **Keep a copy of the token** when you first paste it. Secrets are encrypted at rest and `zeroclaw config get` will print `[masked]` for the token field; you can't retrieve it later. Stash it in a scratch note if you'll need it for the curl validation snippets in [§5C](#c-token-and-identity).
- **Reuse the same `device_id` on every restart**: changing it forces a new server-side device registration, which breaks key sharing and verification in encrypted rooms. The auto-recovery path in [§8](#8-auto-recovery-from-corrupted-local-state) handles the rare cases where wiping is genuinely the right call.
- **Rotating the access token later** without re-running the wizard: update the `access_token` field in your config (see [§2](#2-configuration)), then `zeroclaw service restart`.
- **Token shows as expired or invalid** at startup: mint a new one with the same curl, repeat Step 2.

## 4. Quick validation

Apply the field set in [§2](#2-configuration) if you haven't yet, then restart with `zeroclaw service restart` (background) or `zeroclaw daemon` (foreground). Send a plain-text message in the configured Matrix room. Confirm:

- ZeroClaw logs show the Matrix listener starting with no repeated sync/auth errors.
- In an encrypted room, the bot can read and reply to encrypted messages from allowed users.

## 5. Troubleshooting "no response"

Work through in order.

### A. Room and membership

- Confirm the bot account has joined the room.
- If you put a room in `allowed_rooms`, it must be the **canonical** room ID (`!room:server`), not a `#alias:server`. Aliases are not resolved for the allowlist, so an alias entry silently matches nothing. Find the canonical ID in Element via Room settings → Advanced → Internal room ID.

### B. Sender allowlist (peer groups)

The sender must be in the agent's peer set, see [Who can talk to the agent](#who-can-talk-to-the-agent) at the top of this page. For diagnosis, temporarily set `external_peers = ["*"]` and restart the daemon.

### C. Token and identity

Secrets are encrypted at rest and not retrievable: `zeroclaw config get` prints `[masked]` for any secret field. To run the checks below, use the access token you minted in [§3](#3-token-path-alternative-obtaining-access_token-and-device_id) (or mint a fresh one) and your own homeserver URL.

Validate the token server-side:

<div class="os-tabs-src">

#### sh

```sh
curl -sS -H "Authorization: Bearer <access_token>" \
  "https://your.homeserver/_matrix/client/v3/account/whoami"
```

</div>

- Returned `user_id` must match the bot account.
- If `device_id` is missing from the response, set it manually (see [§5H](#h-finding-device_id-for-an-existing-token)).
- Rotate the access token: update the `access_token` field in your config (see [§2](#2-configuration)), then `zeroclaw service restart`.

### D. E2EE-specific checks

- The bot device must have received room keys from trusted devices.
- If keys haven't been shared to this device, encrypted events cannot be decrypted.
- Verify device trust and key sharing from a trusted Matrix session.
- `matrix_sdk_crypto::backups: Trying to backup room keys but no backup key was found`: key backup recovery isn't enabled on this device yet. Non-fatal for message flow; still worth completing (see [§5I](#i-recovery-key-recommended-for-e2ee)).
- If recipients see bot messages as "unverified", verify/sign the bot device from a trusted Matrix session and keep `device_id` stable across restarts.

### E. Log levels

ZeroClaw suppresses `matrix_sdk`, `matrix_sdk_base`, and `matrix_sdk_crypto` to `warn` by default; they're noisy at `info`. Restore SDK output for debugging:

<div class="os-tabs-src">

#### sh

```sh
RUST_LOG=info,matrix_sdk=info,matrix_sdk_base=info,matrix_sdk_crypto=info zeroclaw daemon
```

</div>

### F. Message formatting (Markdown)

- ZeroClaw sends Matrix replies as markdown-capable `m.room.message` text content.
- Matrix clients that support `formatted_body` render emphasis, lists, and code blocks.
- If formatting appears as plain text: check client capability first, then confirm ZeroClaw is running a build with markdown-enabled Matrix output.

### G. Fresh start test

After config changes, restart the daemon and send a new message. Old timeline history won't be replayed.

### H. Finding `device_id` for an existing token

You only need this on the `access_token` path ([§3](#3-token-path-alternative-obtaining-access_token-and-device_id)). The recommended password +
recovery-key setup omits `device_id` entirely: the homeserver assigns one and
ZeroClaw persists it, so there is nothing to look up. If you have switched to
the recommended setup, skip this section.

If you really must pin a `device_id` (because you are reusing an existing
access token rather than logging in with a password), use this to find the one
bound to that token. For brand-new bots on the token path, see [§3](#3-token-path-alternative-obtaining-access_token-and-device_id): the
password-login flow there returns both values together.

ZeroClaw needs a stable `device_id` for E2EE session restore on the token path. Without it, a new device is registered every restart, breaking key sharing and device verification.

#### Option 1: `whoami` (easiest)

<div class="os-tabs-src">

#### sh

```sh
curl -sS -H "Authorization: Bearer <access_token>" \
  "https://your.homeserver/_matrix/client/v3/account/whoami"
```

</div>

Response includes `device_id` if the token is bound to a device session:

```json
{"user_id": "@bot:example.com", "device_id": "ABCDEF1234"}
```

If `device_id` is missing, the token was created without a device login (e.g. via the admin API). Mint a new token + device_id together via [§3](#3-token-path-alternative-obtaining-access_token-and-device_id).

#### Option 2: From Element or another Matrix client

1. Log in as the bot account in Element.
2. Settings → Sessions.
3. Copy the Device ID for the active session.
4. Set `device_id` in your config (see [§2](#2-configuration)), then `zeroclaw service restart`. Keep `device_id` stable: changing it forces a new device registration, which breaks existing key sharing and verification.

### H (continued). Crypto-store deletion recovery

**Symptom:** `Matrix one-time key upload conflict detected; stopping sync to avoid infinite retry loop` and the channel becomes unavailable.

**Cause:** The local crypto store was deleted while the old device still had one-time keys registered on the homeserver. The SDK can't upload new keys because the old keys still exist server-side, causing an infinite OTK conflict loop.

#### Fix: fresh login

A fresh login creates a new device with a new `device_id`, sidestepping the OTK conflict entirely (no UIA-gated device deletion required).

1. Stop ZeroClaw.

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw service stop
   ```

   </div>

2. Get a fresh access token and `device_id`:

   <div class="os-tabs-src">

   #### sh

   ```sh
   curl -sS -X POST "https://matrix.org/_matrix/client/v3/login" \
     -H "Content-Type: application/json" \
     -d '{"type":"m.login.password","identifier":{"type":"m.id.user","user":"YOUR_BOT_USERNAME"},"password":"YOUR_PASSWORD","device_id":"NEW_DEVICE_ID"}'
   ```

   </div>

   Save the returned `access_token` and `device_id`.

3. Delete the local crypto store:

   <div class="os-tabs-src">

   #### sh

   ```sh
   rm -rf ~/.zeroclaw/state/matrix/
   ```

   </div>

4. Apply the new credentials: set `access_token` (secret, see [§2](#2-configuration)) and `device_id` in your config.

5. Restart:

   <div class="os-tabs-src">

   #### sh

   ```sh
   zeroclaw service start
   ```

   </div>

#### What to expect on first restart

- `Our own device might have been deleted`: harmless; old device is gone.
- `Failed to decrypt a room event`: old messages from before the reset; unrecoverable.
- `Matrix E2EE recovery successful`: room keys restored from server backup (only if `recovery_key` is set; see [§5I](#i-recovery-key-recommended-for-e2ee)).
- New messages decrypt and work normally.

**Prevention:** Don't delete the local state directory without planning a fresh login. If you need a fresh start, get new credentials first, then delete the store, then update config.

### I. Recovery key (recommended for E2EE)

A recovery key lets ZeroClaw automatically restore room keys and cross-signing secrets from server-side backup. Device resets, crypto-store deletions, and fresh installs all recover automatically: no emoji verification, no manual key sharing.

#### Step 1: Get your recovery key from Element

1. Log into the bot account in Element (web or desktop).
2. Settings → Security & Privacy → Encryption → Secure Backup.
3. If backup is already set up, your recovery key was shown when you first enabled it. If you saved it, use that.
4. If backup isn't set up, click "Set up Secure Backup" → "Generate a Security Key". Element shows the key (it looks like `EsTj 3yST y93F SLpB ...`); copy it somewhere safe.
5. Continue past the key display: Element then asks you to **re-enter the key** in a confirmation box to prove you saved it. Paste it and continue to finish setup. This is the same value you put in `recovery_key`.
6. (Optional) Log out of the bot's Element session once the key is saved: click the account menu → **All settings** → Account, then **Remove this device**. Leaving it logged in is fine; removing it just keeps the device list tidy.

#### Step 2: Add the recovery key to ZeroClaw

Apply the recovery key to ZeroClaw:

{{#secret-config channels.matrix.<alias>.recovery_key}}

Then `zeroclaw service restart`. The recovery key is encrypted at rest immediately.

#### Step 3: Restart

<div class="os-tabs-src">

#### sh

```sh
zeroclaw service restart
```

</div>

On startup you should see:

```
Matrix E2EE recovery successful — room keys and cross-signing secrets restored from server backup.
```

From now on, even if the local crypto store is deleted, ZeroClaw recovers automatically on next startup.

## 6. Debug logging

Matrix-channel-specific diagnostics:

<div class="os-tabs-src">

#### sh

```sh
RUST_LOG=zeroclaw::channels::matrix=debug zeroclaw daemon
```

</div>

Surfaces:

- Session restore confirmation
- Each sync cycle completion
- OTK conflict flag state
- Health check results
- Transient vs. fatal sync error classification

For SDK-level detail as well:

<div class="os-tabs-src">

#### sh

```sh
RUST_LOG=zeroclaw::channels::matrix=debug,matrix_sdk_crypto=debug zeroclaw daemon
```

</div>

## 7. Operational notes

- Keep Matrix tokens out of logs and screenshots.
- Start with permissive `external_peers = ["*"]`, tighten to explicit user IDs once verified.
- Always use canonical room IDs in `allowed_rooms`: aliases are not resolved for the inbound allowlist (they are resolved only for outbound `delivery.to`).
- **Threading:** when `channels.matrix.reply_in_thread` is `true` (default), every bot reply lives in a thread rooted at the user's message. Top-level user messages open a fresh thread; existing threads are continued. The main room timeline only carries the user-initiated messages.
- **Thread root context:** the first inbound message ZeroClaw sees in any given thread is prefixed with `[Thread root from @sender]: <root body>` so the agent has the conversation that triggered the reply. Threads the bot itself started skip the preamble. Tracking is in-memory only; after a daemon restart, the next message in each active thread re-injects the preamble exactly once.
- **Inline-reply media:** `channels.matrix.mention_only = true` makes the bot ignore naked media uploads (no text body to mention against). When the user inline-replies to such a dropped event with a question (`@bot can you see this?`), ZeroClaw walks the reply's `m.relates_to.m.in_reply_to.event_id`, fetches the parent event, and pulls its media into the current message: the agent's vision pipeline sees the image even though the original upload was filtered out.
- **Attachments thread alongside text:** `room.send_attachment` calls carry an `AttachmentConfig::reply(...)` with `EnforceThread::Threaded` when a thread anchor is present, so PDFs / images / voice notes land inside the bot's thread instead of the main timeline.
- **Outbound media markers:** the agent emits `[image:url|path]`, `[file:url|path]`, `[voice:url|path]`, `[video:...]`, `[audio:...]` (and uppercase / `[document:...]` aliases) inside its reply text; ZeroClaw fetches the bytes (HTTP for `http(s)://`, local read otherwise) and uploads as the appropriate Matrix message event. **Missing or unreadable targets are non-fatal:** the channel logs a warning, drops just that marker, and appends a `(note: I couldn't deliver the file at <path>.)` line so the operator sees what was attempted instead of a silently-dropped reply.
- **Voice messages** (MSC3245): inbound `m.audio` events carrying the `org.matrix.msc3245.voice` field are saved to `{workspace_dir}/matrix_files/` and run through the agent's configured transcription provider so the agent gets both the transcript text and the source path. Outbound voice notes use the `[voice:<url|path>]` marker; ZeroClaw uploads as `m.audio` with the voice flag + zero-waveform set so Element renders the bubble as a voice note. See [Model Providers](../providers/overview.md) for transcription provider setup.
- **Acknowledgement reactions:** controlled by `channels.matrix.ack_reactions` (default `true`). When on, the bot reacts with 👀 while processing and ✅ when done. Set to `false` to keep rooms reaction-free.
- **Persistent sessions:** on first successful login, ZeroClaw writes `~/.zeroclaw/state/matrix/session.json` (user_id + device_id + access_token + optional refresh_token). Subsequent restarts call `restore_session()` from that blob: no re-login. The matrix-rust-sdk SQLite crypto store lives alongside it at `~/.zeroclaw/state/matrix/store/`. **Once `session.json` exists, rotating `access_token` in config has no effect until the file is deleted**: the saved token wins. Delete `session.json` to force a re-login from config values.
- **Cross-signing:** when `recovery_key` matches what is sealed in your account's server-side secret storage, ZeroClaw runs `recovery().recover(key)` on every startup, the SDK imports your existing master / self-signing / user-signing keys, and the freshly registered device is automatically signed. **No bootstrap, no UIA, no key rotation.** If your account doesn't yet have cross-signing set up, generate the recovery key in Element (Settings → Security & Privacy → Secure Backup) before configuring `recovery_key`.
- **Cron delivery:** `delivery.to` should be a plain room id (`!abc:server`) or alias (`#room:server`). Older configs that wrote `<sender>||<room>` are tolerated: ZeroClaw extracts the last `!`/`#`-prefixed segment and warns about the malformed value.

### Streaming

{{#streaming channel="Matrix" mode="stream_mode" path="channels.matrix.<alias>.stream_mode"}}

Matrix specifics: in `partial` mode, tool-execution status is shown through the same edit pipeline as answer text. In `single_message` mode, tool/progress status updates are edited into one sliding draft while the final answer is sent as a separate Matrix message; `stream_draft_lines` controls how many progress entries remain visible (`0` keeps all), and `message_max_bytes` caps both the rendered draft body (by dropping older progress entries) and the separate final response. The limit truncates at a UTF-8 boundary, so a response can lose its tail; choose a budget below the Matrix event limit to leave room for JSON event overhead. `stream_reasoning` controls provider reasoning visibility in that progress draft: `off` suppresses reasoning-derived draft updates, `status` emits liveness ticks without raw reasoning text, and `full` emits raw provider reasoning text into the draft. `stream_draft_delete` controls whether durable progress transcripts are deleted before the final answer is posted; delete failures are logged and final answer delivery still proceeds. Placeholder-only drafts are removed before the final answer even when transcript retention is enabled. In `multi_message` mode each paragraph posts as its own threaded message, and the split is code-fence-aware, so blank lines inside fenced blocks don't break a code block across messages.

`stream_tool_arguments` controls which tool arguments appear in `single_message`
progress lines. Missing or empty configuration uses conservative per-tool
defaults; unknown skill, plugin, and MCP tools show their names only. A single
`default_base` entry selects `none`, `safe`, or `all`, while exact-name tool
rules can replace that base or add and remove fields:

```toml
stream_tool_arguments = [
    { default_base = "safe", argument_chars = 60 },
    { tool = "delegate", base = "none", include = ["agent", "background", "prompt"], argument_chars = 0 },
    { tool = "mock_tool", base = "all", exclude = ["token"] },
]
```

Rule order is irrelevant, duplicate tool/default entries are rejected, and an
omitted rule `base` inherits `default_base`. `include` adds fields after the
base is selected; `exclude` removes them. Runtime-only fields are never shown,
top-level credential-named fields are always redacted, and every selected
value passes credential leak detection and one-line normalization before it
reaches Matrix. Composite values are intentionally not recursively inspected:
including one is an explicit operator disclosure decision.
In `safe` mode, only the recommended top-level scalar arguments are rendered;
nulls, arrays, and objects are omitted. Selecting `all` or naming an argument
in `include` is explicit operator opt-in to compact JSON rendering of a
composite value.
`argument_chars` on the default entry changes the inherited per-value cap from
`60`; the same field on a tool rule overrides it for that tool. `0` keeps full
values while `message_max_bytes` still bounds the rendered draft. Explicit
`all` applies to unknown tools; use an exact-name rule when enabling arguments
for only one extension tool.

## 8. Auto-recovery from corrupted local state

The matrix-rust-sdk default SQLite store is single-device and assumes the local view stays in sync with the homeserver. Two failure modes break that assumption irrecoverably; ZeroClaw detects each at startup and (when `password` + `user_id` are both configured) auto-wipes `~/.zeroclaw/state/matrix/` and re-authenticates so a fresh device is created server-side.

- **Orphan crypto state.** A `store/` directory exists but `session.json` doesn't (manual cleanup, interrupted prior install, etc.). Logging in fresh on top of orphaned crypto state reproduces `Duplicate one-time keys` / `SigningKeyChanged` conflicts that don't self-heal.
- **`StateStoreDataKey::OneTimeKeyAlreadyUploaded` flag set.** The SDK persists this key into the state store the first time it sees a duplicate-OTK upload (per the SDK's own comment: "we forgot about some of our one-time keys. This will lead to UTDs."). It survives restarts; the only fix is wipe and re-register.

**`device_id` drift is detected but tolerated, not wiped.** If `channels.matrix.device_id` differs from the device id stored in `session.json`, the channel logs a warning and honors the saved id (which is the value the homeserver actually assigned at login). Wiping on drift would create a recovery loop because auto-recovery itself generates a new id, leaving config and session permanently out of sync.

When **`recover()` itself fails** (typically `MAC check for the secret storage key failed`), the channel logs the homeserver's default secret-storage key id, whether the key event has passphrase info, the whitespace-stripped input length, and the full error chain: these point at *which* layer rejected the recovery key without leaking the value. Recovery failures are **non-fatal** (they don't trigger auto-wipe); the bot continues, the new device just won't be cross-signed.

If `password` + `user_id` aren't configured, auto-recovery can't run: the channel bails with an actionable error pointing at the two choices: configure them, or `rm -rf ~/.zeroclaw/state/matrix/` manually.

## See also

- [Network deployment](../ops/network-deployment.md)
- [Config reference](../reference/config.md): generated from the live schema
- [Channels overview](./overview.md)
