# SOP Fan-In: Webhook

The gateway exposes two authenticated HTTP entry points for webhook-triggered
SOPs:

- `POST /sop/{path}` is SOP-only. It dispatches a matching SOP and returns
  `404` when no loaded SOP declares that exact path. It never falls back to an
  agent or model call.
- `POST /webhook` checks for an exact `/webhook` SOP trigger first. If none
  matches, it retains the normal webhook chat behavior.

Run these endpoints through `zeroclaw daemon` with `sop.sops_dir` configured.
They use the daemon's shared SOP engine. A standalone `zeroclaw gateway start`,
or a daemon without the SOP subsystem enabled, returns `503` from `/sop/*`.

## Trigger

{{#sop-trigger webhook}}

The path match is exact. For example:

```toml
[[triggers]]
type = "webhook"
path = "/sop/deploy"
```

fires for `POST /sop/deploy`, but not for `/sop/deploy/` or
`/sop/deploy/production`.

## Request and response

`/sop/*` accepts an empty body or any valid JSON value. The request path becomes
the event topic and the canonical JSON body becomes its payload. Invalid JSON
returns `400`.

Send every control that is configured. With `gateway.require_pairing = true`
*and* `gateway.webhook_secret` set (the configuration shown below), a complete
request carries both:

```bash
curl -X POST http://127.0.0.1:42617/sop/deploy \
  -H 'Authorization: Bearer <paired-token>' \
  -H 'X-Webhook-Secret: <gateway.webhook_secret>' \
  -H 'Content-Type: application/json' \
  -H 'X-Idempotency-Key: deploy-2026-07-20-001' \
  -d '{"revision":"abc123"}'
```

When only one control is configured, send only that one:

```bash
# gateway.webhook_secret set, pairing not required
curl -X POST http://127.0.0.1:42617/sop/deploy \
  -H 'X-Webhook-Secret: <gateway.webhook_secret>' \
  -H 'Content-Type: application/json' \
  -d '{"revision":"abc123"}'

# gateway.require_pairing = true, no webhook secret configured
curl -X POST http://127.0.0.1:42617/sop/deploy \
  -H 'Authorization: Bearer <paired-token>' \
  -H 'Content-Type: application/json' \
  -d '{"revision":"abc123"}'
```

A successful match returns `200` with one result per matching SOP. Admission
outcomes such as `skipped`, `deferred`, and `coalesced` are reported in that
array. Input rejected by the SOP untrusted-input guard returns `422`.

## Authentication and idempotency

Both entry points use the gateway webhook security controls:

- pairing bearer authentication when gateway pairing is required;
- the optional `X-Webhook-Secret` configured by
  `gateway.webhook_secret`; and
- webhook rate limiting.

Starting a SOP run authorizes real side effects, so dispatch fails closed:
at least one control must be configured. Every configured control must pass:
when pairing is required, send a valid `Authorization: Bearer <paired-token>`;
when `gateway.webhook_secret` is set, send its exact value in
`X-Webhook-Secret`; when both are configured, send both.

The credential policy is read **once per request**. Authorization captures an
immutable snapshot of which controls are configured and which of them the
request satisfied, and the SOP dispatch gate decides from that snapshot alone.
A configuration change that lands while a request is in flight therefore cannot
mix two security states inside one request: a request that presented no
credential is never admitted because a secret was added mid-flight, and a
request bearing a retired secret is never admitted because its replacement is
present. Rotation takes effect at next-request granularity.

```toml
[gateway]
webhook_secret = "replace-with-a-random-secret"
```

`[channels.webhook.<alias>].secret` is not a gateway credential. It belongs to
the separate webhook channel listener and verifies
`X-Webhook-Signature: sha256=<HMAC>`. Multiple channel aliases, including
disabled or stale aliases, never affect gateway/SOP authorization.

With neither gateway control configured (for example
`gateway.require_pairing = false` and no `gateway.webhook_secret`), `/sop/*`
returns the same `401` before parsing JSON or consulting the SOP engine. An
anonymous caller therefore cannot distinguish malformed JSON, engine
availability, or whether a path matches. For `/webhook`, the fail-closed
credential requirement applies only when a SOP trigger matches; an unmatched
request retains the existing chat fallback policy.

Optional `X-Idempotency-Key` replay protection is namespaced per SOP path, not
just per endpoint family: the same key sent to two different SOP paths (e.g.
`/sop/deploy` then `/sop/rollback`) is treated as two distinct requests, and
`/sop/*` keys never collide with `/webhook` keys. The stored key is a
length-prefixed encoding of the endpoint domain, the path namespace, and the
caller key, so it is injective: no caller-controlled value can be crafted to
land on another path's or another endpoint's replay slot. HTTP delivery is
at-most-once by attempt: the key is reserved before dispatch, so a race such
as SOP unload between matching and dispatch may consume the key without
starting a run. A duplicate response therefore says that a prior request
reserved the key and that no new dispatch started; it does not claim the prior
attempt completed successfully. A `deferred` result is observable but is not
automatically retried by the gateway.

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
