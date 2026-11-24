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
  - crates/zeroclaw-runtime/src/security/auth_provider/oidc.rs
  - crates/zeroclaw-runtime/src/security/auth_provider/enrollment.rs
  - crates/zeroclaw-runtime/src/security/cert_ledger.rs
  - crates/zeroclaw-memory/src/sqlite_permissions.rs
  - apps/zerocode/src/main.rs
  - ADR-013
---

# ADR-017: Inbound Authentication Resolves Canonical Principals Before Authorization

## Context

ZeroClaw has several ways for outside actors to reach the daemon: local RPC, remote WSS, the web gateway, CLI and Zerocode clients, channels, and future agent-to-agent paths. Before accepted RFC [#7141](https://github.com/zeroclaw-labs/zeroclaw/issues/7141), those surfaces did not share one enforced answer to "who is acting?" and "what may this actor do?".

TLS and reconnect tokens are not enough. TLS can protect a connection without proving that the connected client has ZeroClaw authority, and the existing `tui_id` / `tui_sig` mechanism is reconnect continuity rather than independent identity proof. A remote WSS client, a browser user, a local process, a channel sender, and a service client all need to converge on the same authorization model before privileged runtime work begins.

The risk is not only missing authentication on one route. The larger risk is parallel security systems: native pairing tokens, peer credentials, SSH keys, passwords, OIDC subjects, channel senders, and service credentials each inventing their own user identity, role vocabulary, ownership keys, and fallback behavior. If that happens, one provider's `admin` claim, one local username, or one reused subject string can accidentally bypass the policy that another surface depends on.

Accepted RFC #7141 resolves the architectural direction. It requires providers to verify credentials, a shared resolver to map verified identities into canonical ZeroClaw principals and permission-profile grants, and runtime surfaces to consume those resolved principals rather than reinterpreting credentials themselves. Implementation sequencing belongs to [#8289](https://github.com/zeroclaw-labs/zeroclaw/issues/8289), with detailed session, memory, and per-sender authorization work tracked by [#8290](https://github.com/zeroclaw-labs/zeroclaw/issues/8290). This ADR records the durable decision without claiming the rollout is complete.

Two gaps appeared once OIDC reached remote Zerocode clients. First, a device session cannot outlive one access token. The gateway discards the IdP's refresh token, and Zerocode presents the access token it received at login on every reconnect. A laptop that sleeps past the token's `exp` resumes to `-32010` and must restart and sign in again, even though the IdP session is still valid and the RPC layer already supports re-initializing a live connection and resuming by `tui_id` / `tui_sig`. Second, the JWKS verifier accepts only RFC 9068 access tokens, and the two IdPs required for v1.0 do not issue them. Okta custom authorization server access tokens carry no `typ` header and name the client in a vendor `cid` claim. Entra ID access tokens carry `typ: JWT`, name the token in `uti` rather than `jti`, and Entra publishes no RFC 7662 introspection endpoint to fall back to. Entra also derives a token's audience from the requested scope, while the device grant currently requests only `openid`.

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

A dashboard sign-in session is the browser's connection for the rule above. The gateway keeps the access token in memory as that session's revalidation context and never hands it to the browser, which holds only an opaque `HttpOnly`, `SameSite=Strict` cookie. The provider verifies the token at sign-in, every request checks the session again against the current authority (expiry, revalidation deadline, grants re-resolved at the current generation), and the token is held in a buffer that is wiped when the session ends: on sign-out, expiry (a periodic sweep ends expired sessions with no request arriving), a failed revalidation, provider removal, or eviction. Sockets and event streams a session opened close when it ends or loses the access they need, and when their gateway generation stops, so the next generation's authority decides again. Requests that change state carry a per-session CSRF token, and cookie-authenticated requests and WebSocket upgrades must come from the dashboard's own origin. Refresh tokens are neither relayed nor kept, so expiry means signing in again; sign-out is local to ZeroClaw; and service principals never hold a dashboard session. Dashboard sign-in is opt-in and leaves pairing in place.

### Keep device sessions alive through IdP refresh, held by the daemon

A device-bound client (one holding an active certificate in the issued-certificate ledger) may keep its OIDC principal authenticated for as long as the IdP agrees to refresh, without interactive sign-in. The browser dashboard is unchanged: it holds no refresh capability and expiry still means signing in again.

- **The daemon holds the refresh token; clients never do.** When a provider entry opts in to refresh and the IdP returns a refresh token from the device grant, the daemon stores it as a *refresh grant* rather than discarding it. The grant is sealed through the supported secret-storage boundary ([ADR-013](./ADR-013-key-source-authority.md)) and keyed by provider alias and ledger `device_id`, which survives certificate renewal. It also records the provider binding (issuer and client) and the principal (issuer and `sub`) it was issued for. One owner per refresh token is what makes rotating refresh tokens safe. Two Zerocode windows, a reconnect, and a timer on the same device all go through one grant, and no client-side copy can be replayed and trip the IdP's reuse detection.
- **A login binds its refresh token to exactly one new certificate.** The IdP returns the refresh token while the device grant or browser sign-in completes, before the certificate request that creates the `device_id`. So the token response's two halves pair the steps. The issuing authority holds the refresh token as a *pending grant*, in memory only, keyed by a hash of the access token from the same response. The pending grant also records that response's provider binding and principal, and it lives no longer than that access token, capped at the device-code lifetime. The certificate request already presents that access token as its bearer. When the certificate is issued, the authority claims the pending grant once, atomically with recording the certificate in the ledger. It binds the grant to the new `device_id` only if the bearer verifies as the same principal through the same provider binding. Two devices signing in as the same person therefore hold different access tokens and claim different pending grants. Issuer and `sub` alone could not tell those logins apart. A pending grant that is not claimed, because no certificate request follows or a restart intervenes, is wiped when it expires. The device then simply has no refresh grant until its next login, and no grant ever attaches to a certificate it was not issued with.
- **The device certificate authorizes the refresh; the IdP decides the principal.** A pre-authentication RPC method, admitted like `cert/renew`, accepts a refresh request only over mutual TLS from an active, unrevoked ledger certificate whose `device_id` owns the grant. The daemon calls the IdP's token endpoint with `grant_type=refresh_token` (RFC 6749 §6) and its own client authentication, and omits `scope`. A refresh cannot ask for more than the original grant, so omitting `scope` keeps that grant. Scopes an operator widens after sign-in apply from the next interactive login, rather than making every refresh fail. It verifies the new access token through the same `oidc.<alias>` provider and requires the same issuer and `sub` the grant recorded. Only then does it return the access token. The client presents that token to `initialize` exactly as today. `initialize` remains the only place a principal is bound, and the connection's existing same-principal binding check still applies. A refresh grant is never a bearer credential itself.
- **Refresh is serialized and durable, and deletion wins.** Refreshes of one grant are single-flight. A rotated refresh token is durably stored before the old one is discarded or the access token is released. A response without a new refresh token keeps the current one, as RFC 6749 permits. Single-flight orders refreshes only against each other, so each grant also carries a version that every deletion path invalidates. A refresh commits its result, and releases the access token, only if the grant still exists at the version it started from. A refresh still waiting on the IdP when sign-out, certificate revocation, or provider removal deletes the grant stores nothing and returns nothing. It also revokes any refresh token it received, where the issuer supports RFC 7009.
- **Every refusal fails closed and deletes the grant.** The grant is wiped when the IdP answers `invalid_grant`, the refreshed token fails verification or names another principal, the resolver no longer entitles the principal, the provider entry is removed or repointed, the certificate is revoked or expires without renewal, or the user signs out. The client then falls back to one interactive sign-in. Sign-out also revokes the refresh token at the IdP (RFC 7009) when the issuer advertises a revocation endpoint.
- **An unknown refresh outcome is retried once, never looped.** A refresh that provably never reached the IdP (resolution, connection, or TLS failure before the request was sent) leaves the grant as it was, and the client may retry. Other failures leave the outcome unknown: a request sent with no response, or the daemon stopping after it sent the request but before the result was stored. A durable in-flight marker, written before the request is sent, is how a restarted daemon detects the second case. The IdP may have rotated the refresh token, and retrying with the old one can trip reuse detection and revoke the token family. So an unknown outcome marks the grant uncertain. The next refresh is attempted once with the stored token. If the IdP answers `invalid_grant`, the grant is deleted and the user signs in again. Either way there is no retry loop. The family loss this risks costs one interactive sign-in, never access.
- **The IdP sets session length; ZeroClaw may only shorten it.** No vendor-neutral claim advertises refresh-token lifetime, so the daemon does not predict it. It attempts refresh and treats refusal as the end of the session. ZeroClaw never adds `offline_access` or any other scope implicitly, because on some IdPs it changes the session's lifetime semantics (Keycloak issues an offline token that outlives the SSO session). Requested scopes are explicit per provider.
- **Clients refresh ahead of expiry and on resume.** Zerocode refreshes before the access token's `exp` and re-initializes on the open connection. After sleep, it refreshes before reconnecting and then resumes with its `tui_id` / `tui_sig`. A mid-session `-32010` triggers at most one refresh-and-reinitialize attempt before it surfaces to the user. The bearer the connection presents is read from one shared, refreshable source, not snapshotted at startup.

This bounds revocation better than long-lived access tokens do. JWKS-validated tokens can stay short-lived, and each refresh is a live check with the IdP. Where the IdP refuses refresh for a disabled user, disabling that user takes effect within one access-token lifetime, even on issuers without introspection. That refusal is issuer behavior, not a guarantee of the standards, so it is part of each required provider's real-tenant verification.

The implementation lives with the inbound auth-provider owner, which the roadmap places in the planned `zeroclaw-kernel` extraction. While that owner remains in the `zeroclaw-runtime` holding crate, every implementing pull request that touches it needs its own recorded [ADR-016](./ADR-016-holding-crate-exceptions.md) exception before it merges. The existing OIDC exception excludes refresh-token storage and requires review before OAuth flows, client-attribution rules, or retained credential state widen. Certificate issuance and the HTTP routes stay in `zeroclaw-gateway`; client login, the refresh trigger, and credential storage stay in Zerocode.

### Accept required IdPs' access tokens without weakening token purpose

Okta and Entra ID are required v1.0 providers. The verifier accepts their real access-token formats without accepting anything it would refuse today for a different reason. RFC 9068 `typ: at+jwt` remains the preferred discriminator between access and ID tokens. An issuer that does not set it may still be used for JWKS validation only when purpose is proven another way. An ID token's `aud` must contain the OAuth client it was issued to, and it may also list other audiences (OIDC Core §2). So excluding the API identifier from every client is not enough on its own. An issuer may issue an ID token whose `aud` lists both the signing-in client and ZeroClaw's API identifier, and a device-grant ID token carries no `nonce` to give it away. A token without the RFC 9068 type is therefore accepted only when both of these hold. First, its `aud` is exactly the configured `audience`: one string, or an array holding only that value. Second, the provider entry proves that `audience` is not a client: it differs from the entry's own `client_id` and from every other client configured for the same issuer. An ID token always names its client in `aud`, so a single-valued `aud` equal to an identifier no client uses cannot be one. Anything else is refused, as today. Okta and Entra ID both issue access tokens with a single `aud`, so neither loses anything. One residual cannot be checked locally: no client at the IdP may sign users in as the API registration itself. The documentation states it as a deployment requirement. In practice this means a separate API registration for Entra ID and a custom authorization server audience for Okta.

Vendor claim spellings are accepted only through named, documented, and tested compatibility rules, never by guessing. This follows the precedent of the existing Keycloak `azp` rule. Okta `cid` names the client. Entra `uti` stands in for `jti`, and `azp` (v2.0) names the client. Entra tokens must be v2.0 (`accessTokenAcceptedVersion = 2`), so their `iss` matches the v2.0 discovery issuer. Entra's group-overage form (`_claim_names` in place of `groups`) maps to no profile and fails closed. ZeroClaw does not call Microsoft Graph to expand it, and the documentation recommends app roles for large tenants. Device-grant scopes come from the provider entry, so Entra can request the ZeroClaw API scope that sets the token's audience.

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
- OIDC JWT/JWKS, introspection, browser Authorization Code with PKCE including dashboard sign-in, sign-out and re-authentication on expiry, browserless Device Authorization Grant, service `client_credentials`, token-purpose separation, bounded revalidation, and gateway principal propagation work through the same resolver and fail closed when credentials, provider state, or revalidation context are unavailable;
- sessions, approvals, attachments, resumes, deletes, private-memory reads, and private-memory mutations enforce principal ownership at the storage boundary, including documented treatment for legacy records and an explicit internal-origin owner rule for scheduled or SOP-initiated work;
- existing remote WSS and Nevis users have documented and tested migration paths, and local-only recovery can repair lockout without restoring unauthenticated remote access;
- daemon-held refresh grants keep a device-bound Zerocode session authenticated across access-token expiry and across sleep and resume. Tests cover rotation, single-flight refresh, durable rotation before release, principal mismatch, `invalid_grant`, certificate revocation, provider repointing, and sign-out deletion. They also cover first binding: two devices that sign in as the same person concurrently each end up with only their own grant, and an unclaimed pending grant expires without attaching to any certificate. A refresh racing a deletion stores and releases nothing. An unknown outcome leads to exactly one retry and no loop. The dashboard still holds no refresh capability;
- Okta (custom authorization server) and Entra ID (v2.0 tokens, no introspection) each pass the device grant, JWKS verification, refresh, and profile mapping against a real tenant. ID tokens, refresh tokens, and access tokens issued to another audience are each refused on the bearer path. An untyped ID token whose `aud` also lists the API audience is refused. Each provider refuses refresh for a disabled user. The per-issuer compatibility rules have tests;
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
- Device sessions last as long as the IdP allows, and each refresh rechecks the user with the IdP, so short access-token lifetimes cost nothing in usability.
- Required IdPs are supported by standards-based rules plus named compatibility spellings, not by vendor-specific code paths.

Negative consequences:

- This is a security-breaking change for unauthenticated remote WSS behavior, so native compatibility and recovery must ship with the enforcement path.
- The shared resolver, profile vocabulary, and generation-aware revalidation add more infrastructure than a provider-local role check would.
- OIDC implementation must preserve token purpose, discovery, JWKS refresh, introspection, and standards-aligned metadata boundaries instead of accepting every valid-looking token.
- Principal ownership touches session and memory storage, so migration and rollback need more care than an edge-only authentication check.
- Some useful extensions, including password login, SSH-key authentication, mTLS, SCIM, channel identity, and agent-to-agent identity, remain separate follow-up work.
- The daemon becomes a long-term holder of IdP refresh tokens. A compromised daemon secret store, or a stolen device key while its certificate is unrevoked, yields sessions until the IdP refuses refresh. Certificate revocation and IdP-side session revocation are the containment paths, and both must be documented for operators.
- Accepting tokens without `typ: at+jwt` makes token-purpose separation depend on an audience invariant that is partly a deployment requirement at the IdP, not something ZeroClaw can fully verify.
- Entra ID's lack of introspection means its revocation guarantee is bounded by access-token lifetime plus the refresh interval. There is no live per-request check.

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
- [ADR-013: Key-source authority](./ADR-013-key-source-authority.md)
- [RFC 6749 §6: Refreshing an access token](https://www.rfc-editor.org/rfc/rfc6749#section-6)
- [RFC 7009: OAuth 2.0 token revocation](https://www.rfc-editor.org/rfc/rfc7009)
- [RFC 8628: OAuth 2.0 device authorization grant](https://www.rfc-editor.org/rfc/rfc8628)
- [RFC 9068: JWT profile for OAuth 2.0 access tokens](https://www.rfc-editor.org/rfc/rfc9068)
- [RFC 9700: OAuth 2.0 security best current practice](https://www.rfc-editor.org/rfc/rfc9700) (refresh-token rotation and reuse detection)
- [OpenID Connect Core 1.0](https://openid.net/specs/openid-connect-core-1_0.html) (ID-token `aud`, `offline_access`)
- [Microsoft Entra access token claims reference](https://learn.microsoft.com/en-us/entra/identity-platform/access-token-claims-reference)
- [Okta OpenID Connect and OAuth 2.0 overview](https://developer.okta.com/docs/api/openapi/okta-oauth/guides/overview/)
- [Security model](../../security/model.md)
- `crates/zeroclaw-api/src/principal.rs`
- `crates/zeroclaw-api/src/grants.rs`
- `crates/zeroclaw-runtime/src/security/auth_provider.rs`
- `crates/zeroclaw-runtime/src/security/principal_resolver.rs`
- `crates/zeroclaw-runtime/src/rpc/dispatch.rs`
- `crates/zeroclaw-runtime/src/rpc/wss.rs`
- `crates/zeroclaw-runtime/src/security/auth_provider/oidc.rs`
- `crates/zeroclaw-runtime/src/security/auth_provider/enrollment.rs`
- `crates/zeroclaw-runtime/src/security/cert_ledger.rs`
- `apps/zerocode/src/main.rs`
- `crates/zeroclaw-memory/src/sqlite_permissions.rs`
