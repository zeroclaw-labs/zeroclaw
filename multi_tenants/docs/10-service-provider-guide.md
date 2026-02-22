# 10 — Service Provider Guide

Guide for running zcplatform as a commercial SaaS offering.

---

## Infrastructure Requirements

Size the host by expected tenant count. Memory is the primary constraint.

| Tenants | Recommended VPS | Est. Monthly Cost |
|---------|-----------------|-------------------|
| ≤ 50    | 2 vCPU / 2 GB RAM / 40 GB SSD  | ~$10–15/mo |
| ≤ 200   | 4 vCPU / 4 GB RAM / 80 GB SSD  | ~$20–40/mo |
| ≤ 500   | 8 vCPU / 8 GB RAM / 160 GB SSD | ~$40–80/mo |
| ≤ 1,000 | 16 vCPU / 16 GB RAM / 320 GB SSD | ~$80–160/mo |

Additional requirements:
- Docker Engine 24+
- Caddy reverse proxy (bundled in zcplatform deployment)
- Wildcard TLS certificate or per-subdomain Let's Encrypt (Caddy handles automatically)
- Outbound internet access for LLM provider API calls (OpenAI, Anthropic, etc.)

---

## Plan Tier Design

Design plans around the resource knobs zcplatform exposes per tenant.

### Example Tiers

| Tier        | Price     | Tenants | Message Limit | Token Budget | Channels |
|-------------|-----------|---------|---------------|--------------|----------|
| Free Trial  | $0        | 1       | 100/mo        | 50K tokens   | 1        |
| Starter     | $10/mo    | 1       | 5,000/mo      | 2M tokens    | 3        |
| Pro         | $30/mo    | 1       | 25,000/mo     | 10M tokens   | 10       |
| Team        | $99/mo    | 10      | 100,000/mo    | 50M tokens   | unlimited |
| Enterprise  | custom    | custom  | custom        | custom       | unlimited |

Store plan metadata in your billing system; reference plan ID in the tenant record. The `usage_metrics` table provides message and token counts per tenant per hour for enforcement and billing.

Free trial limits recommendation: cap at 14 days OR 100 messages, whichever comes first. Check `usage_metrics` on a daily job; stop the container if limit is exceeded (`POST /api/tenants/{id}/stop`).

---

## Onboarding Flow Design

### Admin-Provisioned (Simplest)

1. Customer signs up via your marketing site.
2. Admin creates tenant: `POST /api/tenants { name, plan }` → returns draft tenant.
3. Admin invites customer as Owner: `POST /api/tenants/{id}/members { email, role: "Owner" }`.
4. Admin deploys: `POST /api/tenants/{id}/deploy`.
5. Customer logs in, sets provider/model config, connects channels.

### Self-Service (Recommended for Scale)

1. Customer submits sign-up form on your site.
2. Your backend calls OTP request and creates the user account via `POST /api/users`.
3. Auto-create tenant with plan limits embedded in config, auto-deploy.
4. Customer lands in the React dashboard immediately.
5. Welcome email contains platform URL and pairing code if hardware agent is relevant.

---

## Billing Integration Points

The `usage_metrics` table is your billing data source.

Schema reference:
```
usage_metrics(tenant_id, hour, messages_total, tokens_input, tokens_output, recorded_at)
```

### Query Usage for Invoice Generation

```sql
-- Monthly usage per tenant for billing period
SELECT
  tenant_id,
  SUM(messages_total) AS messages,
  SUM(tokens_input + tokens_output) AS total_tokens
FROM usage_metrics
WHERE hour >= '2026-02-01' AND hour < '2026-03-01'
GROUP BY tenant_id;
```

### Webhook Pattern

Run a nightly job that:
1. Queries `usage_metrics` for the billing day.
2. Posts usage events to your billing provider (Stripe Metered Billing, Lago, etc.).
3. Marks events as reported to avoid double-counting.

---

## Custom Branding

The React SPA is white-label ready. Override CSS custom properties in `platform-ui/src/styles/theme.css`:

```css
:root {
  --color-primary: #your-brand-color;
  --color-surface: #1a1a2e;
  --color-text: #e0e0e0;
  --font-family-base: 'Inter', sans-serif;
  --logo-url: url('/assets/your-logo.svg');
}
```

Replace `/public/favicon.ico` and `/public/logo.svg` with your brand assets. Set the `<title>` in `index.html` to your product name. No upstream code changes required; these are build-time assets only.

---

## Multi-Region Deployment

Deploy one zcplatform instance per region. Each instance is independent (own DB, own Docker daemon, own vault).

### DNS Routing

Route customers to their assigned region at onboarding:

```
platform-us.example.com  → US node (zcplatform + Caddy)
platform-eu.example.com  → EU node (zcplatform + Caddy)
platform-ap.example.com  → AP node (zcplatform + Caddy)
```

Use GeoDNS or a signup-time region selector. Region assignment is static per tenant; cross-region migration requires backup/restore.

### Cross-Region Admin

There is no federated admin UI across regions in the current architecture. Operate each region independently. For a unified view, build a lightweight aggregation layer that fans out monitoring API calls to each regional node.

---

## SLA Considerations

| Feature | Behavior |
|---------|----------|
| Auto-restart | health_checker restarts failed containers within ~30 s |
| Backup frequency | Configurable; recommend daily minimum |
| Audit trail | All admin actions logged to `audit_log` table |
| Data isolation | Each tenant is a separate Docker container with its own filesystem namespace |
| Encryption at rest | Vault secrets encrypted with XChaCha20-Poly1305 |

Realistic uptime target: 99.5% per tenant (limited by Docker restart latency and external LLM provider availability). 99.9% requires multi-node failover, which is not in the current single-node architecture.

---

## Revenue Model Examples

### Flat Subscription

100 tenants × $10/mo = **$1,000/mo revenue** on ~$40/mo infrastructure.
Gross margin: ~96%. Scales linearly until you hit node capacity.

### Usage-Based

- $0.001 per message
- $0.0001 per 1,000 tokens

Average agent with 1,000 messages/mo and 500K tokens/mo = $1.05/tenant/mo. High volume at low price; suitable for high-frequency automation use cases.

### Hybrid (Recommended)

$5/mo base per tenant + overage:
- First 2,000 messages free
- $0.001 per message above 2,000
- First 1M tokens free
- $0.0001 per 1K tokens above 1M

Provides predictable base revenue with upside from power users.

---

## Security Compliance Notes

| Concern | Implementation |
|---------|---------------|
| Data isolation | Docker container + filesystem namespace per tenant |
| Secrets at rest | XChaCha20-Poly1305 encryption via vault |
| Secrets in transit | TLS via Caddy (Let's Encrypt or custom cert) |
| Audit logging | All API actions written to `audit_log` with actor, action, timestamp |
| Key rotation | `zcplatform rotate-key` — non-destructive, append-only key versioning |
| Auth | OTP-based (no password storage), short-lived Bearer tokens |

For GDPR/data residency: deploy regional nodes and provision customers to their local region. No cross-region data sync occurs in the default architecture.

---

## Customer Support Tools

| Task | Method |
|------|--------|
| View customer logs | `GET /api/tenants/{id}/logs?tail=200` |
| Check container status | `GET /api/tenants/{id}/status` |
| Restart stuck agent | `POST /api/tenants/{id}/restart` |
| Debug inside container | `POST /api/tenants/{id}/exec { "command": "..." }` |
| Review customer actions | `GET /api/monitoring/audit?tenant_id={id}` |
| Check resource usage | `GET /api/tenants/{id}/resources?range=24h` |

The `exec` endpoint requires Manager role or above. Scope its use to support staff; document its use in your internal runbook. All exec calls are written to the audit log.
