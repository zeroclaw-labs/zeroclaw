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
- the optional `X-Webhook-Secret` configured for webhooks;
- webhook rate limiting; and
- optional `X-Idempotency-Key` replay protection.

Idempotency keys for `/sop/*` are namespaced separately from `/webhook`, so the
same external event key can be used once in each endpoint family without a
collision. HTTP delivery is at-most-once: a `deferred` result is observable but
is not automatically retried by the gateway.

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
