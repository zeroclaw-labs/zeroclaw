# Authentication & multi-user access

By default a ZeroClaw daemon trusts its local socket: the Unix endpoint is
`0o600`, so anything that can open it is the operator. That model is right for
a single-user install and wrong the moment a second person, a remote TUI, or a
service account needs in. This page covers the authentication layer that
closes that gap: pluggable auth providers, named principals, permission
profiles, per-principal session isolation, and audit attribution.

## The model in one pass

Every RPC connection performs an `initialize` handshake. When any auth
provider is configured, the daemon resolves a **credential** from that
handshake into a **principal** before binding any identity:

1. An explicit bearer token (`auth_token` in `initialize`) wins if present.
2. Otherwise an SSH challenge signature (`auth_username` + `auth_signature`
   over a nonce from `auth/challenge`) is tried.
3. Otherwise the transport-intrinsic credential is presented: on the local
   socket that is the peer's Unix uid.

The resolved principal carries a **permission profile**: a named grant set
that says exactly which resources and verbs it holds. Every subsequent RPC
method call is checked against those grants at dispatch. An unlisted resource
is denied.

Failure behavior is asymmetric by design:

- **Local socket, no resolvable credential**: legacy trust. The socket file
  mode is the credential. Existing single-user installs keep working
  unchanged.
- **WSS (remote), no resolvable credential**: default-deny. A remote
  connection must authenticate once any provider is configured, and the
  daemon refuses to expose WSS at all unless a remote-capable credential path
  (OIDC issuer, user roster, or pairing token) exists.

## Auth providers

| Provider | Credential | Configured by |
|---|---|---|
| `peercred` | Unix peer uid on the local socket | Always on; `[users.<name>].uid` upgrades a uid to a named principal |
| `native` | Gateway pairing bearer token | Gateway pairing |
| `ssh-key` | Ed25519 / ECDSA-P256 signature over a server nonce | `[users.<name>].authorized_keys` |
| `oidc.<alias>` | JWT or opaque bearer from your IdP | `[oidc.<alias>]` |

### peercred: named local users

Without a roster entry, the daemon's own uid gets the trusted shared-operator
path and every other uid is denied. Adding `uid` to a `[users.<name>]` entry
authenticates that uid as the named principal with its profile's grants,
which is how you give a second Unix account scoped access to the same daemon.

### ssh-key: challenge-response for remote users

The client calls `auth/challenge` to receive a single-use 32-byte nonce,
signs it with a key listed in the user's `authorized_keys` (OpenSSH format,
`ssh-ed25519` or `ecdsa-sha2-nistp256`), and presents the username and
signature in `initialize`. The nonce is connection-scoped and consumed by the
next attempt whether it succeeds or fails, so a failed attempt cannot replay
it.

### oidc: bring your own IdP

Each `[oidc.<alias>]` entry is one trust relationship with one issuer.
Incoming JWT bearers are routed to the entry whose `issuer` matches the
token's `iss` claim; opaque bearers are only attempted in introspection mode.
Validation is fail-closed on every parse, fetch, signature, expiry, audience,
or mapping failure.

Two validation strategies:

- `jwks` (default): offline signature verification (RS256/ES256) against the
  issuer's published JWKS, fetched at boot and refreshed once on an unknown
  `kid` for key rotation. No per-request IdP round-trip; revocation is only
  as fresh as token expiry.
- `introspection`: every token is checked online via the issuer's RFC 7662
  endpoint. Immediate revocation, one IdP round-trip per validation, requires
  `client_secret`.

Verified claims resolve to grants by walking `claim_path` (a dotted path such
as `realm_access.roles` or `groups`), mapping each value through `role_map`
to a permission profile, and merging with union semantics. A token that maps
to no profile is denied. Set `require_mfa` to demand the token's `amr` claim
attests MFA (`mfa`, `otp`, or `hwk`).

{{#config-fields oidc}}

{{#config-where oidc}}

{{#secret-config oidc.<alias>.client_secret}}

Per-IdP setup guides: [Keycloak](./oidc-keycloak.md),
[Authentik](./oidc-authentik.md), [Zitadel](./oidc-zitadel.md).

### Enrollment without a browser

The daemon host often has no browser. Two OAuth flows obtain a token for the
`initialize` handshake headlessly, using the issuer's discovery document:

- **Device authorization grant**: enrollment starts on the daemon side, you
  get a short user code and verification URI to approve from any other
  device, and the client polls until the IdP grants the token.
- **Client credentials**: confidential-client grant for headless service
  accounts; requires the entry's `client_secret`.

`client_id` defaults to `audience` when unset.

## Users roster

`[users.<name>]` is the roster for the `ssh-key` and `peercred` providers.
The map key is the username presented during the SSH handshake and recorded
as the principal id. OIDC users never appear here; their identity comes from
the token.

{{#config-fields users}}

{{#config-where users}}

A user entry must name a `permission_profile` and carry at least one
credential (`authorized_keys` and/or `uid`); the daemon refuses to start
otherwise.

## Permission profiles

{{#include ../_snippets/concept-permission-profile.md}}

{{#config-fields permission_profiles}}

{{#config-where permission_profiles}}

Grants are resource-verb pairs. Resources: `system`, `sessions`, `memory`,
`cron`, `config`, `agents`, `cost`, `skills`, `personality`, `logs`, `tui`,
`files`, `locales`, `quickstart`, `channels`, `providers`, `models`,
`peer_groups`, `plugins`, `tools`. Verbs: `create`, `read`, `update`,
`delete`, `execute`. `admin = true` grants everything and makes the other
fields irrelevant. Beyond resource verbs, a profile can constrain which
agent aliases the holder may address (`allowed_agents`), which dotted config
paths it may write (`config_write_paths`, with `.*` granting a subtree), and
which tools it may cause an agent to run (`allowed_tools`; the agent's own
risk profile still applies on top).

## Session and memory isolation

Sessions are keyed to their owning principal. When a real (non-admin,
non-shared-operator) principal creates a session, that session is stamped
with its principal id, and access checks at dispatch are fail-closed:

- `session/list` shows a scoped principal only its own sessions.
- Reading messages or state, deleting a session, and session-scoped memory
  operations (list, search, store) all verify ownership first.
- Legacy sessions created before this feature carry no owner and are
  invisible to scoped principals rather than shared with them.

Admin-profile principals and the shared operator are unscoped and see
everything, which preserves single-user behavior exactly.

Memory isolation is transitive through session ownership: a scoped principal
cannot reach another principal's session-scoped memory. There is no separate
per-principal memory namespace beyond that.

## Audit attribution

Every request line is executed inside a principal attribution scope, so every
audit event a handler records carries `principal_id` and `auth_provider`
(the provider's registry name, e.g. `oidc.<alias>`, `ssh-key`, `peercred`)
without any handler opting in. Authorization decisions are themselves events:
grant-gate allows record at DEBUG, denials at WARN with the method, resource,
verb, and verdict, and the gateway's route-layer auth middleware records its
denials with method and path. The upshot: "who did what, and what was
refused" is answerable from the log stream alone.

## Threat model

What this layer defends against:

- **Unauthenticated remote access.** WSS is default-deny without a resolved
  principal, and the daemon refuses to start WSS without a remote credential
  path configured.
- **Privilege escalation between users.** Grants are deny-by-default;
  profiles enumerate what a principal holds and nothing else. A token mapping
  to no profile is denied outright rather than given a floor.
- **Cross-user data exposure.** Session and session-memory access is
  ownership-checked fail-closed; unowned legacy rows are hidden from scoped
  principals, never leaked to them.
- **Token forgery and replay.** JWT validation is fail-closed on signature,
  expiry, issuer, and audience; SSH nonces are single-use and
  connection-scoped; introspection mode gives immediate revocation when you
  need it.
- **Repudiation.** Per-principal audit attribution plus decision events give
  a complete authorization trail.

What it does not defend against:

- **A hostile root or daemon-uid process on the host.** The daemon's own uid
  keeps the trusted path; host compromise is out of scope.
- **A compromised IdP.** The issuer is trusted per configuration. `role_map`
  bounds the blast radius to the profiles you mapped, but a forged-at-source
  token is indistinguishable from a real one.
- **Stale JWKS revocation.** In `jwks` mode a revoked token stays valid until
  it expires. Use `introspection` where that window is unacceptable.
- **Agent-level tool policy.** Profiles constrain what a principal may ask
  for; what an agent may actually execute is the
  [security model](./model.md)'s job, layered underneath.

## Migrating an existing install

Nothing is required. An install with no `[oidc]`, `[users]`, or
`[permission_profiles]` entries behaves exactly as before: local socket
trust, pairing for the gateway. The first configured provider changes only
remote posture (WSS becomes authenticate-or-deny); local socket behavior for
the daemon's own uid is preserved throughout. Sessions created before the
upgrade have no owning principal: the operator and admin principals see them
as always, scoped principals do not.
