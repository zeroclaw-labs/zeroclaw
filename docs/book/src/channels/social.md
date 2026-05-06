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

## Mastodon (ActivityPub)

```toml
[channels.mastodon]
enabled = true
instance_url = "https://mastodon.social"   # any compatible instance
access_token = "xxxxxxxx"                  # see "Token mint" below
allowed_users = ["alice@mastodon.social"]  # `*` allows anyone
mention_only = true                        # default; respond only when @-mentioned
visibility = "direct"                      # default reply visibility
poll_interval_secs = 60                    # polling fallback cadence
```

- **Auth:** personal access token minted via instance UI. No OAuth code-grant flow in v1.
- **Inbound:** subscribes to the user-stream WebSocket at `wss://{instance}/api/v1/streaming?stream=user`. Reconnects with exponential backoff and falls back to polling `/api/v1/notifications` after three consecutive failures.
- **Outbound:** posts statuses with the configured `visibility`. Bodies over 500 characters are split at sentence boundaries and threaded, with each chunk re-applying the recipient `@mention` so non-public visibility levels keep delivering.
- **Recipient format:** `user@instance` for new DMs/posts; `user@instance|<status_id>` to reply in-thread. The agent's runtime sets this automatically when responding to inbound notifications — operators rarely have to construct it by hand.
- **Compatibility:** any Mastodon-API-compatible server (Pleroma, GoToSocial, Akkoma) should work.

### Token mint

1. Log in to your instance and open **Settings → Development → New Application**.
2. Set the application name (e.g. `zeroclaw-bot`) and grant scopes `read:notifications`, `write:statuses`, `read:accounts`. Leave redirect URIs at the default.
3. Save and copy `Your access token` into `access_token`.
4. Restart the daemon; `zeroclaw channel doctor` should now show **Mastodon: ✅**.

### Visibility safety

`visibility` defaults to `direct` so bot replies stay out of public timelines unless the operator explicitly opts in. `private` (followers-only), `unlisted`, and `public` are supported but each successively widens the audience — flip them only after confirming `allowed_users` does what you expect.

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
