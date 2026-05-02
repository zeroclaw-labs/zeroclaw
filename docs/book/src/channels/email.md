# Email

Two email channels depending on how you want inbound messages delivered.

## IMAP + SMTP (`email_channel`)

The general-purpose email channel. Polls IMAP for new messages, sends via SMTP. Works with Gmail, Outlook, Fastmail, self-hosted Postfix, and anything else that speaks IMAP/SMTP.

```toml
[channels.email]
enabled = true

[channels.email.imap]
host = "imap.example.com"
port = 993
username = "you@example.com"
password = "..."                 # or app-password for Gmail/iCloud
mailbox = "INBOX"
poll_interval_secs = 60

[channels.email.smtp]
host = "smtp.example.com"
port = 587
username = "you@example.com"
password = "..."

[channels.email.filter]
allowed_senders = ["boss@example.com", "alerts@example.com"]
subject_prefix = "[agent]"       # only respond to subjects starting with this
```

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

## InboxAPI skill

Use the InboxAPI skill when you already have an InboxAPI mailbox and want ZeroClaw to operate on it without configuring IMAP and SMTP directly inside ZeroClaw.

This is the recommended Phase 1 InboxAPI integration path:

```bash
zeroclaw skills install inboxapi
```

### Operator setup

1. Install the InboxAPI CLI:

```bash
npm install -g @inboxapi/cli
```

2. Authenticate once:

```bash
inboxapi login
```

3. Restart or re-open the agent session so the newly installed skill is visible to the model.

### What the skill supports

- Search inbox messages by sender, subject, and date.
- Read single messages and full thread context.
- Summarize recent mail for triage.
- Send new outbound mail.
- Reply in-thread through InboxAPI using the original message id.
- Forward mail and fetch attachments on demand.

The skill intentionally keeps InboxAPI as the source of truth for thread identity and delivery semantics. For replies, it uses `send-reply --message-id ...` instead of reconstructing SMTP threading inside ZeroClaw.

### Optional MCP tool path

If you want direct InboxAPI tools in ZeroClaw in addition to the skill prompt, register the InboxAPI CLI as a stdio MCP server:

```toml
[mcp]
enabled = true

[[mcp.servers]]
name = "inboxapi"
transport = "stdio"
command = "npx"
args = ["-y", "@inboxapi/cli"]
```

That exposes InboxAPI tool calls through the runtime's normal MCP surface while keeping the same underlying account and auth flow.

### When not to use it

Use the native email channels above when you want ZeroClaw to own inbound polling or push delivery directly. Use the InboxAPI skill when you want a lighter operator path that reuses an existing InboxAPI mailbox and preserves InboxAPI-specific reply semantics.

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
