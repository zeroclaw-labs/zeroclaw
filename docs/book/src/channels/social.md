# Social Channels

Broadcast / social-feed integrations. These differ from chat channels in two ways: messages are typically public, and the agent often acts as a poster rather than a bidirectional responder.

> **Build note:** Social channels are **not included** in the lean default build. To use them, build with `--features channels-full` (all channels) or the specific feature flag (e.g. `--features channel-twitter`). Prebuilt binaries do not include these channels by default. See [Channels → Overview](./overview.md) for the full build-options table.

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

## Rate limits and backoff

All social channels are subject to aggressive rate limits. ZeroClaw's outbound queue uses exponential backoff on 429 responses. If you hit persistent rate-limiting, throttle the agent's posting cadence at the source rather than relying on per-channel streaming knobs (none of these channels expose draft-update intervals; their schema is intentionally minimal).
