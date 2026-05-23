# Email

Two email channels depending on how you want inbound messages delivered.

## IMAP + SMTP (`email_channel`)

The general-purpose email channel. Polls IMAP for new messages, sends via SMTP. Works with Gmail, Outlook, Fastmail, self-hosted Postfix, and anything else that speaks IMAP/SMTP.

```toml
[channels.email]
enabled = true

# IMAP (inbound)
imap_host = "imap.example.com"
imap_port = 993                  # default: 993
imap_folder = "INBOX"            # default: INBOX
poll_interval_secs = 60          # fallback when IDLE not supported

# SMTP (outbound)
smtp_host = "smtp.example.com"
smtp_port = 587                  # default: 465
smtp_tls = true                  # default: true

# Shared credentials (used by both IMAP and SMTP when no smtp_* override is set)
username = "you@example.com"
password = "..."                 # or app-password for Gmail/iCloud

# Optional: use separate credentials for SMTP only (e.g. a relay service)
# smtp_username = "relay-user@sendgrid.net"
# smtp_password = "..."

from_address = "you@example.com"
allowed_senders = ["boss@example.com", "alerts@example.com"]
```

### Gmail gotchas

- **App passwords required** if 2FA is on. Regular account password is rejected.
- **"Less secure app access" is gone** — app password is the only path.
- Consider the Gmail Push channel below for real-time delivery instead of polling.

### Outlook / Office 365

OAuth 2.0 is recommended over password auth:

```toml
[channels.email]
imap_host = "outlook.office365.com"
imap_port = 993
username = "you@example.com"
oauth_token = "..."              # managed via `zeroclaw channel auth email`
```

## Gmail Push (`gmail_push`)

Real-time delivery via Google Cloud Pub/Sub — no polling.

```toml
[channels.gmail_push]
enabled = true
account = "you@gmail.com"
client_secret_json = "~/.zeroclaw/gmail-client-secret.json"
pubsub_topic = "projects/my-project/topics/gmail-inbox"
pubsub_subscription = "projects/my-project/subscriptions/zeroclaw-sub"
allowed_senders = ["boss@example.com"]
```

### Setup

1. Create a Google Cloud project, enable Gmail API and Pub/Sub API
2. Create a Pub/Sub topic the Gmail service can publish to
3. Create a pull subscription on that topic for ZeroClaw
4. Create OAuth client credentials (desktop app type), download JSON
5. On first run, `zeroclaw channel auth gmail-push` opens a browser for the OAuth consent
6. The agent watches the subscription for new-mail notifications

Outbound sends still go via SMTP — configure an `smtp` block in this channel the same way as the IMAP+SMTP channel.

---

## Reply threading

Both email channels thread replies using `In-Reply-To` and `References` headers so conversations stay grouped in whatever client the sender uses.

## Attachment handling

Inbound attachments are stored under `<workspace>/attachments/<conversation>/`. The agent gets file paths in its context and can read them via the `file_read` tool.

Outbound attachments are not supported yet — the agent replies with links to files in the workspace, and the user downloads via whatever tunnel the workspace is exposed through.

## Rate and volume limits

Email isn't optimised for conversational latency. Expect:

- IMAP poll latency: `poll_interval_secs` (default 60 s). Lower at the cost of server load; some providers rate-limit aggressive polling.
- SMTP send: subject to your provider's daily-send quota (Gmail: 500/day for free accounts, 2000/day for Workspace).

## Safety

Email has no auth at the protocol level beyond SMTP's envelope — anyone can claim to be anyone. Always configure `allowed_senders` (strict list of addresses) or `subject_prefix` (shared secret in the subject line) before exposing the agent to an inbox that receives public mail.
