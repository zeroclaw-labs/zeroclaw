# Email

Two email channels depending on how you want inbound messages delivered.

## Who can talk to the agent

{{#peer-group email}}

## IMAP + SMTP (`email_channel`)

The general-purpose email channel. Polls IMAP for new messages, sends via SMTP. Works with Gmail, Outlook, Fastmail, self-hosted Postfix, and anything else that speaks IMAP/SMTP.

{{#config-fields channels.email}}

`password` (and `smtp_password` if you use a separate relay) are secrets:

{{#secret-config channels.email.<alias>.password}}

### Gmail gotchas

- **App passwords required** if 2FA is on. Regular account password is rejected.
- **"Less secure app access" is gone**: app password is the only path.
- Consider the Gmail Push channel below for real-time delivery instead of polling.

### Outlook / Office 365

Office 365 accepts SMTP submissions on `smtp.office365.com:587` with STARTTLS only, not on port 465. Set:

```toml
[channels.email.<alias>]
imap_host = "outlook.office365.com"
imap_port = 993
smtp_host = "smtp.office365.com"
smtp_port = 587
smtp_starttls = true
```

With `smtp_starttls = true` the connection starts in plain text and must be upgraded with STARTTLS before any credentials are sent; if the server refuses the upgrade, the send fails. It takes precedence over `smtp_tls`. SMTP signs in with the password (or `smtp_username` / `smtp_password`), so SMTP AUTH must be enabled for the mailbox; the `oauth2` block applies to IMAP only.

`password` (and `smtp_password`) are secrets:

{{#secret-config channels.email.<alias>.password}}

## Gmail Push (`gmail_push`)

Real-time delivery via Google Cloud Pub/Sub, no polling.

{{#config-fields channels.gmail_push}}

`oauth_token` and `webhook_secret` are secrets:

{{#secret-config channels.gmail_push.<alias>.oauth_token}}

### Setup

1. Create a Google Cloud project, enable Gmail API and Pub/Sub API
2. Create a Pub/Sub topic the Gmail service can publish to, set it as `topic`
3. Authorize the agent's Gmail access and store the resulting token via the secret path above
4. The agent watches for new-mail notifications and routes them to the bound agent

Outbound sends still go via SMTP: configure an IMAP+SMTP `[channels.email.<alias>]` block.

---

## Reply threading

Both email channels thread replies using `In-Reply-To` and `References` headers so conversations stay grouped in whatever client the sender uses.

## Outbound body format

Agent replies are sent as `multipart/alternative` with both a plain-text and an HTML part by default. The HTML part is the Markdown-rendered body; the plain-text part is the raw body text. Mail clients that prefer plain text will select the plain-text alternative automatically.

To send plain text only (no HTML part, for clients or setups that prefer it), set the channel's `html_body` field to `false`.

When attachments are present the body alternatives are wrapped in an outer `multipart/mixed`.

## Attachment handling

Inbound attachments are stored under `<workspace>/attachments/<conversation>/`. The agent gets file paths in its context and can read them via the `file_read` tool. Filenames are taken from the `Content-Disposition` header first, falling back to the `Content-Type` `name` parameter.

Outbound attachments are sent from the bytes supplied with each attachment. The Email channel treats the attachment filename as display metadata, not as a local path. Integrations that intentionally send a local file must validate or constrain the path and load it explicitly with `MediaAttachment::from_file` before sending.

## Rate and volume limits

Email isn't optimised for conversational latency. Expect:

- IMAP poll latency: `poll_interval_secs` (default 60 s). Lower at the cost of server load; some providers rate-limit aggressive polling.
- SMTP send: subject to your provider's daily-send quota (Gmail: 500/day for free accounts, 2000/day for Workspace).

## Safety

Email has no auth at the protocol level beyond SMTP's envelope; anyone can claim to be anyone. Gate inbound senders with a peer group (above) before exposing the agent to an inbox that receives public mail.
