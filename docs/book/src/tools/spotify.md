# Spotify

The `spotify` tool drives a Spotify account via the [Web API](https://developer.spotify.com/documentation/web-api). It is **disabled by default** and uses a refresh-token OAuth flow — the operator does the one-time auth dance externally and pastes the refresh token into config.

When enabled, the agent can:

- `get_playback_state` — read the current playback state (track, device, progress, shuffle/repeat).
- `list_devices` — list active Spotify Connect devices.
- `list_playlists` — list the user's playlists.
- `search` — search the catalogue by query and type (`track`, `album`, `artist`, `playlist`, `show`, `episode`).
- `play` — start or resume playback. Optional `uris` (array of `spotify:track:...`) or `context_uri` (album/playlist/artist) to choose what plays.
- `pause` — pause playback.
- `next` / `previous` — skip.
- `set_volume` — set device volume (0-100).

> Mutating actions (`play`, `pause`, `next`, `previous`, `set_volume`) require **Spotify Premium** on the controlled account. Free accounts will see upstream `403`s on those actions.

## Configuration

```toml
[spotify]
enabled = true
client_id = "..."
client_secret = "..."                           # or set SPOTIFY_CLIENT_SECRET
refresh_token = "..."                           # or set SPOTIFY_REFRESH_TOKEN
allowed_actions = [                             # default: read-only set
    "get_playback_state",
    "list_devices",
    "list_playlists",
    "search",
    # "play", "pause", "next", "previous", "set_volume" — uncomment to enable
]
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `allowed_actions` | `["get_playback_state", "list_devices", "list_playlists", "search"]` (read-only) |
| `request_timeout_secs` | `15` |

`client_secret` and `refresh_token` carry `#[secret]` so they're encrypted at rest when `[secrets] encrypt = true`. Empty `allowed_actions` disables every action — including reads.

## One-time refresh-token mint

ZeroClaw does not run a callback server in v1. Mint the refresh token once, externally, then paste it into config. The simplest recipe:

1. Register an app at <https://developer.spotify.com/dashboard>. Note the `Client ID` and `Client Secret`.
2. Add `http://127.0.0.1:8888/callback` to the app's redirect URIs (any redirect URI works as long as it's listed; the loopback above is convenient).
3. Open the authorization URL in a browser, replacing `CLIENT_ID`:
   ```
   https://accounts.spotify.com/authorize?
     client_id=CLIENT_ID&
     response_type=code&
     redirect_uri=http%3A%2F%2F127.0.0.1%3A8888%2Fcallback&
     scope=user-read-playback-state%20user-modify-playback-state%20playlist-read-private%20user-read-currently-playing
   ```
4. Approve. You'll be redirected to `127.0.0.1:8888/callback?code=AUTHCODE...`. Copy the `code` value.
5. Exchange the code for a refresh token:
   ```bash
   curl -X POST https://accounts.spotify.com/api/token \
     -u "CLIENT_ID:CLIENT_SECRET" \
     -d grant_type=authorization_code \
     -d code=AUTHCODE \
     -d redirect_uri=http://127.0.0.1:8888/callback
   ```
6. The response includes `refresh_token` — paste it into `[spotify] refresh_token` (or export `SPOTIFY_REFRESH_TOKEN`).

Refresh tokens are long-lived; you only repeat this if Spotify revokes the token (e.g. you change your password or revoke the app via your account page).

## Allowlist guidance

`allowed_actions` is the per-action throttle. Conservative starting points:

- Read-only deployment: `["get_playback_state", "list_devices", "list_playlists", "search"]` (the default).
- Add control: include `"play"`, `"pause"`, `"next"`, `"previous"`, and/or `"set_volume"` as desired.
- Disabled: `[]` blocks every action including reads — equivalent to `enabled = false`.

## Examples

```jsonc
// Search for a track
{
  "action": "search",
  "query": "anti-hero taylor swift",
  "search_type": "track",
  "limit": 5
}

// Start playing a specific track on the active device
{
  "action": "play",
  "uris": ["spotify:track:0V3wPSX9ygBnCm8psDIegu"]
}

// Pause on a specific device
{
  "action": "pause",
  "device_id": "abc123..."
}

// Set volume to 30%
{
  "action": "set_volume",
  "volume_percent": 30
}
```

## Token cache

Access tokens are cached in memory only. The tool refreshes when:

- The cached token is within 60 seconds of expiry (proactive).
- The upstream returns `401` (reactive — retried once).

Restarting the agent forgets the access token and re-mints from `refresh_token` on the next request.

## Troubleshooting

- **`Spotify token refresh failed (400)`** — `refresh_token` is wrong, revoked, or doesn't match `client_id`. Re-run the mint flow.
- **`Spotify ... failed (403)`** on `play`/`pause`/etc. — Account is not Premium, or no active device.
- **`No active device`** in the response body — Open the Spotify app on a device first, or pass `device_id` from `list_devices`.
- **`Action 'X' is not in allowed_actions`** — Add the action to `allowed_actions`.

## Rollback

Set `[spotify] enabled = false` (or delete the block) and restart the agent.
