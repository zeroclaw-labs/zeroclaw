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
3. Every RPC method except the two handshake methods is classified to a
   required resource-verb grant and refused without it. `initialize`
   carries the credential itself, and `cert/renew` is authenticated by the
   mutual-TLS client certificate that presents it, not by a principal.
   Fine-grained selectors compose on top:
   - config writes check `config_write_paths`;
   - `session/new` and `session/prompt` check the agent selector and hold
     the session's workspace to a directory that agent's policy lets it both
     read and write, whether the workspace was named by the request, stored
     with a resumed session, or restored from a durable one. The other
     session methods do not check the agent yet, as described under
     [What this layer does not do (yet)](#what-this-layer-does-not-do-yet);
   - running or approving an SOP requires every agent it runs as, with the
     same tool-selector rule as a session, and creating, saving, or deleting
     one requires the agents it runs as. A step that names no agent, on the
     step or on the procedure, counts as the first configured agent alias in
     sort order, which is the agent the headless executor falls back to.
     Every step counts, including the steps of a deterministic procedure;
   - attachments, personality files, cost queries that name an agent, and
     cron jobs check the agent selector. An attachment sent by local path
     must also name an absolute path the destination agent's policy lets it
     read. A fleet cost summary lists only the principal's agents in its
     per-agent breakdown, but its totals and its per-model usage still cover
     every agent;
   - `fs/list_dir` lists only absolute paths that the policy of an enabled
     agent the principal may use lets that agent read. It refuses relative
     paths, `..` components, and, on Windows, network and device paths.

   These roots come from each agent's resolved policy: its risk profile, the
   sibling workspaces its `workspace.access` grants read access to (read and
   write, for a session workspace), and the shared skills directory, which is
   read-only. An agent that is not workspace-only
   (autonomy `full`, a risk profile with `workspace_only = false`, the
   `yolo` preset, or `workspace.unrestricted_filesystem = true`) may read
   any path outside its forbidden paths, so a principal entitled to such an
   agent may list and open sessions anywhere that agent could.

   `session/new` and `session/prompt` repeat these checks after waiting for
   the session's queue, so a request queued before its principal was
   narrowed or its credential expired is refused when its turn comes.

Authorization is **live** for edits made through the daemon's RPC config
methods, which is what zerocode's config editor uses: editing
`[permission_profiles]`, `[users]`, `[oidc]`, or `security.trust_daemon_uid`
that way re-compiles the policy at save time. Established native-token and
local connections re-resolve at their next operation, with no reconnect or
restart, and an OIDC connection must initialize again. Edits made
outside the daemon, directly in `config.toml`, through the web dashboard, or
with `zeroclaw config set`, apply at the next daemon reload or restart.
Revoking a gateway pairing token through the gateway's pairing controls
invalidates connections authenticated with it before their next operation.
Removing a token from `gateway.paired_tokens` by editing config, over RPC
or on disk, has no effect on the running daemon, and connections using the
token stay authorized. The removal applies at the next daemon reload or
restart, unless a pairing change made through the gateway before then
writes the live token set, that token included, back to config. Revoke
tokens through the pairing controls.

## Providers

| Provider | Credential | Configured by |
|---|---|---|
| `native` | Gateway pairing bearer token | Gateway pairing (`/pair`); the daemon and gateway share one live token authority |
| `peercred` | Unix peer uid on the local socket | Always on; `[users.<name>].uid` maps a uid to a named principal |
| `oidc.<alias>` | JWT or opaque bearer from your IdP | `[oidc.<alias>]` |

### Local connections

On a Unix socket, a connection that presents no token is identified by its
kernel peer uid. While `security.trust_daemon_uid` is `true` (the default),
the daemon's **own uid** connects as the trusted shared operator with full access, with or
without a `[users]` roster, so an install with no roster behaves as before
for the account that runs the daemon.

The operator who runs the daemon owns its config file, and local-only
lockout recovery depends on that authority. On a Unix socket, set
`security.trust_daemon_uid` to `false` to require every local peer,
including the daemon's own uid, to map through the roster or present a
token. The setting has no effect on a Windows named pipe, described below.

Any **other** uid must be mapped by an explicit `[users.<name>].uid`
entry. An unmapped uid (root included) that presents no token is denied,
whether or not a roster exists; there is no fallback to shared-operator
access. The listener
creates the socket owner-only, so today only the daemon's own account and
root can reach it; a roster entry decides what any other uid may do once
it can.

Windows named pipes carry no peer uid. With no roster, the pipe ACL is the
credential and a local connection is the shared operator. Once a
`[users]` roster exists, a local client there must present a token,
`security.trust_daemon_uid` has no effect, and the daemon's own account has
no trusted local route. A paired gateway token still authenticates as the
shared operator while the policy compiles, so a client that sends one, over
WSS or over the pipe, can repair the roster live. zerocode's local pipe
connection sends no token, so without such a client a lockout is repaired
by editing `config.toml` and restarting.

A client may forward its shell environment in `initialize` so the
daemon's subprocesses see its `PATH` and credential sockets (see
[Environment variable pass-through](../zerocode/environment.md)). That
snapshot is kept only for an operator-level principal (the shared
operator, or a roster principal with `admin = true`) on a local connection.
Every other principal, and every remote connection, gets the daemon's own
environment instead.

#### Recovery

A remote authentication bypass is never offered: a remote connection always
has to present a valid credential. While the authorization policy still
compiles, a client holding a paired gateway token or an operator-level
principal can repair a lockout from anywhere. Without such a credential,
and always in the deny-all state, the route back runs on the host that runs
the daemon. Which local route applies depends on whether the policy still
compiles.

**Locked out of a policy that compiles.** Authorization is live and the
local trusted path is intact on a Unix socket; on Windows, see the named
pipe note above. Connect locally as the daemon's own uid:
with `security.trust_daemon_uid = true` (the default) that account is the
trusted shared operator whatever the roster says. Repair the offending
entry over that local connection, for example in zerocode's config editor
or with an RPC `config/set` of `users.alice.uid`, and the change is
compiled and published at save time. Editing `config.toml` or running
`zeroclaw config set` also repairs it, but only once the daemon reloads or
restarts.

**Locked out by a deny-all accepted state.** The policy did not compile,
so the accepted state refuses every principal before resolution runs, the
daemon's own uid and the shared operator included. No RPC repairs it:
`config/set` and `config/reload` are refused along with everything else.
Edit `config.toml` directly as its owner, then restart the daemon so the
repaired sections are compiled and published. The daemon ignores `SIGHUP`,
so a restart is the step that reloads it.

If `security.trust_daemon_uid` is set to `false`, the trusted-uid route is
gone. A policy that compiles can still be repaired live by a client that
presents a paired gateway token, or by a roster principal with admin
grants; otherwise both states repair the same way: edit `config.toml` as
its owner and restart. Turn the setting off only where that is acceptable.

### The users roster

{{#config-fields users}}

The entry name doubles as the durable principal id unless `principal_id`
pins one explicitly. Audit records key on that id today. Sessions, memory,
and approvals are not keyed on it yet (see
[What this layer does not do (yet)](#what-this-layer-does-not-do-yet)), but
they will be, so to rename an entry without orphaning its data later, set
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

Progress messages go to stderr; stdout carries only the token, so both
commands compose with command substitution (the `oidc` commands run before
any startup prelude that could print, the OTP seed disclosure included).
Nothing is stored: present the token as `auth_token` in the RPC handshake
(or via the environment variable) before it expires, then re-enroll.

The client trusts the issuer the entry names and nothing else: the
discovery document must assert exactly that issuer before any endpoint it
advertises is used, every endpoint that receives a credential must satisfy
the same URL policy as the issuer (`https`, or `http` only for an exact
loopback host), redirects are never followed, response bodies are
size-capped, and a token response is accepted only when it carries a
non-empty `Bearer` access token. A confidential client (an entry with a
`client_secret`) authenticates with HTTP Basic on every request; a public
client sends its `client_id` in the form.

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
`"*"` selector or `admin = true`. Multiple profiles merge by union. For
cron jobs, attachments, personality files, per-agent cost queries, and SOP
authoring, `allowed_agents = ["*"]` covers only the agents the
configuration defines, not any alias a request names.

Tool selectors compose by intersection at agent assembly, on top of the
coarse grant: model-facing tool execution is `tools = ["execute"]`, and a
principal without that grant receives a tool-less session whatever its
`allowed_tools` names, including `"*"`. With the grant held, a session
created by a constrained principal only receives the tools its
`allowed_tools` names (an empty list yields a tool-less session), on top
of whatever the agent's own risk profile allows. After a queued prompt is
admitted, authorization is rechecked against the shared resolver, including
credential expiry and revocation. The current principal selector narrows
static tools, the deferred search registry, already-activated tools, and
pinned MCP resource content (each pinned block is admitted under its
`<server>__<uri>` name and is withdrawn from later prompts once the selector
no longer names it); removed tools cannot be reactivated. Reused sessions
are narrowed before their next turn, and rehydrated sessions are rebuilt
under current grants. Agent selectors are checked before a turn and before
rehydration.

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

That composition does not cover every route to an agent's tools. Cron jobs
and SOP authoring check only the agent selector. A constrained principal
holding cron grants can create a shell job for its agent, or give an
existing agent job a new prompt and trigger it, and one holding SOP create
or update grants can save a procedure whose trigger runs it later. Treat
cron and SOP authoring grants as grants of the agent's tools.

## Breaking change: remote WSS requires authentication

From this change on, a remote WSS connection must present `auth_token` in
`initialize`. There is no unauthenticated remote fallback.

A `[wss]` listener enabled with no possible credential path (no
`[oidc.<alias>]`, no paired tokens, and `gateway.require_pairing = false`)
is rejected by config validation, so no supported surface can save one.
A configuration already on disk in that shape still boots: the daemon
starts and the listener denies every remote handshake. It does not refuse
to load, because an operator has to be able to boot a daemon in order to
repair it.

Invalid `[oidc.<alias>]`, `[users]`, or `[permission_profiles]` sections
are the separate case. There the authorization policy itself does not
compile, so the daemon installs a deny-all accepted state and logs that
it is doing so until the sections are repaired and reloaded. Every
principal is refused under that state, on remote and local connections
alike, the daemon's own uid included, so the repair is the on-disk one
described under Recovery above.

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

   or by pointing at a file that holds it:

   ```toml
   [connection.wss]
   uri = "wss://daemon.example.com:9443"
   auth_token_file = "/etc/zeroclaw/zerocode-bearer"
   ```

   Precedence is `ZEROCLAW_AUTH_TOKEN`, then `auth_token_file`, then
   `auth_token`. On platforms with Unix permission bits, a referenced file
   that any other account can read is skipped with a warning rather than
   used, and the next source in that order applies. Elsewhere the file's ACL
   is the only guard.

   The environment variable is the recommended path. When the token is
   kept in the config file instead, zerocode writes that file owner-only
   (`0600`, in a `0700` config directory) and repairs the modes of a file
   that predates this, on platforms with Unix permission bits. On
   platforms without them the directory ACL is the only guard, so treat
   the file as a secret there.

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
  scoped principal. Selection is explicit, mirroring the RPC
  handshake's `auth_provider` field: the named provider's denial is
  authoritative, and there is never a fallback between providers.
- CORS preflight (`OPTIONS`) passes through unauthenticated, as it
  always has. Any other method outside GET, HEAD, POST, PUT, PATCH and
  DELETE is refused.

A scoped principal's `Config` grants are enforced in two steps. The
route layer applies a coarse floor per HTTP method: a read needs
`read`, anything else needs at least one of `create`, `update` or
`delete`, so a read-only principal never reaches a mutating handler.
Each mutating handler then authorizes its **complete write set** before
its first side effect: every config path the mutation will persist,
classified by what it does to the configuration (`create` for a path it
brings into being, `delete` for one it removes, `update` otherwise) and
matched against the profile's `config_write_paths` selectors. The
classification follows the operation, not the method: creating a map
key through `POST /api/config/map-key`, or implicitly through a `PUT`
under a new alias, needs `create`; a JSON Patch `remove` and
`DELETE /api/config/map-key` need `delete`; a rename needs `delete` on
its source and `create` on its destination; the references a delete or
rename cascade rewrites elsewhere are part of the write set too. One
unauthorized member refuses the whole mutation, batch or cascade, and
nothing is written. Operations whose write set cannot be enumerated up
front (a schema migration of the file, a Quickstart apply) require the
`*` selector. The persist boundary re-checks the paths about to be
written against what the handler authorized, so a handler cannot
persist more than it authorized.

Policy moves only at that persist boundary: the handler that writes a
configuration publishes the authorization state compiled from it as
the next accepted revision, and every request is verified and resolved
against the accepted snapshot as it stands. Nothing on the request
path recompiles policy, so a request that read the configuration
before a concurrent persist can never reinstall the older policy over
the newer one; a persisted change to a provider's verification
settings, a roster or a profile takes effect on the next request. The
daemon's own RPC surface holds a separate live configuration and
reaches the same state through the reload the gateway write flags.
Other gateway surfaces keep the pairing check per handler and adopt the
layer in follow-ups.

## Credential lifecycle

- **Expiry** ends the connection's authorization at the deadline; the
  client re-initializes with a fresh token.
- **Introspection revalidation**: OIDC introspection identities carry a
  revalidation deadline; past it, the next operation is refused until the
  client re-initializes (which re-verifies against the IdP).
- **Pairing revocation** applies before the connection's next operation.
- **Log subscriptions**: an open `logs/subscribe` stream is rechecked on
  every delivery and ends at the first one after its credential expires,
  its pairing is revoked, or its principal loses `Logs:Read`.
- **Policy changes**: a config save that leaves `[oidc]`, `[users]`,
  `[permission_profiles]`, and `security.trust_daemon_uid` unchanged keeps
  every binding as it is. A change to any of them publishes a new policy.
  Native-token and local connections re-resolve against it in place, but
  the daemon does not keep an OIDC bearer, so the next operation on an OIDC
  connection is refused until the client re-initializes.
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

Consolidation and governance derive shared-plane rows and do not run for
private sessions, and administrative access into another principal's
private memory has no surfaced pathway yet (deny-by-default).
`sops/runs` and `sops/run-detail` return the run history of every
procedure to a principal holding `Sops:Read`, whichever agents it ran as,
unlike cron history. Gateway HTTP routes keep their existing pairing
checks, and channel identities do not resolve into this principal model.

While `security.trust_daemon_uid = true` (the default) and the policy
compiles, the daemon's own uid on a Unix socket keeps full access, so a
single-operator install with no `[users]` roster and valid auth sections
behaves as before.

Permission profiles limit what a principal can do over RPC. They do not
isolate the code a principal causes an agent to run. Session turns, cron
jobs, and SOP steps run as the daemon account. On Unix that code can
connect to the local socket as the daemon's uid, which is the shared
operator while `security.trust_daemon_uid = true` (the default) and
otherwise gets whatever roster entry maps that uid. On Windows with no
roster, the pipe makes it the shared operator. It can also edit `config.toml`, which
the daemon applies at its next reload or restart. Setting
`security.trust_daemon_uid = false` does not change either. Grant session,
cron, or SOP execution or authoring only to principals you would trust
with operator access. Only OS-level confinement that keeps agent processes
away from the socket and `config.toml` isolates that code.

Some config paths carry authority themselves, so treat a broad
`config_write_paths` grant as operator access unless it covers only vetted
leaf settings. For example:

- a principal that can write `permission_profiles`, `users`, `oidc`,
  `security`, or `gateway.paired_tokens` can grant itself anything;
- one that can write `agents`, `risk_profiles`, `cron`, `channels`, or
  provider settings can change which agents run, with which tools, and
  where they may read and write;
- one that can write `mcp`, `mcp_bundles`, or
  `tunnel.custom.start_command` can choose programs the daemon account
  starts.

`Config:Update` also grants `config/reload`, which applies whatever
`config.toml` holds on disk.

zerocode's remote directory picker opens at the daemon's filesystem root.
A principal without operator grants may list it only through an enabled
agent whose policy lets it read `/`, such as one that is not workspace-only.
For any other such principal the picker reports a refusal there until it
opens inside an allowed root instead.
