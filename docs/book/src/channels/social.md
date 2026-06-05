# Social Channels

Broadcast / social-feed integrations. These differ from chat channels in two ways: messages are typically public, and the agent often acts as a poster rather than a bidirectional responder.

> **Build note:** Social channels are **not included** in the lean default build. To use them, build with `--features channels-full` (all channels) or the specific feature flag (e.g. `--features channel-twitter`). Prebuilt binaries do not include these channels by default. See [Channels → Overview](./overview.md) for the full build-options table.

## Bluesky (AT Protocol)

```toml
[channels.bluesky]
enabled = true
handle = "you.bsky.social"
app_password = "xxxx-xxxx-xxxx-xxxx"      # create at bsky.app/settings/app-passwords
```

- **Auth:** Bluesky app-password (not your real password). Create one in settings.
- **Outbound:** 300-character posts; longer responses auto-thread.
- **Protocol:** AT Protocol via the `atrium-api` crate.

## Nostr

```toml
[channels.nostr]
enabled = true
private_key = "..."                       # nsec bech32 or hex
relays = [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.primal.net",
]
allowed_pubkeys = ["npub1..."]            # empty = deny all, "*" = allow all
```

- **Auth:** raw private key (`nsec` bech32 or hex). Store in the encrypted secrets backend — never in a checked-in config.
- **Inbound:** kind-1 (text), kind-4 (DM, NIP-04), and kind-1059 (gift-wrap, NIP-17).
- **Outbound:** same kinds. Zap handling is experimental.
- **Relays:** the agent connects to all listed relays; use 3–5 for reliability. If `relays` is omitted, ZeroClaw connects to a built-in set of popular public relays.

## Twitter / X

```toml
[channels.twitter]
enabled = true
bearer_token = "..."                      # Twitter API v2 OAuth 2.0 Bearer Token
allowed_users = ["singlerider"]           # usernames or user IDs; empty = deny all, "*" = allow all
```

- **Auth:** Twitter API v2 OAuth 2.0 Bearer Token only.
- **Inbound:** mentions via the Filtered Stream endpoint.
- **Outbound:** posts, replies, threads.
- **Caveat:** the free tier is rate-limited to the point of near-uselessness. Budget accordingly.

## Reddit

```toml
[channels.reddit]
enabled = true
client_id = "..."
client_secret = "..."
refresh_token = "..."                     # OAuth 2.0 refresh token (required)
username = "your-bot-username"            # without `u/` prefix
subreddit = "rust"                        # optional: filter to a single subreddit (without `r/` prefix)
```

- **Auth:** OAuth 2.0 with a refresh token. Generate one with a script-type Reddit app and the `password` or `code` flow, then save the refresh token here for persistent access.
- **Inbound:** new posts and comments in the configured subreddit (or all subreddits the bot has access to when `subreddit` is unset), plus replies to the agent's own posts.
- **Outbound:** posts, comments, private messages.

## Mastodon

The Mastodon channel speaks the standard Mastodon REST + Streaming APIs and works against any Mastodon-compatible instance (mastodon.social, fosstodon.org, hachyderm.io, Pleroma, GoToSocial). The agent reads inbound mentions and direct messages, then posts replies back as statuses. It is a polling channel — no inbound webhook or public route is required.

**Getting credentials.** Mint a personal access token on your instance under **Settings → Development → New Application**. Grant the scopes `read`, `write:statuses`, and `read:notifications` (the last is required for inbound listening). The OAuth code-grant flow is not implemented; the manual token mint is the supported path for a single bot account.

```toml
[channels.mastodon.default]
enabled = true                             # must be explicitly enabled (default false)
instance_url = "https://mastodon.social"   # instance base URL; trailing slash optional (stripped at load)
access_token = "..."                        # PAT from Settings → Development → New Application (read, write:statuses, read:notifications)
allowed_users = ["alice@mastodon.social"]  # user@instance form; empty = deny all, "*" = allow all (local accounts may omit @instance)
mention_only = true                        # default true: only respond to statuses that @-mention the bot (DMs always count)
visibility = "direct"                      # direct | private | unlisted | public — default direct (outbound reply visibility)
poll_interval_secs = 60                    # default 60: polling fallback cadence when streaming is unavailable
excluded_tools = []                        # tools withheld from the model on this channel
```

**How it works.** On `listen()` the channel first resolves its own account via `GET /api/v1/accounts/verify_credentials`, then subscribes to the user-stream WebSocket at `wss://{instance}/api/v1/streaming?stream=user&access_token=…` and routes `notification` events of type `mention` into prompts. Disconnects retry with exponential backoff (1s → 60s). After three consecutive connect failures the channel falls back to **polling** `GET /api/v1/notifications?types[]=mention` every `poll_interval_secs` (default 60, floored at 5s), deduping by `since_id`. Outbound replies are sent via `POST /api/v1/statuses` (bearer-auth) with `status`, `visibility`, and an optional `in_reply_to_id`; bodies over 500 characters are split into threaded chunks, each re-prefixed with the recipient `@mention` and an `(i/N)` marker.

**Allowlist & filtering.** `allowed_users` lists accounts in `user@instance` form; an empty list **denies everyone**, `"*"` allows everyone, and matching is case-insensitive. Local-instance accounts may be listed bare (without `@instance`). When `mention_only` is true (the default) the bot ignores statuses that do not @-mention it, except `direct`-visibility statuses, which always count as mentions. The bot never reacts to its own statuses, and notification types other than `mention` (favourite, reblog, follow, poll) are ignored. Outbound replies use the configured `visibility`, which defaults to `direct` so replies are not broadcast to public timelines unless an operator explicitly opts in.

- **Slot:** alias-keyed `[channels.mastodon.<alias>]` (any ActivityPub-compatible instance).
- **Polling channel:** no inbound webhook or public route is needed. The `access_token` is a `#[secret]` config field — set it in config, not via environment variables.

## Lemmy

The Lemmy channel works against any Lemmy-compatible instance (lemmy.world, beehaw.org, self-hosted). v1 focuses on **private messages only** — the simplest and most useful surface for a personal agent; comment/post listening is deferred. It is a polling channel — no inbound webhook or public route is required.

**Getting credentials.** Two paths, with the pre-minted JWT checked first:

1. **Username + password** — set `username` and `password` for the bot account; the channel calls `POST /api/v3/user/login` once at startup to mint a JWT and caches it in memory. v1 does not support 2FA via this path.
2. **Pre-minted JWT** — copy a long-lived token from the Lemmy web UI (browser cookie / admin tools) into `jwt`. When non-empty it takes precedence over username/password and the login call is skipped. **Recommended for production and required for accounts with 2FA.**

```toml
[channels.lemmy.default]
enabled = true                                # must be explicitly enabled (default false)
instance_url = "https://lemmy.world"          # instance base URL; trailing slash optional (stripped at load)
username = "your-bot"                          # bot account username; required when `jwt` is empty
password = "..."                               # bot account password; required when `jwt` is empty (prefer jwt in production)
jwt = ""                                       # pre-minted JWT; takes precedence over username/password, required for 2FA accounts
allowed_users = ["alice", "bob@lemmy.world"]  # bare or instance-qualified; empty = deny all, "*" = allow all (case-insensitive)
poll_interval_secs = 30                        # default 30: private-message poll cadence (floored at 5s)
excluded_tools = []                            # tools withheld from the model on this channel
```

**How it works.** After acquiring a JWT (seed JWT or login), the channel resolves its own user id via `GET /api/v3/site` for self-suppression. Every `poll_interval_secs` (default 30, floored at 5s) it polls `GET /api/v3/private_message/list?unread_only=true&page=1&limit=20`. For each delivered message it builds a `ChannelMessage` with `reply_target = "pm:{creator_id}"`, then issues `POST /api/v3/private_message/mark_as_read` (best-effort) so the next poll does not re-deliver it; dropped messages are also marked read so they stop reappearing in the unread list. A 401 clears the cached auth and forces a re-login. Outbound replies are sent via `POST /api/v3/private_message` with `{recipient_id, content}` (bearer-auth); bodies over 10000 characters are split with `(i/N)` continuation markers.

**Allowlist & filtering.** `allowed_users` may be **bare** (`"alice"`) or **instance-qualified** (`"alice@lemmy.world"`). An empty list **denies everyone**, `"*"` allows everyone, and matching is case-insensitive. A bare allowlist entry matches both bare and instance-qualified senders; a qualified entry requires an exact match on the full string. The bot never reacts to its own messages, and read or deleted messages are skipped.

- **Slot:** alias-keyed `[channels.lemmy.<alias>]`.
- **Polling channel:** no inbound webhook or public route is needed. The `password` and `jwt` fields are `#[secret]` config fields — set them in config, not via environment variables.

---

## Operating social channels safely

Bots on public social networks attract adversarial input. Two precautions:

1. **Restrict who the agent will respond to.** Use `allowed_pubkeys` (Nostr) or `allowed_users` (Twitter) to whitelist senders. Bluesky has no per-channel allowlist field — gate at the autonomy / tool layer instead. The default empty-list behaviour is **deny all** for the channels that have an allowlist field.
2. **Keep autonomy level at `Supervised` or lower.** A public-facing agent in `Full` autonomy is effectively a public shell. For public-facing channels, restrict the tool surface in the global tool-policy config rather than expecting per-channel `tools_allow` (no such per-channel field exists).

## Rate limits and backoff

All social channels are subject to aggressive rate limits. ZeroClaw's outbound queue uses exponential backoff on 429 responses. If you hit persistent rate-limiting, throttle the agent's posting cadence at the source rather than relying on per-channel streaming knobs (none of these channels expose draft-update intervals; their schema is intentionally minimal).
