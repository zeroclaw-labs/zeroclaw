# Authentication & principals

Every RPC connection to the daemon binds a **principal** during the
`initialize` handshake, and every method call is checked against the grants
the current configuration assigns that principal. This page covers the
provider set, the local-user roster, permission profiles, and, most
importantly, what changes for existing remote connections.

## The model in one pass

1. A **provider** verifies one credential: an explicit `auth_token` from
   the handshake selects the provider named by `auth_provider` (defaulting
   to `native`, the gateway pairing token), and with no token a local Unix
   socket presents its kernel peer uid to the `peercred` provider. A
   selected provider's rejection is final: a credential is never retried
   against another provider.
2. The **shared resolver** maps the verified identity to a canonical
   principal id and the permission profiles the configuration assigns it.
   OIDC identities are keyed by validated issuer + subject, local roster
   identities by their durable `[users.<name>]` principal id.
3. Every RPC method is classified to a required resource-verb grant and
   refused without it. Fine-grained selectors compose on top: config
   writes check `config_write_paths`, `session/new` checks the agent
   selector.

Authorization is **live**: editing `[permission_profiles]`, `[users]`,
`[oidc]`, or `security.trust_daemon_uid` re-compiles the policy at save
time, and established connections re-resolve at their next operation, with
no reconnect or restart. Revoking a gateway pairing token invalidates
connections authenticated with it the same way.

## Providers

| Provider | Credential | Configured by |
|---|---|---|
| `native` | Gateway pairing bearer token | Gateway pairing (`/pair`); the daemon and gateway share one live token authority |
| `peercred` | Unix peer uid on the local socket | Always on; `[users.<name>].uid` maps a uid to a named principal |
| `oidc.<alias>` | JWT or opaque bearer from your IdP | `[oidc.<alias>]` |

### Local connections

With no `[users]` roster configured, local behavior is unchanged: the
socket's `0o600` mode is the credential and the connection is the trusted
shared operator with full access.

The daemon's **own uid** keeps that trusted path even after a roster is
configured, controlled by `security.trust_daemon_uid` (default `true`).
The operator who runs the daemon owns its config file, and local-only
lockout recovery depends on that authority. Set it to `false` to require
every local peer, including the daemon's own uid, to map through the
roster or present a token.

Any **other** uid must be mapped by an explicit `[users.<name>].uid`
entry. An unmapped uid (root included) is denied; there is no fallback to
shared-operator access.

### The users roster

{{#config-fields users}}

The entry name doubles as the durable principal id unless `principal_id`
pins one explicitly. Ownership of sessions, memory, and audit trails keys
on that id. To rename an entry without orphaning its data, set
`principal_id` to the original id in the same edit.

### OIDC

Each `[oidc.<alias>]` entry is one trust relationship with one issuer;
token verification (offline JWKS or RFC 7662 introspection), claim
mapping, and the lifetime bounds are documented on the section reference:

{{#config-fields oidc}}

#### Enrolling (getting a token to present)

The daemon only verifies tokens; clients obtain them from the IdP. Two
browserless flows ship with the CLI:

```sh
# Interactive sign-in via the Device Authorization Grant (RFC 8628):
# prints a verification code to enter in any browser, waits for
# approval, then writes the access token to stdout.
export ZEROCLAW_AUTH_TOKEN="$(zeroclaw oidc login corp)"

# Same, via the system browser: Authorization Code + PKCE (S256 only)
# with an RFC 8252 one-shot loopback listener. The mechanisms never
# fall back into each other.
export ZEROCLAW_AUTH_TOKEN="$(zeroclaw oidc login corp --browser)"

# Headless service principals via client_credentials (requires the
# entry's client_secret):
export ZEROCLAW_AUTH_TOKEN="$(zeroclaw oidc token corp)"
```

Progress messages go to stderr; stdout carries only the token, so all
commands compose with command substitution. Nothing is stored: present
the token as `auth_token` in the RPC handshake (or via the environment
variable) before it expires, then re-enroll.

`--browser` opens the system browser for you on macOS and Linux, and on
every platform it also prints the sign-in URL so you can open it by
hand in a browser on the same machine (the callback lands on a loopback
port of the host running the CLI). The browser opener runs detached
from the CLI's standard streams, so whatever a launcher writes on its
own stdout cannot contaminate the result: stdout still carries only the
token.

Both callback adapters, the CLI's loopback listener and the gateway
callback, enforce the RFC 9207 issuer check. A response whose `iss`
parameter does not match the issuer that started the flow is refused
before its `code` or its `error` is acted on. A response that carries no
`iss` at all is accepted, since the parameter is optional and not every
issuer sends it. When the code exchange returns an `id_token` next to
the access token, that `id_token` is validated in full (issuer,
audience, expiry, and the nonce bound to this flow) and then discarded.
Only the access token is ever presented to the daemon. The enrollment
client applies the same suspicion to discovery itself: it refuses a
`.well-known/openid-configuration` document whose `issuer` is not an
exact match for the configured issuer, trailing slash included, before
it uses any endpoint named in it (the RFC 8414 check), and it caps the
size of the documents it reads, so a hostile or broken issuer cannot
redirect the flow to endpoints of its choosing or answer with an
unbounded body.

Clients that hold no IdP credentials (the web dashboard, zerocode)
enroll through the gateway instead, which proxies the same flows with
the configured entry's client credentials: `GET /api/oidc/providers`
lists aliases, `POST /api/oidc/{alias}/device/start` and
`/device/poll` drive the device grant, and `GET /oidc/login/{alias}`
runs the browser flow, whose one-time callback page hands the token to
the opening window via `postMessage` (same-origin only) with a manual
copy fallback. The gateway also sends
`Cross-Origin-Opener-Policy: same-origin`, which severs `window.opener`
once the popup has navigated through the identity provider, so in
current browsers the manual copy on that page is the handoff that works
today and the `postMessage` contract is in place for the dashboard
follow-up. These routes are unauthenticated by necessity
(enrollment precedes authentication), rate limited, and grant nothing:
they only relay what the IdP grants after the user approves. Design
rationale and failure-mode table:
`docs/security/oidc-browser-pkce-design-8289.md` in the repository.

The rate limiting works in layers. Requests that start a flow
(`POST /api/oidc/{alias}/device/start`, `GET /oidc/login/{alias}`, and
`GET /oidc/callback`) count against an enrollment-specific instance of
the gateway's brute-force limiter: the same thresholds and the same
lockout that govern a bad pairing token, kept on their own ledger, so
an address locked out of enrollment can still pair or present a webhook
signature, and a lockout earned on either of those never blocks
enrollment. A callback counts as an attempt only when it is
unproductive: no live flow state for the `state` it carries, an issuer
that does not match, an error handed back by the identity provider, no
authorization code, an alias removed while the flow was in flight, or a
code exchange that fails. A callback that completes a sign-in costs
nothing, and neither does one the gateway itself turns away because its
relay capacity is in use, so a crowd of people signing in at once cannot
lock their shared address out. A browser sign-in that is refused because
the pending-flow store is full costs the caller nothing either, for the
same reason. Provider listings, device polls and sign-in starts carry a
per-client budget of 20 requests per minute, which leaves headroom over
RFC 8628's five-second minimum polling interval (12 polls per minute)
without letting a client spin. A poll counts as an attempt when the
identity provider answers `slow_down` or rejects the device code
outright, so a client relaying garbage device codes walks into the
existing lockout instead of polling forever; a transport failure on the
gateway's own leg to the identity provider says nothing about the
caller and is not billed to it. A client over its budget, or locked
out, gets HTTP 429 with a `Retry-After` naming the delay in seconds,
and zerocode waits at least that delay, and never less than the RFC's
five-second increment, before its next poll (still clipped to the device
code's remaining lifetime), and never polls faster than once every five
seconds in any case. At most 16 outbound relays to the identity
provider are in flight at once across all clients, which bounds what the
gateway will do to the IdP on everyone's behalf. The pending-flow store
holds 32 browser sign-ins at once, and one remote client may hold eight of
them, so a caller that starts sign-ins and never finishes them cannot take
the store away from everyone else for the ten minutes those flows live. Loopback clients are
exempt from the per-client budgets, as they are from every other gateway
auth limit, so a reverse proxy sitting on the same host must enable
`trust_forwarded_headers` for the per-client limits to apply to the real
callers behind it. Every enrollment response carries
`Cache-Control: no-store` and `Pragma: no-cache` on top of the gateway's
`no-referrer` policy, because device codes and access tokens travel in
those bodies.

zerocode enrolls over the same API from its own config.
`[connection.wss] enroll_url` names the gateway's HTTP origin, and an
optional path prefix is allowed. It must be `https://` unless it points
at a loopback address (`127.0.0.1`, `::1`, or `localhost`), because the
device code and then the access token travel over it, and redirects are
not followed, so a plaintext hop cannot be introduced after the fact.
The enrollment connection uses the same `[connection.wss.tls]` trust
material as the WSS leg: the configured CA, `skip_verify`, and the
mutual-TLS client certificate. When that material leaves the certificate
unchecked, zerocode asks before the device code goes out rather than
after the token has arrived, and it asks about the enrollment origin
itself, which may not be the daemon the session connects to afterwards.
Answering `always` records that origin in `skip_verify_routes`; a
non-interactive run has nobody to ask, so enrollment stops there. zerocode clips every polling wait to the
device code's remaining lifetime and never polls after it expires, and
it refuses an advertised lifetime above one hour or a polling interval
above five minutes rather than sleeping on a hostile answer. An
advertised interval of `0` or `1` is floored at RFC 8628's five-second
default, so a gateway answer cannot put zerocode on a once-a-second
poll the per-client budget would only refuse. The token it receives is
held in memory for that session only.

## Permission profiles

{{#config-fields permission_profiles}}

Profiles are deny-by-default: an unlisted resource is refused, an empty
selector list grants no instances, and broad access requires the explicit
`"*"` selector or `admin = true`. Multiple profiles merge by union.

Tool selectors compose by intersection at agent assembly: a session
created by a constrained principal only receives the tools its
`allowed_tools` names (an empty list yields a tool-less session), on top
of whatever the agent's own risk profile allows. After a queued prompt is
admitted, authorization is rechecked against the shared resolver, including
credential expiry and revocation. The current principal selector narrows
static tools, the deferred search registry, and already-activated tools;
removed tools cannot be reactivated. Reused sessions are narrowed before their
next turn, and rehydrated sessions are rebuilt under current grants. Agent
selectors are checked before a turn and before rehydration.

Narrowing never adds tools back to an existing agent. After expanding grants,
create a new session to receive the expanded surface. No new config snapshot
or independent principal-policy cache is stored in the agent.

Deferred MCP instructions are derived from the remaining loadable tools and
shown only when `tool_search` is exposed. A principal needs both `tool_search`
and the named deferred MCP tool for on-demand activation; the helper is never
implicitly granted. Permitted `mode = "always"` MCP tools are preactivated and
remain callable without the helper.

If either the principal's tool selector or agent selector is constrained,
`delegate` (bounded and independent), `spawn_subagent`, and `execute_pipeline`
are unavailable, including skill aliases wrapping those tools. These nested
paths do not yet carry both current principal ceilings; the ordinary parent
turn remains usable. Admin principals and principals with both selectors set
to `"*"` keep their agent's configured nested capabilities.

The existing eight-argument Rust `Agent::from_live_config_with_tui_env`
constructor remains available. RPC uses the additive
`from_live_config_with_tui_env_and_principal_tools` constructor and reapplies
the shared resolver's grants at prompt admission.

## Breaking change: remote WSS requires authentication

From this change on, a remote WSS connection must present `auth_token` in
`initialize`. There is no unauthenticated remote fallback, and a daemon
whose `[wss]` listener is enabled with no possible credential path (no
`[oidc.<alias>]`, no paired tokens, and `gateway.require_pairing = false`)
refuses to load its configuration rather than accept unauthenticated
clients.

Migration for existing remote zerocode users:

1. Pair with the gateway as usual to obtain a bearer token.
2. Give zerocode the token, either in its config:

   ```toml
   [connection.wss]
   uri = "wss://daemon.example.com:9443"
   auth_token = "zc_..."
   ```

   or via the `ZEROCLAW_AUTH_TOKEN` environment variable, which overrides
   the config value and keeps the credential out of the file.

An OIDC access token works the same way with `auth_provider = "oidc.<alias>"`.

## The gateway HTTP API

The gateway's configuration and onboarding routes (`/api/config*`,
`/api/quickstart/*`, `/api/channels/bind`) enforce authentication
structurally: one route-layer middleware guards the whole group, so no
individual handler carries (or can forget) a check. The middleware
speaks the same principal model as the RPC path, through the same
provider registry and resolver:

- A **paired bearer** (`Authorization: Bearer zc_...`) resolves to the
  shared operator with full access, exactly as before. Denials keep the
  historical 401 shape.
- An **OIDC bearer** presented with the `X-ZeroClaw-Auth-Provider:
  oidc.<alias>` header is verified by that provider and resolved to a
  scoped principal. Its `Config` grants then gate the request by HTTP
  method: read for GET, delete for DELETE, update for everything else.
  Selection is explicit, mirroring the RPC handshake's `auth_provider`
  field: the named provider's denial is authoritative, and there is
  never a fallback between providers.
- CORS preflight (`OPTIONS`) passes through unauthenticated, as it
  always has.

Scoped requests re-derive resolver policy from the live configuration,
so a roster or profile change applied through the gateway takes effect
on the next request. Changing a provider's verification settings
(issuer, keys, validation mode) still requires the daemon reload the
config write already flags. Other gateway surfaces keep the pairing
check per handler and adopt the layer in follow-ups.

## Credential lifecycle

- **Expiry** ends the connection's authorization at the deadline; the
  client re-initializes with a fresh token.
- **Introspection revalidation**: OIDC introspection identities carry a
  revalidation deadline; past it, the next operation is refused until the
  client re-initializes (which re-verifies against the IdP).
- **Pairing revocation** applies before the connection's next operation.
- The `tui_id`/`tui_sig` reconnect mechanism is continuity only: it
  preserves the TUI's registry identity and grants **no** authority. Every
  `initialize` re-presents a credential.

## Session isolation

Every session a scoped principal creates is stamped with that principal's
id in the live store and on disk (chat backend and ACP store). Scoped
principals see and touch only their own sessions: listings are filtered,
reads and mutations get one uniform not-found-or-not-owned denial (no
existence probing), destructive deletes run as owner-predicated storage
statements, and in-flight approvals resolve only for the owner of the
session they were raised for. Sessions created before this change (or by
unscoped connections) carry no owner: they stay fully visible to unscoped
connections and invisible to scoped principals.

Scoped principals get PRIVATE memory: their memory operations read and
write a per-principal plane whose owner travels in every storage
statement, invisible to and untouchable from the shared plane (and vice
versa). Private writes pass the same content scanning and policy gates as
shared writes, and private operations are audited with principal
attribution. On memory backends without principal support (markdown,
lucid, postgres, qdrant today) private memory fails closed with a clear
denial rather than silently un-scoping.

## Lockout recovery (local only)

An IdP outage, an expired client secret, or a bad `profile_map` edit can
lock every remote principal out at once. Recovery never depends on the
IdP: it runs over the local socket on the daemon's host, which stays
usable in two ways.

1. **The daemon's own uid** keeps the trusted shared-operator path on the
   local socket while `security.trust_daemon_uid` is `true` (the
   default). SSH to the host as the account that runs the daemon and the
   CLI works with full access, no credential involved.
2. **A mapped local uid** from the `[users]` roster authenticates through
   OS peer credentials, IdP or not. Keep at least one admin-profiled
   roster entry on any hardened deployment that sets
   `trust_daemon_uid = false`.

From that local session, fix or remove the broken `[oidc.<alias>]`
entry, then reload the daemon. If both local paths were disabled and no
roster uid maps to you, recovery is by editing `config.toml` directly on
the host as the file's owner and restarting the daemon: authorization
policy lives in the config, so host file access is by design the root of
trust. There is deliberately no remote break-glass credential.

## Migrating from [security.nevis]

The Nevis IAM integration was removed; its config table is accepted,
ignored with a load-time warning, and dropped on the next config save.
Its scope maps onto the current stack:

| Nevis concept | Replacement |
|---|---|
| `instance_url` / `realm` token validation | `[oidc.<alias>]` `issuer` + `validation` (`jwks` or `introspection`) |
| `role_mapping` role → permissions | `claim_path` + `profile_map` → `[permission_profiles.<alias>]` grants |
| `require_mfa` | `[oidc.<alias>] require_mfa` / `required_acr` |
| `session_timeout_secs` | `max_auth_lifetime_secs` (offline) / `revalidation_secs` (introspection) |

## What this layer does not do (yet)

Agent-loop memory (auto-save and per-turn recall inside sessions) stays
on the shared plane, administrative access into another principal's
private memory has no surfaced pathway yet (deny-by-default), gateway
HTTP routes keep their existing pairing checks, and channel identities do
not resolve into this principal model. The daemon's own uid and the shared operator retain full
access throughout, so single-operator installs behave exactly as before.
