# Shazam

The `shazam` tool looks up tracks in the Shazam catalogue via a third-party RapidAPI Shazam service.

> **Unofficial wrapper.** Shazam does not publish a free public API. This tool talks to a service hosted on [RapidAPI](https://rapidapi.com) (default host `shazam.p.rapidapi.com`). The wrapping service may rate-limit, change response shapes, or sunset endpoints without notice — treat as best-effort.

When enabled, the agent can:

- `search_track` — text-search the Shazam catalogue (by title, artist, or both).
- `get_track_details` — fetch full metadata for a track by its Shazam track key.

Audio-fingerprint identification is **not** supported in v1.

## Configuration

```toml
[shazam]
enabled = true
rapidapi_key = "..."                    # or set SHAZAM_RAPIDAPI_KEY
rapidapi_host = "shazam.p.rapidapi.com" # default
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `rapidapi_host` | `shazam.p.rapidapi.com` |
| `request_timeout_secs` | `15` |

`rapidapi_key` carries `#[secret]` so it's encrypted at rest when `[secrets] encrypt = true`.

## Setup (one-time)

1. Sign up at <https://rapidapi.com/>.
2. Subscribe to a Shazam service. The most common is "Shazam" (host `shazam.p.rapidapi.com`); search the marketplace if you want a different wrapper.
3. Copy your RapidAPI key from the dashboard.
4. Either paste it into `[shazam] rapidapi_key`, or export `SHAZAM_RAPIDAPI_KEY`.

Different RapidAPI Shazam services have slightly different endpoint shapes. The default host is the most common; if you subscribe to a different one and the response shape doesn't match what you expect, set `rapidapi_host` to the alternative service's host.

## Examples

```jsonc
// Search for a track
{
  "action": "search_track",
  "query": "shape of you ed sheeran",
  "limit": 5
}

// Fetch details for a specific track key (returned by search_track)
{
  "action": "get_track_details",
  "track_key": "40286312"
}
```

## Out of scope (v1)

- **Audio-fingerprint recognition.** The multipart/audio endpoint exists on most Shazam wrappers but is the most fragile path on a third-party API and was not safe to ship without per-wrapper testing. Deferred until there's demand and a reliable wrapper to verify against.
- **Charts, top-tracks, location-based discovery.** v1 keeps the surface tight to two well-defined ops.

## Troubleshooting

- **`Shazam ... failed (401)` or `(403)`** — RapidAPI key is wrong, expired, or the subscription has lapsed. Check the RapidAPI dashboard.
- **`Shazam ... failed (429)`** — Rate-limited. Free RapidAPI tiers are aggressive about this; either upgrade or back off.
- **Empty / unexpected response shape** — The wrapping service may have changed its schema. Compare against the service's own documentation in the RapidAPI marketplace.

## Rollback

Set `[shazam] enabled = false` (or delete the block) and restart the agent.
