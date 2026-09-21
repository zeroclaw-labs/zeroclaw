---
id: ADR-017
title: Inbound authentication resolves canonical principals before authorization
date: 2026-08-14
status: proposed
relates-to:
  - ADR-010
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7141
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8289
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8290
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8076
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7142
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6996
  - https://github.com/zeroclaw-labs/zeroclaw/pull/8063
  - https://github.com/zeroclaw-labs/zeroclaw/pull/8272
  - crates/zeroclaw-api/src/principal.rs
  - crates/zeroclaw-api/src/grants.rs
  - crates/zeroclaw-runtime/src/security/auth_provider.rs
  - crates/zeroclaw-runtime/src/security/principal_resolver.rs
  - crates/zeroclaw-runtime/src/rpc/dispatch.rs
  - crates/zeroclaw-runtime/src/rpc/wss.rs
  - crates/zeroclaw-memory/src/sqlite_permissions.rs
---

# ADR-017: Inbound Authentication Resolves Canonical Principals Before Authorization

## Context

ZeroClaw has several ways for outside actors to reach the daemon: local RPC, remote WSS, the web gateway, CLI and Zerocode clients, channels, and future agent-to-agent paths. Before accepted RFC [#7141](https://github.com/zeroclaw-labs/zeroclaw/issues/7141), those surfaces did not share one enforced answer to "who is acting?" and "what may this actor do?".

TLS and reconnect tokens are not enough. TLS can protect a connection without proving that the connected client has ZeroClaw authority, and the existing `tui_id` / `tui_sig` mechanism is reconnect continuity rather than independent identity proof. A remote WSS client, a browser user, a local process, a channel sender, and a service client all need to converge on the same authorization model before privileged runtime work begins.

The risk is not only missing authentication on one route. The larger risk is parallel security systems: native pairing tokens, peer credentials, SSH keys, passwords, OIDC subjects, channel senders, and service credentials each inventing their own user identity, role vocabulary, ownership keys, and fallback behavior. If that happens, one provider's `admin` claim, one local username, or one reused subject string can accidentally bypass the policy that another surface depends on.

Accepted RFC #7141 resolves the architectural direction. It requires providers to verify credentials, a shared resolver to map verified identities into canonical ZeroClaw principals and permission-profile grants, and runtime surfaces to consume those resolved principals rather than reinterpreting credentials themselves. Implementation sequencing belongs to [#8289](https://github.com/zeroclaw-labs/zeroclaw/issues/8289), with detailed session, memory, and per-sender authorization work tracked by [#8290](https://github.com/zeroclaw-labs/zeroclaw/issues/8290). This ADR records the durable decision without claiming the rollout is complete.

## Decision

### Split inbound identity into verification, resolution, and enforcement

Inbound authentication has three separate stages:

1. An `AuthProvider` verifies one credential and returns an authenticated identity with provider provenance, subject, actor kind, claims or local roster binding, expiry, and revalidation metadata.
2. One shared ZeroClaw resolver maps that authenticated identity to a canonical `PrincipalId` and the current permission-profile grants.
3. Dispatch, gateway handlers, sessions, memory, tools, configuration, and other privileged runtime surfaces enforce the resolved principal and grants.

The Rust implementation may combine types internally, but the authority boundaries remain separate. Providers verify identity. They do not mint ZeroClaw runtime grants, reinterpret IdP roles as permissions, or bypass the shared resolver.

### Use canonical principals that cannot collide by string accident

A principal is ZeroClaw's durable identity for the actor after authentication succeeds. Principal identity is globally unambiguous within a deployment.

OIDC human identities are keyed by validated issuer and `sub`, not by `sub` alone. A provider alias may appear in audit output, but it cannot make equal subject strings from different issuers represent the same actor.

Local identities map through explicit roster configuration. Different local credentials may resolve to the same principal only because configuration links them. A roster display rename must preserve a stable principal identifier or atomically migrate the owned session, memory, approval, and audit records.

`client_credentials` resolves to a service principal, not a human user. Service principals must not inherit browser sessions, interactive-user assumptions, or private human memory because claims have similar names.

Native pairing remains a compatibility path. Unless a later explicit token-to-principal mapping is configured, a shared pairing token resolves to the shared operator rather than a named human.

String equality across providers never links accounts. Cross-provider account linking requires explicit configuration and an auditable migration path.

### Make permission profiles the only runtime grant vocabulary

Permission profiles are the only runtime authorization vocabulary. Raw provider claims, IdP groups, local usernames, channel senders, or service-client names grant nothing until the shared resolver maps them to ZeroClaw permission profiles.

When one identity maps to multiple permission profiles, the initial resolver combines explicit grants through deterministic union. Fine-grained selectors compose by intersection with the relevant resource grant. Empty selector lists grant no instances, and broad access requires an explicit wildcard, all-selector, or administrator grant.

Every privileged RPC, gateway, tool, configuration, session, and memory operation is classified and denied by default. A route or method added without classification is not implicitly allowed. Administrative access does not erase ownership; cross-principal access requires an explicit administrative grant and produces an audit event.

Explicit deny semantics, provider-specific policy languages, broader policy administration APIs, and different precedence rules are separate decisions. They do not arrive as side effects of this ADR.

### Keep authentication and grants live enough to revoke

`initialize` is the RPC authentication handshake. Only the minimum methods needed to complete authentication run before a principal is bound. Every privileged method rejects unauthenticated callers.

Credential routing is explicit. When a credential format does not securely identify its provider, the handshake identifies the intended provider alias. An opaque bearer is not sprayed across every introspection endpoint, and failure under the selected method or provider cannot fall back to shared-operator access or broader authority.

Established connections do not keep indefinite authorization snapshots. Before the next privileged operation after an authorization-policy generation changes, the runtime re-resolves effective grants from the current permission-profile and claim-mapping configuration. Removing or narrowing a profile, mapping, roster entry, provider, or pairing token must affect established authorization at the documented generation or revalidation boundary rather than waiting for reconnect or daemon restart.

Credential expiry and provider revocation apply to established connections within the provider's real guarantees. The canonical `Principal`, ownership records, logs, and audit records remain non-secret. If live revalidation needs bearer material or an equivalent secret handle, the provider keeps only that minimum material in a provider-owned, connection-scoped revalidation context through the supported secret-storage boundary. Ordinary runtime consumers cannot read it, and it is destroyed on disconnect, credential expiry, provider removal, or revocation.

OIDC token purpose is explicit. Browser Authorization Code with PKCE validates an ID token as evidence of the authentication event and establishes a ZeroClaw session. API and RPC bearer paths accept only an access token intended for ZeroClaw. Device Authorization Grant covers browserless human clients, and `client_credentials` covers service principals. Offline JWT validation is bounded by token expiry and a configured maximum authentication lifetime; deployments that need faster revocation must use introspection, back-channel revocation, or another live authority.

### Enforce principal ownership at session and memory storage boundaries

Every authenticated session is owned by a canonical principal. Session lookup, approval, attachment, resume, mutation, and deletion must agree on that owner before the operation succeeds. Unknown, conflicting, or legacy-unowned records fail closed for scoped principals unless a documented migration maps them to the shared-operator principal.

Private memory is keyed by principal and agent and is visible only to that principal unless an explicit administrative grant permits access. Shared agent memory is a separate scope and is visible only when enabled and granted.

Principal ownership is an additional storage dimension; it does not replace agent, session, namespace, or tenant dimensions. Private-memory reads, writes, upserts, and deletes include principal ownership in the atomic storage predicate. A pre-check followed by a broader mutation, or a read followed by post-filtering, is not enough.

Internally initiated work must not choose an owner by accident. Scheduled, SOP-initiated, or other non-interactive runs that write owned session or memory state need an explicit principal rule before the first implementation slice depends on a default.

### Keep provider and transport implementations separate from authorization authority

All inbound surfaces consume the same principal and permission-profile model, but they do not need the same transport implementation. Local RPC may use peer credentials, remote WSS may use native pairing or OIDC, the web gateway authenticates at the router boundary, and CLI or Zerocode clients expose provider-appropriate login flows.

Native pairing and local peer credentials are required compatibility paths for the first enforcement rollout so remote WSS authentication does not ship in an enforced-but-unusable state. OIDC is the first required external provider. Local password browser login, SSH-key challenge-response, mTLS identity, SCIM-style provisioning, channel identity, and agent-to-agent identity are compatible extensions, not closure requirements for the first OIDC delivery unless a later decision promotes them.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- the canonical provider registry, authenticated-identity contract, principal identity model, permission-profile vocabulary, shared resolver, explicit credential routing, and generation-aware grant re-resolution ship together with tests for namespacing, unmapped identities, selector default-deny behavior, and provider fallback refusal;
- native pairing, local peer credentials, RPC dispatch, WSS startup validation, and reconnect continuity enforce authentication by construction without creating an enforced-but-unusable remote interval;
- OIDC JWT/JWKS, introspection, browser Authorization Code with PKCE, browserless Device Authorization Grant, service `client_credentials`, token-purpose separation, bounded revalidation, and gateway principal propagation work through the same resolver and fail closed when credentials, provider state, or revalidation context are unavailable;
- sessions, approvals, attachments, resumes, deletes, private-memory reads, and private-memory mutations enforce principal ownership at the storage boundary, including documented treatment for legacy records and an explicit internal-origin owner rule for scheduled or SOP-initiated work;
- existing remote WSS and Nevis users have documented and tested migration paths, and local-only recovery can repair lockout without restoring unauthenticated remote access;
- documentation explains identities, actor kinds, claim mapping, permission profiles, grants, private versus shared memory, provider guarantees, migration, recovery, and standards-aligned handshake vocabulary where applicable; and
- the rollout lands as independently reviewable and reversible security boundaries tracked by #8289 and #8290 rather than one combined provider, dispatcher, gateway, storage, migration, and documentation branch.

The behavior-neutral `Principal` / `AuthProvider` seam and contract-test groundwork have landed through #8063 and #8272. Those slices are groundwork, not proof that this ADR's full gates are complete.

## Consequences

Positive consequences:

- New inbound providers can be added without each provider creating its own authorization system.
- A future reviewer can ask one question at every privileged boundary: what resolved principal and grants are being enforced here?
- OIDC, local credentials, native pairing, and service credentials cannot collide merely because they share a string.
- Revocation and permission-profile changes have documented live boundaries instead of waiting indefinitely for reconnect or restart.
- Session and private-memory isolation become storage properties rather than best-effort filtering above the store.

Negative consequences:

- This is a security-breaking change for unauthenticated remote WSS behavior, so native compatibility and recovery must ship with the enforcement path.
- The shared resolver, profile vocabulary, and generation-aware revalidation add more infrastructure than a provider-local role check would.
- OIDC implementation must preserve token purpose, discovery, JWKS refresh, introspection, and standards-aligned metadata boundaries instead of accepting every valid-looking token.
- Principal ownership touches session and memory storage, so migration and rollback need more care than an edge-only authentication check.
- Some useful extensions, including password login, SSH-key authentication, mTLS, SCIM, channel identity, and agent-to-agent identity, remain separate follow-up work.

## References

- [RFC #7141: Pluggable inbound authentication and canonical principals](https://github.com/zeroclaw-labs/zeroclaw/issues/7141)
- [Tracker #8289: OIDC milestone: canonical principals and inbound authentication](https://github.com/zeroclaw-labs/zeroclaw/issues/8289)
- [Tracker #8290: multi-user milestone: per-principal isolation + per-sender authz](https://github.com/zeroclaw-labs/zeroclaw/issues/8290)
- [Issue #8076: local username/password provider](https://github.com/zeroclaw-labs/zeroclaw/issues/8076)
- [Issue #7142: runtime-owned security decision pipeline](https://github.com/zeroclaw-labs/zeroclaw/issues/7142)
- [Issue #6996: operating-system sandbox policy](https://github.com/zeroclaw-labs/zeroclaw/issues/6996)
- [PR #8063: `Principal` and `AuthProvider` seam](https://github.com/zeroclaw-labs/zeroclaw/pull/8063)
- [PR #8272: `Principal` and `AuthOutcome` contract tests](https://github.com/zeroclaw-labs/zeroclaw/pull/8272)
- [ADR-010: Memory authority boundaries](./ADR-010-memory-authority-boundaries.md)
- [Security model](../../security/model.md)
- `crates/zeroclaw-api/src/principal.rs`
- `crates/zeroclaw-api/src/grants.rs`
- `crates/zeroclaw-runtime/src/security/auth_provider.rs`
- `crates/zeroclaw-runtime/src/security/principal_resolver.rs`
- `crates/zeroclaw-runtime/src/rpc/dispatch.rs`
- `crates/zeroclaw-runtime/src/rpc/wss.rs`
- `crates/zeroclaw-memory/src/sqlite_permissions.rs`
