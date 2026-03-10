# Nextcloud Talk Setup

This guide covers native Nextcloud Talk integration for ZeroClaw.

## 1. What this integration does

- Receives inbound Talk bot webhook events via `POST /nextcloud-talk`.
- Verifies webhook signatures (HMAC-SHA256) when a secret is configured.
- Sends bot replies back to Talk rooms via Nextcloud OCS API.

## 2. Configuration

Add this section in `~/.zeroclaw/config.toml`:

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"
allowed_users = ["*"]
```

Field reference:

- `base_url`: Nextcloud base URL.
- `app_token`: Bot app token used as `Authorization: Bearer <token>` for OCS send API.
- `webhook_secret`: Shared secret for verifying `X-Nextcloud-Talk-Signature`.
- `allowed_users`: Allowed Nextcloud actor IDs (`[]` denies all, `"*"` allows all).

Environment override:

- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` overrides `webhook_secret` when set.

## 3. Gateway endpoint

Run the daemon or gateway and expose the webhook endpoint:

```bash
zeroclaw daemon
# or
zeroclaw gateway --host 127.0.0.1 --port 3000
```

Configure your Nextcloud Talk bot webhook URL to:

- `https://<your-public-url>/nextcloud-talk`

## 4. Signature verification contract

When `webhook_secret` is configured, ZeroClaw verifies:

- header `X-Nextcloud-Talk-Random`
- header `X-Nextcloud-Talk-Signature`

Verification formula:

- `hex(hmac_sha256(secret, random + raw_request_body))`

If verification fails, the gateway returns `401 Unauthorized`.

## 5. Message routing behavior

- ZeroClaw ignores bot-originated webhook events (`actorType = bots`).
- ZeroClaw ignores non-message/system events.
- Reply routing uses the Talk room token from the webhook payload.

## 6. Quick validation checklist

1. Set `allowed_users = ["*"]` for first-time validation.
2. Send a test message in the target Talk room.
3. Confirm ZeroClaw receives and replies in the same room.
4. Tighten `allowed_users` to explicit actor IDs.

## 7. Troubleshooting

- `404 Nextcloud Talk not configured`: missing `[channels_config.nextcloud_talk]`.
- `401 Invalid signature`: mismatch in `webhook_secret`, random header, or raw-body signing.
- No reply but webhook `200`: event filtered (bot/system/non-allowed user/non-message payload).
