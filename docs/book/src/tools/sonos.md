# Sonos

The `sonos` tool drives a Sonos household via the official [Sonos Control API](https://developer.sonos.com/build/direct-control/) at `api.ws.sonos.com`. It is **disabled by default** and uses a refresh-token OAuth flow — the operator does the one-time auth dance externally and pastes the refresh token into config.

When enabled, the agent can:

- `list_households` — list households the linked account has access to.
- `list_groups` — list groups (zones) and players within a household.
- `get_playback_status` — read the current playback state for a group.
- `list_favorites` — list saved favorites in a household.
- `play` / `pause` — start or stop a group's playback.
- `set_volume` — set group volume (0-100).
- `play_favorite` — load a saved favorite into a group and start playback.

Mutating actions (`play`, `pause`, `set_volume`, `play_favorite`) require the action to be present in `allowed_actions`. Reads are gated by `allowed_actions` too — empty list disables every action.

## Configuration

```toml
[sonos]
enabled = true
client_id = "..."
client_secret = "..."                           # or set SONOS_CLIENT_SECRET
refresh_token = "..."                           # or set SONOS_REFRESH_TOKEN
allowed_actions = [                             # default: read-only
    "list_households",
    "list_groups",
    "get_playback_status",
    "list_favorites",
    # "play", "pause", "set_volume", "play_favorite" — uncomment to enable
]
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `allowed_actions` | `["list_households", "list_groups", "get_playback_status", "list_favorites"]` (read-only) |
| `request_timeout_secs` | `15` |

`client_secret` and `refresh_token` carry `#[secret]` so they're encrypted at rest when `[secrets] encrypt = true`.

## One-time refresh-token mint

ZeroClaw does not run a callback server in v1. Mint the refresh token once, externally, then paste it into config.

1. Register an integration at <https://developer.sonos.com/>. Note the `Key` (client ID) and `Secret`. Add a redirect URI such as `http://127.0.0.1:8888/callback`.
2. Open the authorization URL in a browser, replacing `CLIENT_ID`:
   ```
   https://api.sonos.com/login/v3/oauth?
     client_id=CLIENT_ID&
     response_type=code&
     state=zeroclaw&
     scope=playback-control-all&
     redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback
   ```
3. Approve. You'll be redirected to `127.0.0.1:8888/callback?code=AUTHCODE&state=zeroclaw`. Copy the `code` value.
4. Exchange the code for a refresh token:
   ```bash
   curl -X POST https://api.sonos.com/login/v3/oauth/access \
     -u "CLIENT_ID:CLIENT_SECRET" \
     -d grant_type=authorization_code \
     -d code=AUTHCODE \
     -d redirect_uri=http://127.0.0.1:8888/callback
   ```
5. The response includes `refresh_token` — paste it into `[sonos] refresh_token` (or export `SONOS_REFRESH_TOKEN`). The `access_token` in that response can be discarded; the tool re-mints one as needed.

Refresh tokens are long-lived. You only repeat this flow if Sonos revokes the token (e.g. you re-link the account).

## Allowlist guidance

`allowed_actions` is the per-action throttle. Conservative starting points:

- Read-only: defaults — `list_households`, `list_groups`, `get_playback_status`, `list_favorites`.
- Add control: include `"play"`, `"pause"`, `"set_volume"`, and/or `"play_favorite"`.
- Disabled: `[]` blocks every action including reads — equivalent to `enabled = false`.

## Examples

```jsonc
// Discover what we have
{ "action": "list_households" }

// List groups in a household
{
  "action": "list_groups",
  "household_id": "Sonos_abc123..."
}

// Pause a group
{
  "action": "pause",
  "group_id": "RINCON_xxx:1"
}

// Set group volume
{
  "action": "set_volume",
  "group_id": "RINCON_xxx:1",
  "volume": 25
}

// Load a favorite (returned by list_favorites)
{
  "action": "play_favorite",
  "group_id": "RINCON_xxx:1",
  "favorite_id": "12345"
}
```

## Out of scope (v1)

- **Local UPnP control.** Cloud Control API only — the cloud path mirrors Spotify's auth pattern, while UPnP would require an SSDP-capable client and per-device discovery.
- **Audio clip playback.** Sonos's clip endpoint has separate auth-scope considerations; deferred.
- **Group / household reconfiguration** (`createGroup`, `setGroupMembers`).
- **Voice / TTS injection.**

## Token cache

Access tokens are cached in memory only. Refresh fires when the cached token is within 60s of expiry, or when the upstream returns `401` (retried once). Restarting the agent forgets the access token and re-mints from `refresh_token` on the next request.

## Troubleshooting

- **`Sonos token refresh failed (4xx)`** — `refresh_token` is wrong, expired, or doesn't match `client_id`. Re-run the mint flow.
- **`Sonos ... failed (404)`** on group-scoped actions — Stale `group_id`. Households often re-shuffle group IDs after standby/wake; re-fetch with `list_groups`.
- **`Sonos ... failed (403)`** — Missing scope. Re-mint the refresh token with `scope=playback-control-all`.
- **`Action 'X' is not in allowed_actions`** — Add the action to `allowed_actions`.

## Rollback

Set `[sonos] enabled = false` (or delete the block) and restart the agent.
