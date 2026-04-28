# Email

Two email channels depending on how you want inbound messages delivered.

## IMAP + SMTP (`email_channel`)

The general-purpose email channel. Polls IMAP for new messages, sends via SMTP. Works with Gmail, Outlook, Fastmail, self-hosted Postfix, and anything else that speaks IMAP/SMTP.

```toml
[channels.email]
enabled = true
allowed_senders = []
default_subject = "ZeroClaw Message"
from_address = "demo@gmail.com"
idle_timeout_secs = 1740
imap_folder = "INBOX" # optional foldier to use
imap_host = "imap.gmail.com"
imap_port = 993
imap_tls = true
max_attachment_bytes = 26214400
password = "enc2:cdf60071892ba88c753338db62c592fa7b54426ad8ed0e0bfceb403c5ddb506f"
smtp_host = "smtp.gmail.com"
smtp_port = 465
smtp_tls = true
username = "demo"
subject_prefix = "[agent]"       # only respond to subjects starting with this
poll_interval_secs = 60 # if the server doesn't support IDLE mode, how frequently poll the server for updates
reply_format = "text"            # or "html"
include_thread = true            # include conversation thread
```

### General tips

- If your server doesn't support SSL/TLS, make sure to set both `imap_tls = false` and `smtp_tls = false`.
- Concequentially, if your server uses SSL/TLS, make sure to set both `imap_tls = true` and `smtp_tls = true`

### Gmail gotchas

- **App passwords required** if 2FA is on. Regular account password is rejected.
- **"Less secure app access" is gone** — app password is the only path.
- Consider the Gmail Push channel below for real-time delivery instead of polling.

### Outlook / Office 365

OAuth 2.0 is recommended over password auth:

```toml
[channels.email.imap]
host = "outlook.office365.com"
port = 993
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
