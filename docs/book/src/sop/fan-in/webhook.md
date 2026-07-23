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

```bash
curl -X POST http://127.0.0.1:42617/sop/deploy \
  -H 'Authorization: Bearer <paired-token>' \
  -H 'Content-Type: application/json' \
  -H 'X-Idempotency-Key: deploy-2026-07-20-001' \
  -d '{"revision":"abc123"}'
```

A successful match returns `200` with one result per matching SOP. Admission
outcomes such as `skipped`, `deferred`, and `coalesced` are reported in that
array. Input rejected by the SOP untrusted-input guard returns `422`.

## Authentication and idempotency

Both entry points use the gateway webhook security controls:

- pairing bearer authentication when gateway pairing is required;
- the optional `X-Webhook-Secret` configured for webhooks; and
- webhook rate limiting.

Starting a SOP run authorizes real side effects, so dispatch fails closed:
whenever a request matches a loaded SOP trigger, at least one of the two
controls above must be configured and satisfied: `gateway.require_pairing =
true` with a valid `Authorization: Bearer <paired-token>`, or
`[channels.webhook.<alias>].secret` with a valid `X-Webhook-Secret`. With
neither configured (for example `gateway.require_pairing = false` and no
webhook secret set), a matching request is rejected with `401` naming both
ways to configure a credential, instead of starting the run. This applies to
`/sop/*` unconditionally, and to `/webhook` only when a SOP trigger actually
matches; an unmatched `/webhook` request still falls back to the existing
chat authentication policy.

Optional `X-Idempotency-Key` replay protection is namespaced per SOP path, not
just per endpoint family: the same key sent to two different SOP paths (e.g.
`/sop/deploy` then `/sop/rollback`) is treated as two distinct requests, and
`/sop/*` keys never collide with `/webhook` keys. HTTP delivery is
at-most-once: a `deferred` result is observable but is not automatically
retried by the gateway.

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
