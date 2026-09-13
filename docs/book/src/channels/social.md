# Social Channels

Broadcast / social-feed integrations. These differ from chat channels in two ways: messages are typically public, and the agent often acts as a poster rather than a bidirectional responder.

> **Build note:** Social channels are **not included** in the lean default build. To use them, build with `--features channels-full` (all channels) or the specific feature flag (e.g. `--features channel-twitter`). Prebuilt binaries do not include these channels by default. See [Channels → Overview](./overview.md) for the full build-options table.

## Twitch

Twitch chat is a thin adapter over IRC. Build it with `--features channel-twitch` or include it through `--features channels-full`; the lean default build does not include it.

Configure `enabled`, `bot_username`, `oauth_token`, and the channels to join under a Twitch alias. `mention_only` is optional and defaults to `false`. A minimal instance looks like this:

```toml
[channels.twitch.default]
enabled = true
bot_username = "zeroclaw_bot"
oauth_token = "replace-with-twitch-token"
channels = ["zeroclaw_channel"]
mention_only = true

[peer_groups.twitch_default]
channel = "twitch.default"
external_peers = ["zeroclaw_user"]
```

- **Auth:** use a Twitch user access token for the bot account with [`chat:read` and `chat:edit`](https://dev.twitch.tv/docs/chat/irc/#authenticating-with-the-twitch-irc-server). After configuring the [Twitch CLI](https://dev.twitch.tv/docs/cli/token-command/), generate it while signed in as that account with `twitch token -u -s 'chat:read chat:edit'`, then store it through a protected config surface. ZeroClaw trims the value and adds the required `oauth:` prefix when it is omitted.
- **Channels:** entries in `channels` may include the leading `#`; ZeroClaw adds it when missing and normalizes channel names to lowercase.
- **Inbound and outbound:** channel messages are answered in the same channel. `mention_only = true` ignores channel messages that do not mention `bot_username`. If Twitch delivers a non-channel IRC `PRIVMSG` to the bot login, ZeroClaw replies to that sender; the adapter does not configure a separate whisper transport.
- **Formatting:** Twitch replies use plain text and are split to fit IRC frames. Markdown formatting is not preserved.
- **Rate limits:** the adapter writes IRC `PRIVMSG` frames directly and has no Twitch-specific rate limiter or HTTP `429` backoff. Keep the agent's posting cadence within Twitch Chat limits and throttle bursty workflows at their source.

## Bluesky (AT Protocol)

- **Auth:** Bluesky app-password (not your real password). Create one in settings.
- **Outbound:** 300-character posts; longer responses auto-thread.
- **Protocol:** AT Protocol via the `atrium-api` crate.

## Nostr

{{#peer-group nostr}}

- **Auth:** raw private key (`nsec` bech32 or hex).
- **Inbound:** kind-1 (text), kind-4 (DM, NIP-04), and kind-1059 (gift-wrap, NIP-17).
- **Outbound:** same kinds. Zap handling is experimental.
- **Relays:** the agent connects to all listed relays; use 3–5 for reliability. If `relays` is omitted, ZeroClaw connects to a built-in set of popular public relays.

## Twitter / X

{{#peer-group twitter}}

- **Auth:** Twitter API v2 OAuth 2.0 Bearer Token only.
- **Inbound:** mentions via the Filtered Stream endpoint.
- **Outbound:** posts, replies, threads.
- **Caveat:** the free tier is rate-limited to the point of near-uselessness. Budget accordingly.

## Reddit

- **Auth:** OAuth 2.0 with a refresh token. Generate one with a script-type Reddit app and the `password` or `code` flow.
- **Inbound:** new posts and comments in the configured subreddits (or all subreddits the bot has access to when `subreddits` is empty), plus replies to the agent's own posts.
- **Outbound:** posts, comments, private messages.

---

## Operating social channels safely

Bots on public social networks attract adversarial input. Two precautions:

1. **Restrict who the agent will respond to.** Gate inbound senders with a peer group (per channel, above): an empty peer set denies everyone, `["*"]` accepts anyone. Bluesky has no peer-group sender field; gate at the autonomy / tool layer instead.
2. **Keep autonomy level at `Supervised` or lower.** A public-facing agent in `Full` autonomy is effectively a public shell. For public-facing channels, restrict the tool surface in the global tool-policy config rather than expecting per-channel `tools_allow` (no such per-channel field exists).

## Rate limits

Rate-limit handling differs by channel. Twitch writes IRC `PRIVMSG` frames directly and has no adapter-specific limiter. If you hit persistent rate-limiting, throttle the agent's posting cadence at the source rather than relying on per-channel streaming knobs (none of these channels expose draft-update intervals; their schema is intentionally minimal).
