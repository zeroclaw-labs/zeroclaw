# Email

Two email channels depending on how you want inbound messages delivered.

## IMAP + SMTP (`[channels.email]`)

The general-purpose email channel. Watches an IMAP mailbox via the IDLE extension (RFC 2177) and falls back to polling when the server doesn't advertise IDLE; sends via SMTP. Works with Gmail, Outlook, Fastmail, self-hosted Postfix, and anything else that speaks IMAP/SMTP.

```toml
[channels.email]
enabled = true
imap_host = "imap.example.com"
imap_port = 993                                # default 993
imap_folder = "INBOX"                          # default "INBOX"
smtp_host = "smtp.example.com"
smtp_port = 465                                # default 465 (implicit TLS)
smtp_tls = true                                # default true
username = "you@example.com"
password = "..."                               # or app-password for Gmail/iCloud
from_address = "you@example.com"               # the From: header on outbound mail
idle_timeout_secs = 1740                       # default 1740 (29 min, before IDLE re-issue)
poll_interval_secs = 60                        # default 60 (used only when IDLE is unavailable)
allowed_senders = ["boss@example.com", "alerts@example.com"]
default_subject = "ZeroClaw Message"           # default subject when the agent originates a thread
max_attachment_bytes = 26214400                # default 25 MiB
```

Full field reference: [Config](../reference/config.md#channelsemail).

### Gmail gotchas

- **App passwords required** if 2FA is on. Regular account password is rejected.
- **"Less secure app access" is gone** — app password is the only path.
- Consider the Gmail Push channel below for real-time delivery instead of IDLE/polling.

### Outlook / Office 365

Outlook accepts IMAP/SMTP with an app password the same way Gmail does. Set `username` to your full address, `password` to the app password (not your account password), `imap_host = "outlook.office365.com"`, and `smtp_host = "smtp.office365.com"`.

## Gmail Push (`[channels.gmail_push]`)

Real-time delivery via Google Cloud Pub/Sub — Gmail publishes to a topic when new mail arrives, and ZeroClaw is notified through a webhook.

```toml
[channels.gmail_push]
enabled = true
topic = "projects/my-project/topics/gmail-inbox"   # Pub/Sub topic Gmail publishes to
label_filter = ["INBOX"]                           # default ["INBOX"]; restrict by Gmail label
oauth_token = "..."                                # OAuth token authenticating against the Gmail API
allowed_senders = ["boss@example.com"]
webhook_url = "https://your-host/gmail-push"       # public URL Pub/Sub posts notifications to
webhook_secret = "..."                             # shared secret for inbound notification verification
```

Full field reference: [Config](../reference/config.md#channelsgmail_push).

### Setup

1. Create a Google Cloud project, enable the Gmail API and Pub/Sub API.
2. Create a Pub/Sub topic the Gmail service can publish to and grant Gmail's service account `pubsub.publisher` on it.
3. Create OAuth client credentials (desktop app type) and obtain an `oauth_token` for the bot's Gmail account.
4. Expose `webhook_url` publicly (reverse proxy or `[tunnel]`) and configure Pub/Sub to deliver notifications to it. Set `webhook_secret` so ZeroClaw can verify each delivery.
5. Outbound sends still go via SMTP — configure a sibling `[channels.email]` block if you want the agent to be able to reply.

---

## Reply threading

Both email channels thread replies using `In-Reply-To` and `References` headers so conversations stay grouped in whatever client the sender uses.

## Attachment handling

Inbound attachments are stored under `<workspace>/attachments/<conversation>/`. The agent gets file paths in its context and can read them via the `file_read` tool. `max_attachment_bytes` (default 25 MiB) caps inbound size; oversize attachments are skipped with a log warning.

Outbound attachments are not supported yet — the agent replies with links to files in the workspace, and the user downloads via whatever tunnel the workspace is exposed through.

## Rate and volume limits

Email isn't optimised for conversational latency. Expect:

- IMAP delivery latency: near-real-time when the server supports IDLE; otherwise `poll_interval_secs` (default 60 s). Lower at the cost of server load; some providers rate-limit aggressive polling.
- SMTP send: subject to your provider's daily-send quota (Gmail: 500/day for free accounts, 2000/day for Workspace).

## Safety

Email has no auth at the protocol level beyond SMTP's envelope — anyone can claim to be anyone. Always configure `allowed_senders` (strict list of addresses) before exposing the agent to an inbox that receives public mail.
