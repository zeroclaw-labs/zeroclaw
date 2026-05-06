# 8Sleep

The `eight_sleep` tool reads and adjusts an [8Sleep](https://www.eightsleep.com/) Pod via 8Sleep's cloud API.

> **Unofficial API.** 8Sleep does not publish a stable public API. This integration uses the same HTTPS endpoints the official mobile app reaches, following the conventions popularized by the open-source [`pyEight`](https://github.com/mezz64/pyEight) library. Endpoints can change at any time — treat this integration as best-effort and expect occasional breakage.

When enabled, the agent can:

- `get_bed_state` — read the current device state, including per-side heating levels and presence.
- `get_metrics` — read sleep intervals (last-night metrics) for the logged-in user.
- `set_temperature` — set the heating level on a side (`-100` cools, `0` holds, `+100` warms).

`set_temperature` is gated by `allowed_sides`. Read actions are not gated by `allowed_sides`.

## Configuration

```toml
[eight_sleep]
enabled = true
email = "you@example.com"
password = "..."                            # or set EIGHT_SLEEP_PASSWORD
api_base_url = "https://client-api.8slp.net/v1"
allowed_sides = ["left", "right"]
request_timeout_secs = 15
```

Defaults:

| Field | Default |
| --- | --- |
| `enabled` | `false` |
| `api_base_url` | `https://client-api.8slp.net/v1` |
| `allowed_sides` | `["left", "right"]` |
| `request_timeout_secs` | `15` |

The `password` field carries `#[secret]` so it's encrypted at rest when `[secrets] encrypt = true`.

## Auth flow

1. POST `{email, password}` → `<api_base_url>/login`.
2. Response includes `session.token` (JWT) and `session.userId`.
3. Subsequent requests send the token via the `Session-Token` header.
4. On any `401`, the tool re-authenticates once and retries the original request.

The token is cached **in memory only** — it is not persisted to disk and is lost on agent restart.

> **Cognito accounts.** Newer 8Sleep accounts that mandate the AWS Cognito OAuth flow are **not supported in v1**. If your `/login` POST returns a `401` immediately on otherwise-correct credentials, that's the most likely cause.

## Side allowlist

`allowed_sides` is the per-mutation throttle. Examples:

- Solo bed: `["left"]` (only the side you sleep on can change temperature).
- Shared bed: `["left", "right"]` (default).
- Read-only deployment: `[]` — all `set_temperature` calls are blocked, but `get_bed_state` and `get_metrics` continue to work.

## Heating-level convention

The `level` field is an integer in `-100..=100`:

- `-100` → maximum cooling.
- `0` → conditioning off (Pod holds without active heat/cool).
- `+100` → maximum warming.

When `set_temperature` runs it sends three fields scoped to the chosen side: `<side>Now: true`, `<side>TargetHeatingLevel`, and `<side>HeatingLevel`. This matches the pyEight body shape; if a future Pod firmware diverges, this is the integration point to revisit.

## Examples

```jsonc
// Warm the left side
{
  "action": "set_temperature",
  "side": "left",
  "level": 60
}

// Cool the right side fully
{
  "action": "set_temperature",
  "side": "right",
  "level": -100
}

// Pull last night's metrics
{ "action": "get_metrics" }

// Inspect current bed state
{ "action": "get_bed_state" }
```

## Out of scope (v1)

- **Alarms.** Alarm scheduling has more API surface (recurring vs one-off, light vs sound vs vibration) and higher safety stakes (a wrong wake time is bad UX). Deferred until the wire format can be verified against an active reverse-engineered library.
- **Prime cycle.** The endpoint shape varies between Pod generations (Pod 2 / Pod 3 / Pod 4) and was not safe to ship without per-generation verification.
- **HRV / sleep stages.** `get_metrics` returns the raw intervals payload; richer derived metrics are left to the agent.

## Troubleshooting

- **`8Sleep login failed (401)`** — Wrong credentials, or the account is on the Cognito-only flow (see above).
- **`could not resolve device id from /users/me response`** — The account has no Pod registered, or the response shape changed. Inspect by calling `get_bed_state` and capturing the raw error body in logs.
- **`level X out of range`** — `level` must be an integer in `-100..=100`.
- **Side `'X'` is not in allowed_sides** — Either widen the allowlist or use a different side.
- **Frequent 401 retries** — The cached token refresh is on a 401-trip-wire, not a TTL. If the upstream consistently returns 401, the integration will re-login on every request, which can rate-limit you. Verify credentials and account state.

## Rollback

Set `[eight_sleep] enabled = false` (or delete the block) and restart the agent. No data migration is needed.
