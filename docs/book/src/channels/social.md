# Social Channels

Broadcast / social-feed integrations. These differ from chat channels in two ways: messages are typically public, and the agent often acts as a poster rather than a bidirectional responder.

## Bluesky (AT Protocol)

```toml
[channels.bluesky]
enabled = true
handle = "you.bsky.social"
app_password = "xxxx-xxxx-xxxx-xxxx"      # create at bsky.app/settings/app-passwords
allowed_mentions = ["@trustedfriend.bsky.social"]
```

- **Auth:** Bluesky app-password (not your real password). Create one in settings.
- **Inbound:** mentions and direct messages trigger the agent. Scope with `allowed_mentions`.
- **Outbound:** 300-character posts; longer responses auto-thread.
- **Protocol:** AT Protocol via `atrium-api` crate.

## Nostr

```toml
[channels.nostr]
enabled = true
private_key_hex = "..."                   # nsec in hex
relays = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
]
allowed_pubkeys = ["npub1..."]
```

- **Auth:** raw private key. Store in the encrypted secrets backend — never in a checked-in config.
- **Inbound:** kind-1 (text), kind-4 (DM, NIP-04), and kind-1059 (gift-wrap, NIP-17).
- **Outbound:** same kinds. Zap handling is experimental.
- **Relays:** the agent connects to all listed relays; use 3–5 for reliability.

## Twitter / X

```toml
[channels.twitter]
enabled = true
api_key = "..."
api_secret = "..."
access_token = "..."
access_secret = "..."
bearer_token = "..."
```

- **Auth:** OAuth 1.0a app credentials. Requires Twitter Developer Portal access — paid tier for full API v2 access.
- **Inbound:** mentions via the Filtered Stream endpoint.
- **Outbound:** posts, replies, threads.
- **Caveat:** the free tier is rate-limited to the point of near-uselessness. Budget accordingly.

## Reddit

```toml
[channels.reddit]
enabled = true
client_id = "..."
client_secret = "..."
username = "..."
password = "..."                          # or use refresh_token
user_agent = "zeroclaw-agent/0.1 by your-username"
subreddits = ["rust", "commandline"]
```

- **Auth:** OAuth 2.0 password flow or refresh token. Password flow requires a script-type app.
- **Inbound:** new posts and comments in the configured subreddits, plus replies to the agent's own posts.
- **Outbound:** posts, comments, private messages.
- **User-agent convention:** Reddit's API requires a descriptive user-agent string. Non-compliance → rate limits.

## Lemmy

```toml
[channels.lemmy]
enabled = true
instance_url = "https://lemmy.world"        # any Lemmy-compatible instance
# Two auth paths — pick one:
#   1) Pre-minted JWT (recommended, required for 2FA accounts)
jwt = "eyJhbGciOiJIUzI1Ni..."               # SECRET — copy from browser cookie or admin UI
#   2) Username + password (channel auto-logs in at startup)
# username = "agent-bot"
# password = "..."                          # SECRET — stored via the same redaction pipeline as Reddit
allowed_users = ["alice", "bob@beehaw.org"] # bare or instance-qualified; `*` allows anyone
poll_interval_secs = 30                     # default 30s; lower bound 5s
```

- **Auth model:** Lemmy v3 REST. Two paths: pre-minted JWT (recommended for production) or username + password — the channel will call `POST /api/v3/user/login` once at startup and cache the token. v1 does **not** support 2FA on the bot account; use the JWT path if that applies.
- **Scope:** v1 handles **private messages only**. The bot polls `GET /api/v3/private_message/list?unread_only=true` every `poll_interval_secs`, dispatches each PM as a `ChannelMessage`, and `PUT /api/v3/private_message/mark_as_read` so the next poll doesn't re-deliver. Comment / post listening is deferred to v2.
- **Outbound:** `POST /api/v3/private_message` with `recipient_id` + `content`. Bodies over 10000 chars are split at sentence/word boundaries with `(i/N) ` continuation markers.
- **Recipient grammar:** `pm:{user_id}` (canonical) or bare numeric (shorthand). Set automatically when the agent replies to inbound PMs.
- **Allowlist forms:** bare usernames (`alice`) match both local and federated senders; instance-qualified (`alice@beehaw.org`) require an exact match. Comparison is case-insensitive. Empty list denies all; `*` wildcard allows anyone.
- **Self-hosted TLS:** common in self-hosted installs. Default `reqwest` client honours system trust roots; private CAs need OS-level install.

---

## Operating social channels safely

Bots on public social networks attract adversarial input. Two precautions:

1. **Restrict who the agent will respond to.** Use `allowed_mentions` / `allowed_pubkeys` / `allowed_users` to whitelist. The default empty-list behaviour varies per channel — check each.
2. **Keep autonomy level at `Supervised` or lower.** A public-facing agent in `Full` autonomy is effectively a public shell. If you want to run public-facing, disable shell tools for that channel:

```toml
[channels.bluesky]
tools_allow = ["http", "web_search"]      # whitelist — no shell, no file_write
```

## Rate limits and backoff

All social channels are subject to aggressive rate limits. ZeroClaw's outbound queue uses exponential backoff on 429 responses. If you hit persistent rate-limiting, lower `draft_update_interval_ms` and check whether you're accidentally editing messages (Bluesky does not support edits; others have per-operation limits).
