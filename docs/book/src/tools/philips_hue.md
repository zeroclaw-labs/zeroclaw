# Philips Hue

The `philips_hue` tool drives a local [Philips Hue Bridge](https://www.philips-hue.com/) over the v2 CLIP API. It is **disabled by default** and requires a one-time push-button pairing to mint an application key.

When enabled, the agent can:

- `list_lights`, `get_light` — read light state.
- `set_light` — turn lights on/off, change brightness, set xy color or color temperature.
- `list_scenes`, `recall_scene` — discover and recall scenes.
- `list_rooms` — enumerate rooms.
- `list_groups`, `set_group` — read and mutate `grouped_light` state.

Mutating actions (`set_light`, `recall_scene`, `set_group`) are gated by `allowed_resource_types`. Read actions ignore that allowlist.

## Configuration

```toml
[philips_hue]
enabled = true
bridge_address = "192.168.1.42"            # IP or "<bridge-id>.local"
application_key = "abcdef..."              # or set PHILIPS_HUE_APPLICATION_KEY
allowed_resource_types = ["light", "grouped_light", "scene", "room"]
verify_tls = false                         # bridges ship with self-signed certs
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `allowed_resource_types` | `["light", "grouped_light", "scene", "room"]` |
| `verify_tls` | `false` |
| `request_timeout_secs` | `15` |

The `application_key` field carries `#[secret]` so it's encrypted at rest when `[secrets] encrypt = true`.

## First-time pairing

The Hue Bridge requires a physical button press before it will mint an application key.

1. Discover the bridge address:
   - Cloud: `curl https://discovery.meethue.com` returns a list of bridges seen from your IP.
   - Local: mDNS hostname `<bridge-id>.local`.
2. Press the round button on top of the bridge.
3. Within 30 seconds:
   ```bash
   curl -k -X POST \
        -H 'Content-Type: application/json' \
        -d '{"devicetype":"zeroclaw#host","generateclientkey":true}' \
        https://<bridge-ip>/api
   ```
4. The response includes `success.username` — that's your `application_key`. Copy it into `philips_hue.application_key`, or export `PHILIPS_HUE_APPLICATION_KEY`.

The `-k` flag skips TLS verification because bridges present a self-signed certificate, which is the same reason `verify_tls` defaults to `false` in the tool.

## TLS

`verify_tls = false` (default) is the safe-on-LAN choice: Hue bridges only ship with self-signed certs, so a verifying client cannot connect at all. If your bridge is exposed via a reverse proxy that terminates TLS with a real certificate, set `verify_tls = true` and `bridge_address` to the proxy's hostname.

## Allowlist guidance

`allowed_resource_types` is the per-mutation throttle. Conservative starting points:

- Lights only: `["light"]`.
- Lights + groups + scenes: `["light", "grouped_light", "scene"]`.
- Read-only deployment: `[]` — disables every `set_*`/`recall_*` action while keeping `list_*` and `get_light` available.

## Examples

```jsonc
// Turn a light on at 60% brightness
{
  "action": "set_light",
  "id": "00112233-4455-6677-8899-aabbccddeeff",
  "on": true,
  "brightness": 60
}

// Recall a scene
{
  "action": "recall_scene",
  "id": "deadbeef-1234-5678-90ab-cdef12345678"
}

// Set a group to warm white
{
  "action": "set_group",
  "id": "feedface-1111-2222-3333-444455556666",
  "on": true,
  "color_temperature_mirek": 366
}
```

## Troubleshooting

- **`401 Unauthorized` / `403 Forbidden`** — Application key is wrong or revoked. Re-run the pairing flow.
- **Connection refused / TLS errors** — Confirm `bridge_address` is reachable on your LAN; if you set `verify_tls = true`, the bridge's cert must be in your trust store.
- **`Resource type 'X' is not in allowed_resource_types`** — Either widen the allowlist or use a different operation.
- **`set_light` succeeds but nothing happens** — Check the bridge response body; v2 returns errors as `{"errors": [...]}` even with HTTP 200. Common causes: wrong resource ID, light is unreachable (powered off at the wall), or scene already active.

## Rollback

Set `[philips_hue] enabled = false` (or delete the block) and restart the agent. No data migration is needed — the tool only talks to the bridge over HTTP.
