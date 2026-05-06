# Home Assistant

The `home_assistant` tool talks to a self-hosted [Home Assistant](https://www.home-assistant.io/) instance over its REST API. It is **disabled by default** and gated behind a config block plus a long-lived access token.

When enabled, the agent can:

- `get_state` — read a single entity's state.
- `list_states` — enumerate every entity HA exposes.
- `list_services` — discover service domains and the services each domain supports.
- `call_service` — trigger a service (turn lights on/off, run an automation, etc.) restricted to operator-allowed domains.

## Configuration

```toml
[home_assistant]
enabled = true
base_url = "http://homeassistant.local:8123"
access_token = "eyJhbGciOi..."             # or set HOME_ASSISTANT_TOKEN
allowed_domains = ["light", "switch", "scene", "script"]
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `allowed_domains` | `["light", "switch", "scene", "climate", "media_player", "input_boolean", "script", "automation"]` |
| `request_timeout_secs` | `15` |

The `access_token` field carries the `#[secret]` attribute, so it is encrypted at rest when `[secrets] encrypt = true`.

## Generating a long-lived access token

1. Open Home Assistant in a browser.
2. Click your profile (bottom-left) → **Security** tab.
3. Scroll to **Long-lived access tokens** → **Create token**.
4. Name it (e.g. `zeroclaw`) and copy the value — HA only shows it once.
5. Either paste into `home_assistant.access_token` or export `HOME_ASSISTANT_TOKEN`.

## Allowlist guidance

`allowed_domains` is the blast radius for `call_service`. Start with the smallest set you need:

- Lights only? `["light"]`.
- Lights + scenes? `["light", "scene"]`.
- Need to fire HA automations? Add `"automation"` or `"script"`.

Read actions (`get_state`, `list_states`, `list_services`) ignore `allowed_domains` — they are read-only and surface anything HA exposes to the token's user.

## Examples

```jsonc
// Turn on the kitchen lights
{
  "action": "call_service",
  "domain": "light",
  "service": "turn_on",
  "service_data": { "entity_id": "light.kitchen", "brightness_pct": 60 }
}

// Read a single sensor
{
  "action": "get_state",
  "entity_id": "sensor.living_room_temperature"
}
```

## Troubleshooting

- **`401 Unauthorized`** — Token is wrong, expired, or revoked. Mint a new one.
- **Timeouts** — Bump `request_timeout_secs`; if HA is on a separate VLAN, check routing.
- **Domain `'X'` not in allowed_domains** — Either widen `allowed_domains` or use a different service domain.
- **Self-signed TLS** — If `base_url` is `https://` with an internal CA, the underlying `reqwest` client will reject it. Either install the CA in the system trust store or terminate TLS at a reverse proxy.

## Rollback

Set `[home_assistant] enabled = false` (or remove the block) and restart the agent. No data migration is needed; the integration only reads/writes through HA's own REST API.
