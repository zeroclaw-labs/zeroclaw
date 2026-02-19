# Authentication Integration Research Report: OpenClaw / ClawSuite

Date: February 12, 2026
Scope: Product-grade authentication options for OpenClaw gateway + ClawSuite UI

## Executive Summary

OpenClaw currently uses shared gateway secrets (password/token), optional Tailscale trust boundaries, and short-lived pairing codes. This is workable for single-admin or private networks, but it is not product-grade for multi-user SaaS-like access because identity is not per-user, MFA/RBAC are not first-class, and credential handling is partially client-side (`localStorage` in current docs).

For OpenClaw + ClawSuite, **WorkOS AuthKit** is the strongest top pick if you optimize for speed to production, polished login UX, enterprise SSO, MFA, and lower operational burden. It now has explicit **TanStack Start** support and mature Node SDKs.

If self-hosting and data/control ownership are top priorities, **Better Auth** is the best runner-up: TypeScript-native, modern developer ergonomics, and direct TanStack Start integration, but with more implementation responsibility and less enterprise depth than WorkOS today.

## Current Auth Analysis (OpenClaw)

### What OpenClaw documents show now

- OpenClaw gateway defaults to auth mode `password`, with WebSocket client auth passed via `connect.params.auth`.
- OpenClaw Control docs state the gateway password is stored in browser `localStorage` for reconnects.
- Pairing codes are 8-character codes, valid for 1 hour, for linking local clients to cloud instances.
- Cloud documentation references Tailscale headers / allowlists as a network trust control.

### Implications for product-grade multi-user auth

- **Identity model gap**: shared secret auth is gateway-centric, not user-centric.
- **MFA gap**: no first-class MFA policy engine.
- **RBAC gap**: permissions are not tied to explicit user/org role claims.
- **Session lifecycle gap**: no centralized session revocation, conditional access, or admin-friendly login portal.
- **Security posture gap**: client-stored long-lived secrets increase leakage risk versus HttpOnly session cookies + short-lived access tokens.

### Target state for OpenClaw/ClawSuite

- OIDC-based login with polished hosted or embedded login journey.
- Per-user identity + organization/workspace membership.
- MFA policy enforcement (at minimum admin roles; ideally org configurable).
- RBAC enforced in gateway API and WebSocket command authorization.
- Auditable sessions, revocation, and enterprise SSO path.

## Option Comparison Matrix

Pricing and activity are snapshots as of **February 12, 2026** from public pages; vendors can change terms quickly.

| Option | Self-hosted vs Cloud | Security Model | Integration (ClawSuite React + TanStack Start + Node gateway) | Multi-tenant | SSO | Maintenance Burden | Cost Snapshot | GitHub / Recent Update Snapshot |
|---|---|---|---|---|---|---|---|---|
| **WorkOS AuthKit** | Cloud-managed auth service | Managed identity + sessions, OIDC/SAML, MFA, org primitives; WorkOS compliance posture (SOC2/ISO/HIPAA/DPA claims) | **Strong**: official AuthKit packages for React and TanStack Start, Node SDKs; fastest path to polished login | **Strong** (Organizations) | **Strong** (enterprise SSO add-on) | **Low** (vendor-managed infra) | AuthKit active users: free up to 1M, then $2,500 per extra 1M. SSO add-on: $125/connection/mo. Custom domain: $99/mo. | Product is mostly SaaS; SDK repos are active (e.g., `authkit-react`, `authkit-tanstack-start` release activity in Jan 2026). |
| **Better Auth** | Primarily self-host/in-app (TypeScript library); managed infrastructure in waitlist/free-trial stage | App-owned auth server/data model; plugins for org, 2FA, passkeys, SSO | **Strong** for this stack: official TanStack Start integration docs + Node/TS-first architecture | **Good** (organization plugin) | **Medium** (SSO/SAML available via plugins; maturity evolving) | **Medium** (you own infra + upgrades) | OSS (MIT) is free; managed infra has 14-day trial, no clear public long-term price listed | `better-auth/better-auth` ~26k stars; frequent releases (v1.4.18 on Jan 29, 2026). |
| **Authelia** | Self-hosted, typically behind reverse proxy | Access control + 2FA at edge, can provide OIDC for apps | **Medium**: good for perimeter auth; deeper app-level identity/RBAC requires additional app integration work | **Limited/Medium** (policy/domain based, not rich tenant product model) | **Medium** (OIDC provider; enterprise federation breadth narrower than Keycloak/WorkOS) | **Medium-High** (ops for proxy/auth stack) | Open source, no license fee; pay infra/ops/support | `authelia/authelia` ~26k stars; release v4.39.15 on Nov 29, 2025. |
| **Keycloak** | Self-hosted (or vendor-supported distributions) | Dedicated IAM server, realms/clients/roles/groups, broad federation, strong MFA options | **Medium**: standards-compliant OIDC/SAML integration works, but operational complexity is significant | **Strong** (realms + role model) | **Strong** | **High** (Java service lifecycle, clustering/DB/ops overhead) | OSS free; commercial support available via vendors (e.g., Red Hat build) | `keycloak/keycloak` ~32.2k stars; active 26.x release stream (26.4.0 around Jan 2026). |
| **SuperTokens** | Cloud-managed or self-hosted open-source core | Session/user management focused auth platform; MFA, RBAC, SSO add-ons | **Strong**: React + Node SDKs, practical for custom UI and API-first flows | **Good** (multi-tenancy support available) | **Good** (enterprise add-on) | **Medium** (low on cloud, medium-high self-host) | Cloud: free to 5k MAU then $0.02/MAU. Enterprise from $900/mo. SSO from $500/mo. MFA from $200/mo. | `supertokens/supertokens-core` ~14.7k stars; v11.2.0 on Oct 28, 2025. |
| **Lucia Auth** | Self-hosted library, no managed service | Lightweight auth/session primitives; you build most policy/product layers | **Medium-Low**: TS-friendly but a lot must be custom-built for MFA/RBAC/SSO polish | **Limited** (custom build) | **Limited** (custom build) | **Medium-High** (high app ownership) | Free OSS | `lucia-auth/lucia` ~10.4k stars; last update July 13, 2025; v3 docs indicate reduced update cadence after March 2025. |
| **Auth.js / NextAuth** | Library-first OSS; often used with Next.js | OAuth/social and session patterns; many adapters/providers | **Medium** for TanStack Start: no first-class TanStack Start integration; possible via `@auth/core` with custom wiring | **Limited** (custom) | **Medium** (OAuth providers strong; enterprise SAML often custom/third-party) | **Medium** | Free OSS | `nextauthjs/next-auth` ~28k stars; active, with v5 beta and package releases in late 2025. |

## Top Recommendation: WorkOS AuthKit

### Why this is the best fit now

- **Fastest route to product-grade UX**: polished login, enterprise-ready auth flows, MFA, and org features without building auth UI/systems from scratch.
- **Strong stack compatibility**: explicit AuthKit support for TanStack Start and React plus mature Node SDK support for gateway integration.
- **Enterprise readiness**: SSO, directory-centric features, compliance posture, and admin workflows reduce future re-platform risk.
- **Lower maintenance**: avoids operating your own IAM cluster (Keycloak) or assembling many plugins and bespoke controls (Lucia/DIY).

### Key trade-off

- You accept a managed-cloud dependency and per-feature pricing (especially SSO connection pricing). If full self-host sovereignty is non-negotiable, Better Auth or Keycloak become stronger.

## Runner-Up Alternative: Better Auth

Better Auth is the best alternative when self-hosting and TypeScript-native control are priorities.

- **Pros**: modern TS ergonomics, direct TanStack Start support, plugin ecosystem (2FA/passkeys/org/SSO), fast development velocity.
- **Cons**: more implementation and security ownership on your team; enterprise SSO depth and long-term managed pricing transparency are less predictable than WorkOS.

## Step-by-Step Implementation Plan (Top Pick: WorkOS)

### Phase 0: Auth domain design (1 week)

1. Define principal model:
   - `user`, `organization`, `membership`, `role` (`owner`, `admin`, `member`, `viewer`).
2. Define permission matrix for gateway actions:
   - Example: `agent:run`, `agent:kill`, `session:read`, `admin:settings`.
3. Define token claims contract consumed by gateway:
   - `sub`, `org_id`, `role`, `permissions`, `iat`, `exp`, `iss`, `aud`.

### Phase 1: ClawSuite login integration (1-2 weeks)

1. Add AuthKit TanStack Start integration for route/session handling.
2. Replace legacy password/token entry UI with login + org selection.
3. Move away from persistent gateway password in `localStorage`.
4. Ensure cookies are `HttpOnly`, `Secure`, `SameSite` appropriate to deployment.

### Phase 2: Gateway token validation + WebSocket auth (1-2 weeks)

1. Add gateway auth middleware to validate WorkOS-backed tokens (via issuer/JWKS).
2. Update WebSocket handshake parsing:
   - accept bearer token in `connect.params.auth` (or auth header equivalent).
3. Enforce strict token checks:
   - signature, audience, issuer, expiration, not-before, clock skew.
4. Attach resolved principal/permissions to WS session context.

### Phase 3: RBAC enforcement (1 week)

1. Add command-level authorization checks in gateway handlers.
2. Implement deny-by-default behavior for unknown permissions.
3. Log authorization decisions for auditability.

### Phase 4: MFA + enterprise SSO rollout (1 week)

1. Enforce MFA for privileged roles first (`owner`, `admin`).
2. Enable SSO per organization as an opt-in enterprise feature.
3. Document break-glass recovery flows (admin lockout, lost MFA).

### Phase 5: Migration from legacy auth (1-2 weeks)

1. Introduce dual mode flag (e.g., `AUTH_MODE=legacy|oidc`) in gateway.
2. Pilot with internal orgs first.
3. Run dual-stack period, monitor auth failures and WS reconnect behavior.
4. Decommission password/token-only mode once error rate and support load are stable.

### Phase 6: Operations and security hardening (ongoing)

1. Centralize auth/audit logs.
2. Add alerting for suspicious events (MFA bypass attempts, repeated token failures).
3. Add load tests for login and WS reconnect storms.
4. Add incident runbooks for IdP outage or JWKS fetch failures.

## Potential Issues & Mitigations

- **Managed IdP outage risk (WorkOS dependency)**
  - Mitigation: cache JWKS keys, allow short grace for already-issued tokens, maintain emergency local admin path with strict controls.
- **Token misuse in browser**
  - Mitigation: avoid long-lived tokens in `localStorage`; use short-lived tokens + HttpOnly cookie sessions.
- **WebSocket reconnect churn with expiring tokens**
  - Mitigation: proactive token refresh window and retry/jitter logic before expiry.
- **Authorization drift between UI and gateway**
  - Mitigation: enforce RBAC server-side as source of truth; UI only hints.
- **SSO cost growth by enterprise tenant count**
  - Mitigation: package SSO as paid tier; track per-connection economics before broad rollout.
- **Migration friction from existing pairing/token flows**
  - Mitigation: phased rollout with dual auth mode and precise telemetry.

## Next Actions

1. Decide between **managed-first (WorkOS)** and **self-host-first (Better Auth)** strategy at product level.
2. Approve target RBAC model (`owner/admin/member/viewer`) and permission list for gateway commands.
3. Build a 2-week proof-of-concept branch with WorkOS AuthKit in ClawSuite and JWT validation in OpenClaw gateway.
4. Define rollout success criteria:
   - login success rate, WS reconnect success rate, auth-related support tickets, and mean time to revoke access.
5. If data sovereignty becomes a blocker during PoC, pivot PoC scope to Better Auth before full rollout.

## Sources

### OpenClaw

- https://docs.openclaw.ai/gateway/authentication
- https://docs.openclaw.ai/control/gateway-connection
- https://docs.openclaw.ai/cloud/pairing
- https://docs.openclaw.ai/cloud/tailscale-deployment
- https://github.com/openclaw/openclaw

### WorkOS AuthKit

- https://workos.com/docs/authkit/overview
- https://workos.com/pricing
- https://workos.com/docs/sdks/node
- https://workos.com/docs/sdks/react
- https://workos.com/changelog
- https://workos.com/security
- https://github.com/workos/authkit-react
- https://github.com/workos/authkit-tanstack-start

### Better Auth

- https://www.better-auth.com/
- https://www.better-auth.com/docs/introduction
- https://www.better-auth.com/docs/integrations/tanstack
- https://www.better-auth.com/docs/plugins/sso
- https://www.better-auth.com/docs/plugins/organization
- https://www.better-auth.com/docs/plugins/two-factor
- https://www.better-auth.com/enterprise
- https://www.better-auth.com/infrastructure
- https://github.com/better-auth/better-auth

### Authelia

- https://www.authelia.com/overview/prologue/introduction/
- https://www.authelia.com/overview/prologue/security/
- https://www.authelia.com/integration/openid-connect/introduction/
- https://github.com/authelia/authelia
- https://github.com/authelia/authelia/releases

### Keycloak

- https://www.keycloak.org/
- https://www.keycloak.org/features
- https://www.keycloak.org/documentation
- https://www.keycloak.org/2025/10/keycloak-30k-github-stars
- https://github.com/keycloak/keycloak
- https://github.com/keycloak/keycloak/releases

### SuperTokens

- https://supertokens.com/docs
- https://supertokens.com/pricing
- https://github.com/supertokens/supertokens-core
- https://github.com/supertokens/supertokens-core/releases

### Lucia Auth

- https://lucia-auth.com/
- https://v3.lucia-auth.com/
- https://github.com/lucia-auth/lucia

### Auth.js / NextAuth

- https://authjs.dev/
- https://authjs.dev/reference
- https://authjs.dev/reference/overview
- https://github.com/nextauthjs/next-auth
- https://github.com/nextauthjs/next-auth/releases

