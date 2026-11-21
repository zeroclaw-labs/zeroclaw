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
     with a resumed session, or restored from a durable one. The session is
     bound to the resolved directory, not the requested spelling, so
     retargeting a symlinked cwd after admission does not move it, and a
     live session held under such an alias is refused. The other
     session methods do not check the agent yet, as described under
     [What this layer does not do (yet)](#what-this-layer-does-not-do-yet);
   - running or approving an SOP requires administrator grants. Creating,
     saving, or deleting one requires the agents it runs as. A step that names
     no agent, on the step or on the procedure, counts as the first configured agent alias in
     sort order, which is the agent the headless executor falls back to.
     Every step counts, including the steps of a deterministic procedure;
   - attachments, personality files, cost queries that name an agent, and
     cron jobs check the agent selector. An attachment sent by local path
     must also name an absolute path the destination agent's policy lets it
     read. A fleet cost summary lists only the principal's agents in its
     per-agent breakdown, but its totals and its per-model usage still cover
     every agent;
   - the daemon-wide log and event reads (`logs/subscribe`,
     `events/subscribe`, `events/history`, `logs/query`, and `logs/get`)
     serve only administrators and the shared operator. Their records come
     from every principal's work, other principals' authorization denials
     among them, and many name no owner, so a scoped principal is refused
     even with `Logs:Read` and the `*` agent selector;
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

### When authorization edits take effect

Authorization is **live** for edits the running daemon commits: the daemon's
RPC config methods, which is what zerocode's config editor uses, the web
dashboard's config API when the gateway runs under the daemon, and
`zeroclaw config set` or `zeroclaw config patch` when the daemon accepts the
edit from the CLI. The RPC methods and the supervised gateway share the
daemon generation's live configuration, config write lock, and accepted
authorization authority. Editing `[permission_profiles]`, `[users]`,
`[oidc]`, or `security.trust_daemon_uid` that way compiles and publishes the
policy at save time, before the write returns success. Established
native-token and local connections re-resolve at their next operation, with
no reconnect or restart, and an OIDC connection must initialize again. The
dashboard's pending-reload flag still covers daemon-owned subsystems that
need rebuilding; it does not delay this authorization publication. Edits
made directly in `config.toml` apply at the next daemon reload or restart.

For `zeroclaw config`, an authorization edit is one that writes or changes
`[permission_profiles]`, `[users]`, `[oidc]`, or
`security.trust_daemon_uid`. The CLI counts this configuration's daemon as
running while its heartbeat file beside `config.toml`
(`state/daemon_state.json`) is recent and, on Unix, names a live process.
Without such a heartbeat, the CLI checks the configuration's ownership
lock, which a daemon takes before it loads its configuration and holds
while it runs. If another process holds the lock, typically a daemon still
starting, before its first heartbeat, or a standalone `zeroclaw agent` or
`zeroclaw gateway start`, the CLI sends nothing, saves the edit itself and
reports it pending reload. Only when the lock is free does the CLI find no
daemon: it then saves `config.toml` as before, prints nothing new, and
contacts no endpoint, not even one an exported `ZEROCLAW_SOCKET` names; the
edit applies when a daemon next starts.

With the daemon running, `config set` and `config patch` first check that
the endpoint is that daemon's (on Unix, the socket's peer must be this
account or root, and its process the one the heartbeat names), then hand
the edit to it as `config/set`, or `config/set-many` for a patch. The
daemon validates and saves the edit, swaps it into its live configuration
and publishes the policy before the CLI reports it applied. It commits a
patch that also writes other paths as one batch. A patch the daemon would
commit fails whole if it has any `test` op, whatever path it checks, before
the daemon is contacted and with nothing saved: the CLI would check that
`test` against its on-disk copy while every write in the patch lands on the
daemon's live configuration. A patch the CLI saves itself, as with no
daemon, keeps the usual meaning of its `test` ops.

With the daemon running, the CLI instead saves `config.toml` itself and
reports the edit as pending reload, naming the reason, when the daemon
refuses the caller at the handshake, before binding a principal: a uid
outside the roster, or any caller while the daemon is in the deny-all
state. This lets the file's owner repair a lockout from the command line
(see [Recovery](#recovery)). The CLI does the same when the daemon runs
another release, fails the handshake, or cannot be reached; when the
endpoint is not this configuration's daemon, or is a named pipe (the CLI
does not verify a named pipe's server, so on Windows it never sends an
authorization edit over one); and when the daemon's config methods do not
take the write (clearing or masking a secret, a patch that creates a
keyed-list row such as `plugins.entries`, a patch of more than 256
writes). `config init` never commits an authorization entry through the
daemon, so while the daemon runs it reports every one it writes as
pending. Until a reload or restart picks up a pending edit, the running
daemon enforces the previous policy.

Before any pending save, the CLI checks that the resulting policy compiles.
It refuses, saving nothing, only an edit that would turn a policy that
compiles into one that does not, so with `config set` and `config init` a
file whose policy already fails to compile can still be repaired one field
at a time; `config patch` validates the whole resulting configuration
first, as it always has. A save writes back the value each `ZEROCLAW_*`
override replaced, so while such overrides set authorization inputs, the
policy config.toml holds after the save is checked the same way as the one
the CLI sees: a daemon started without the overrides compiles that one. The
file check uses the actual persisted baseline and candidate after environment
masking and incremental merging, including unrelated file edits made while
the CLI waited for the daemon. Both policies keep the same repair allowance;
CLI environment overrides are not reapplied when parsing the persisted file.
So while a daemon runs or another process holds
the ownership lock, `config init users.<name>` and
`config init oidc.<alias>` fail, since their entries cannot compile until
filled in, and `config init permission_profiles.<name>` is saved and
reported pending. The command fails with the daemon's reason, and nothing
is saved, when the daemon rejects the edit, or identifies the caller and
then refuses it because its principal lacks the config-write grant.
Set fields that are only valid together, such as those of a new roster or
OIDC entry, in one `zeroclaw config patch`. If the daemon is asked and does
not answer, the command fails without saving and names the property to
check with `zeroclaw config get`. A standalone `zeroclaw gateway start`
publishes its own config-API edits but picks up CLI and hand edits only at
its restart.

Human output adds one notice on stderr for an applied or pending edit.
With `--json` there is no notice; instead the envelope of `config set`,
`config patch` and `config init` gains a `daemon` member for an
authorization edit the daemon applied or the CLI reports pending:
`{"applied": true}`, or
`{"applied": false, "pending_reload": true, "reason": R}` with `R` one of
these codes.

| `reason` | Why the CLI saved the edit itself |
|---|---|
| `refused` | The daemon refused this caller at the handshake, before binding a principal: its uid is outside the roster, or the daemon is in the deny-all state. |
| `version_mismatch` | The daemon runs another release than the CLI. |
| `handshake` | The daemon answered, but the handshake failed. |
| `unreachable` | The heartbeat names a running daemon, but its endpoint could not be reached. |
| `other_daemon` | The endpoint is not this configuration's daemon, so it was not sent the edit. |
| `unverified_endpoint` | The endpoint is a Windows named pipe, whose server the CLI does not verify, so it was not sent the edit. |
| `owner_unconfirmed` | No recent heartbeat confirms a daemon, but another process holds the configuration's ownership lock, so the CLI sent nothing. |
| `not_replayable` | The daemon's config methods do not take this write. |
| `offline_command` | The command never commits this write through the daemon (`config init`). |

A `config patch --json` that fails uses the command's JSON error envelope:
`validation_failed` for an edit the daemon rejects as invalid, refuses for
the principal it identified, or one that would break a compiling policy;
`op_not_supported`, with `path` naming the tested property, for a `test` op
in a patch the daemon would commit; and `internal_error` otherwise, with
`path` naming the property to check when the outcome is unknown.

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
by editing `config.toml` and restarting. The CLI does not yet verify which
process serves a named pipe, so on Windows it commits no authorization edit
through a running daemon: `zeroclaw config set` and `zeroclaw config patch`
save the edit themselves and report it pending (`unverified_endpoint`)
until the next reload or restart.

A client may forward its shell environment in `initialize` so the
daemon's subprocesses see its `PATH` and credential sockets (see
[Environment variable pass-through](../zerocode/environment.md)). That
snapshot is kept only for an operator-level principal (the shared
operator, or a roster principal with `admin = true`) on a local connection.
Every other principal, and every remote connection, gets the daemon's own
environment instead.

Eligibility is checked again when a session is built or restored. An existing
session that retains a non-empty forwarded environment can be resumed or
prompted only by a currently authorized local operator. Reconnecting over WSS,
or losing `admin` while a prompt is queued, does not carry that environment into
the next turn: the request is refused before execution. Create a new session to
continue without the forwarded environment. Sessions without forwarded values
remain eligible for normal resume, subject to the other authorization checks.
The retained environment is immutable for the lifetime of its session; refusing
a later request does not rewrite it underneath an already running turn. A
resume also requires the current connection's authorized environment to match
the session's retained environment. If it differs, create a new session; an
environment-free session can still be resumed after `admin` is removed.

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
entry over that local connection, for example in zerocode's config editor,
with an RPC `config/set` of `users.alice.uid`, or with
`zeroclaw config set` run as that account, which commits through the
daemon; the change is compiled and published at save time. Editing
`config.toml` by hand also repairs it, but only once the daemon reloads or
restarts, and so does a `zeroclaw config set` that the CLI reports as
pending. When a roster principal without the config-write grant runs
`zeroclaw config set`, the daemon identifies it and refuses the edit, so
the command fails and saves nothing. The refusal does not guard the file:
the file's owner can still change `config.toml` directly, and that takes
effect at the next reload or restart.

**Locked out by a deny-all accepted state.** The policy did not compile,
so the accepted state refuses every principal before resolution runs, the
daemon's own uid and the shared operator included. No RPC repairs it:
`config/set` and `config/reload` are refused along with everything else.
Edit `config.toml` as its owner, by hand or with `zeroclaw config set`:
the daemon refuses that command at the handshake, so the CLI saves the
edit itself and reports it pending (`refused`). Then restart the daemon so
the repaired sections are compiled and published. The daemon ignores
`SIGHUP`, so a restart is the step that reloads it.

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

[Identity providers](./identity-providers.md) lists the providers tested
with ZeroClaw, what each one needs, and the current limitations.

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
`client_secret`) authenticates with HTTP Basic on device-grant and
`client_credentials` requests, and sends the secret in the form body for the
authorization-code exchange (`--browser`, the gateway's browser flow and
dashboard sign-in); a public client sends its `client_id` in the form.

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
current browsers the manual copy on that page is the handoff that works.
The dashboard itself does not use that page: its sign-in keeps the token
in the gateway behind a session cookie (see
[Dashboard sign-in](#dashboard-sign-in)). These routes are unauthenticated by necessity
(enrollment precedes authentication), rate limited, and grant nothing:
they only relay what the IdP grants after the user approves. Design
rationale and failure-mode table:
`docs/security/oidc-browser-pkce-design-8289.md` in the repository.

The gateway browser flow also requires a short-lived `HttpOnly` binding
cookie from the browser that started it. The cookie grants no session or
API access. A callback with no matching cookie is refused before code
exchange and consumes the pending flow; open a new login in the intended
browser instead of forwarding a callback URL between browsers. Each tab
has its own flow cookie. A relay-capacity refusal retains a valid flow and
cookie for retry within the original ten-minute deadline.

Binding cookies use `SameSite=Lax` for the identity provider's top-level
GET redirect. They have no `Domain` attribute and use `Path=/`, including
behind a gateway path prefix; HTTPS also uses `Secure` and the `__Host-`
name prefix. Serve remote enrollment over HTTPS. Plain-HTTP cookie scope
still spans ports on the same host; this is not isolation from another
application running on that host. Cookie blocking prevents browser
enrollment; the device flow remains available.

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
callers behind it. When that setting is on, the gateway takes the
rightmost address from `X-Forwarded-For` (the hop the immediate proxy
added) or else `X-Real-IP`. Configure the proxy to append or overwrite
rather than forward a client-supplied list unchanged, for example nginx
`proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;`.
If you set only `X-Real-IP`, also clear the higher-priority list with
`proxy_set_header X-Forwarded-For "";` together with
`proxy_set_header X-Real-IP $remote_addr;`; otherwise a client-supplied
`X-Forwarded-For` still wins. A forwarded loopback address is skipped so
it cannot claim the local-only limiter exemption; when the chain is
loopback-only the gateway uses a non-exempt key rather than the loopback
peer. Every enrollment response carries
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
poll the per-client budget would only refuse. The legacy automatic enrollment
path holds the token in memory for that session only. That path uses the
same device-grant helper as `zerocode login`, so it also sends an S256
PKCE challenge; a provider that rejects `code_challenge` on its device
authorization endpoint will fail here as well. For URL-scoped persistent login, use
`zerocode login <alias> <wss-url> --gateway-url <https-origin>`; see
[Remote setup](../zerocode/remote.md#sign-in-with-oidc-recommended-for-oidc-deployments).
That command starts from an empty client directory: OIDC sign-in authorizes
issuance of its first device certificate through the gateway, with no pre-created
device, local user, daemon pairing code, SAS, or separate enrollment listener.
The daemon verifies the bearer through the same accepted OIDC policy as RPC,
requires a human with `sessions:create` and an allowed agent (or
`system:execute` / admin), signs only the CSR's public key, and
records the new device in the existing certificate ledger and audit trail.
Static client-certificate pins still require out-of-band provisioning. WSS still
requires both mTLS and a valid OIDC access token on every initialize.
The command saves the accepted access token in the owner-only local
`zerocode-config.toml` and re-authenticates when startup needs a fresh token.

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
turn remains usable.

Some tools do work that does not carry the caller's principal, so no
principal without `admin` holds them in its sessions, whatever its selectors
name, `"*"` included:

- `sessions_list`, `sessions_history`, and `sessions_send` list, read, and
  append to other sessions without checking who owns them, and so would
  `sessions_reset` and `sessions_delete` if an agent registered them.
  `sessions_current`, which reports only the caller's own session, stays.
- `cron_add`, `cron_update`, `cron_run`, `cron_list`, `cron_runs`,
  `cron_remove`, and `schedule`: scheduled turns do not carry the caller's
  restrictions, and cron rows and run output are shared by agent rather than
  owned by the calling principal.
- `send_message_to_peer`, whose turns run on another agent.
- `sop_execute`, `sop_advance`, `sop_approve`, and `sop_workshop`: SOP steps
  run headlessly as the procedure's agents, and an applied workshop proposal
  becomes a shared procedure that later runs the same way. Read-only SOP
  tools follow the ordinary tool selector.
- `model_routing_config` and `proxy_config`, which save daemon config that
  every agent's later runs use. Over RPC the same change needs
  `config:update`.
- `spawn_subagent` and `execute_pipeline`, whose nested runs would hold the
  tools above again.

Each of these tools reports `requires_unrestricted_principal`, so a skill
alias of one goes with it. The direct `delegate` tool stays when both
selectors are `"*"`, because the session holds its handle: every bounded,
independent, background, parallel, and nested child it builds withholds the
same tools. It also resolves the originating credential's current authority
when it builds each child, so a child built after that credential loses
`admin` withholds them too. Its skill aliases are withheld, because their
captured targets cannot be identified as the bound handle.

The withholding is applied at session creation, at rehydration, before every
prompt, and when accepted steering is consumed, including its recheck after
memory writes. An administrator demoted mid-session loses the tools at the
next prompt or accepted steering. Like selector narrowing, it never gives
them back to a live session. Admin principals and the shared operator keep
their agent's configured tools, and existing cron jobs keep running.

A queued prompt resolves authority again on the exact Agent that will execute
the turn, after provider reconciliation and Agent/task waits. The same admission
applies withholding and protects prompt checkpoint or running-state writes
after storage contention. A refused admission does not call the provider or
persist a terminal turn. This is a turn-admission boundary, not continuous
revocation of a turn that is already running.

RPC `cron/patch` and `cron/trigger` also refuse agent jobs for non-admins,
including wildcard selectors. These jobs cannot carry the caller's session
restrictions; the check uses current authority after looking up the job.
Shell jobs retain their existing grant and command-policy checks. Cron reads,
deletion, and jobs already scheduled by an operator are unchanged.

RPC `sops/run` and `sops/decide` require administrator grants, including
when a non-admin has wildcard tool and agent selectors. Headless steps do
not carry the caller's principal restrictions. This includes deny, amend,
and revise decisions. After decision-model and engine waits, the RPC holds
current authority through its synchronous store effects. A busy SOP store
refuses the request immediately; retry once contention clears. Background
SOP processing keeps its normal storage timeout. SOP authoring retains its
separate agent-selector checks.

Until the session-data tools check ownership, scheduled agent jobs, SOP runs,
and peer turns started by an administrator or a trigger can still call them.
To remove them from those runs too, list `sessions_list`, `sessions_history`,
and `sessions_send`, plus any skill tool that targets them, in
`excluded_tools` of the risk profile of each agent those runs use. That
removes them from every run of those agents, administrators' sessions
included.

The existing eight-argument Rust `Agent::from_live_config_with_tui_env`
constructor remains available. RPC uses the additive
`from_live_config_with_tui_env_and_principal_tools` constructor and reapplies
the shared resolver's grants at prompt admission.

That composition does not cover every route to an agent's tools. Cron jobs
and SOP authoring check only the agent selector. A constrained principal
holding cron grants can create or edit a shell job for its agent with RPC
`cron/add` and `cron/patch`. The command meets the agent's approval policy
there, as it does from the CLI and the dashboard, so under supervised
autonomy a command that needs approval is refused rather than stored. Agent
jobs are refused to it as described above. One holding SOP create or update
grants can save a procedure whose trigger runs it later. Treat cron and SOP
authoring grants as grants of the agent's tools.

These checks run again when the job is written, not only when the request
arrives. A grant withdrawn, or a paired device revoked, while a cron write
waits for the database either stops that write or waits for it to finish.

A tool called from a `pipeline` step or through a skill alias never inherits
an approval. The runtime approves only the call it dispatched, so under
supervised autonomy a command that needs approval is refused there; call the
tool directly to be asked.

Principal-owned sessions may use synchronous and parallel delegation when
their grants allow it. Background delegation and its `check_result`,
`list_results`, `cancel_task`, and `await_sessions` actions are refused,
including calls through skill aliases and nested delegates. Those task rows
and result files currently use agent-alias visibility, which cannot protect
one principal's private output from another principal using the same agent.
This refusal includes administrator-owned sessions; their private memory is
still private. Shared operator delegation remains available. Existing shared
result artifacts are not migrated or removed by this restriction.

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

Verifying an OIDC bearer is work an unauthenticated caller makes the
gateway do, and with `validation = "introspection"` each verification
is one authenticated round trip to the identity provider under the
daemon's own client credentials. Two limits bound it. A refused
credential counts against a per-client ledger with the same thresholds
as a bad pairing token (10 in a minute, then a 5-minute lockout, with
loopback exempt), kept on its own ledger, so a client replaying a token
past its expiry never locks its operator out of pairing, and a pairing
lockout never blocks a bearer. The bearer ledger reads
`gateway.trust_forwarded_headers` from the live configuration for each
request, so a saved proxy-trust change applies without rebuilding the
router. A transport failure on the gateway's own
leg to the identity provider, and a verified identity the policy grants
nothing, say nothing about the caller and are not billed to it. At most
16 bearer verifications are in flight at once across all clients,
loopback included, which bounds what the gateway will do to the
identity provider on everyone's behalf; the slot is held for the
verification alone, never across the request it admits. A locked-out
client gets HTTP 429 and a verification refused for capacity gets HTTP
503, each with a `Retry-After` naming the delay in seconds. Dashboard
sessions are verified by their own layer and never compete for these
limits.

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
settings, a roster or a profile takes effect on the next request. In a
supervised run, the gateway and RPC surface share the live configuration,
write lock, and accepted policy authority, so a save through either surface
publishes to both. A standalone gateway has its own authority; it does not
publish changes to a separately running daemon.
Other gateway surfaces keep the pairing check per handler and adopt the
layer in follow-ups; a [dashboard sign-in](#dashboard-sign-in) session
also satisfies that check when its account is an administrator.

## Dashboard sign-in

The web dashboard can offer sign-in through a configured OIDC provider
instead of, or alongside, a pairing code. It is off by default: an
install without `[gateway.dashboard_sign_in]` shows the pairing dialog
exactly as before, and a session cookie sent to it is ignored.

```toml
[gateway]
require_pairing = true

[gateway.dashboard_sign_in]
oidc = ["corp"]                # [oidc.<alias>] entries offered on the sign-in screen
session_lifetime_secs = 28800  # optional cap on one session (300..=604800, default 8 h)
```

Turning it on needs:

- `gateway.require_pairing = true`. With pairing off every dashboard
  route is open to whoever reaches the gateway, so the config is refused.
  The official Docker image writes `require_pairing = false` into its
  default config; set it to `true` there, or with
  `ZEROCLAW_gateway__require_pairing=true`.
- HTTPS (`[gateway.tls]`, a reverse proxy with
  `trust_forwarded_headers`, or a relay-served dashboard with
  `[relay.dashboard]`, which is relay-terminated: see
  [secure transport, 3e](../zerocode/secure-transport.md)). Sign-in is
  refused over plain HTTP unless the browser runs on the gateway's own
  machine. A relay-served dashboard offers sign-in alone: the relay would
  see a pairing code or token, so pairing and paired tokens are refused
  there. An administrator signed in through the relay gives the relay
  administrator power, including lasting changes, so administer the daemon
  over a direct connection or with `zerocode`.
- Behind a reverse proxy, the browser's `Host` passed through unchanged
  (for example `proxy_set_header Host $host;` in nginx). The callback URI
  and the dashboard's own origin are both taken from it, so a proxy that
  rewrites `Host` to an internal name breaks sign-in and gets every
  signed-in change refused as coming from another origin.
- An identity provider client that uses Authorization Code with PKCE
  (S256), with `<dashboard origin><path_prefix>/oidc/callback` as a
  redirect URI (the same URI as the browser enrollment relay above).
  Register that exact URI, with no wildcard or prefix pattern: until the
  gateway checks the URI against a configured dashboard origin (#12048),
  it builds it from the request's `Host`, and the identity provider's
  exact match is what keeps a sign-in code from going to another address.
  Through a relay-served dashboard it is always that dashboard's own
  address, which the daemon logs at startup.
  Its client must be listed in `interactive_clients` (or classified by
  `actor_claim`), and its access tokens must carry the configured
  `audience`. [Identity providers](./identity-providers.md) lists the
  providers tested for this and the settings each one needs.
- An access-token lifetime that suits a working session, such as one to
  eight hours. The dashboard does not use refresh tokens: when the token
  expires the user signs in again.

When a user picks a provider, the gateway runs the browser flow and
binds it to that browser with a short-lived cookie, so a sign-in started
elsewhere cannot be finished there. The ID token is required and
validated as evidence of the sign-in (issuer, audience, authorized
party, expiry, nonce) and then discarded. The access token is verified by
the provider exactly as an RPC bearer is, and the account is the one it
resolves to; the ID token's subject is not compared with it, since both
come from the same back-channel exchange. A service client is never
given a dashboard session. The token then stays in the gateway's memory:
the browser holds only an opaque `HttpOnly`, `SameSite=Strict` session
cookie (`Secure` over HTTPS), and no token reaches browser storage or
script.

Every request carrying the cookie is checked again against the same
authority RPC uses: the token's expiry, the introspection revalidation
deadline, and the grants the account holds under the policy in force
now. A changed profile mapping, a removed provider, or an alias taken off
`gateway.dashboard_sign_in.oidc` takes effect on the next request. A
request that changes state must also send the session's CSRF token in
`X-ZeroClaw-CSRF`, and requests and WebSocket upgrades that carry the
cookie must come from the dashboard's own origin. A request with an
`Authorization` header is judged by that header alone.

What a signed-in account can do follows its permission profile:

- An account whose profile has `admin = true` is operator-equivalent and
  reaches every dashboard page, as a paired browser does. Each sign-in is
  logged with the principal it resolved to. Conversations it starts in
  the dashboard chat belong to the shared operator, as paired browsers'
  do.
- Any other account reaches the configuration pages its `Config` grants
  allow. Other pages answer 403 with `"reason": "operator_required"`, and
  the dashboard hides them.
- When the gateway runs inside the daemon, an account without `admin` can
  also chat in the dashboard, with the agents its grants allow, if its
  profile grants `sessions` create, read, execute and update. The agent
  uses only the tools the profile grants (`allowed_tools`, with
  `tools = ["execute"]`). A disabled agent takes no new chat, and a chat
  already open with it answers each message with an error until the agent
  is enabled again. Its conversations are its own: the daemon runs
  them as that account, under its grants, and nobody else's dashboard
  lists them. Image upload is not offered. Each chat is one conversation,
  and the conversation lasts only as long as the chat:
  - Closing or reloading the page, leaving the chat, or a dropped
    connection ends the conversation, and stops a reply under way. So do
    signing out, the session expiring, and a daemon reload or restart. A
    new chat starts a new conversation; none is reopened.
  - A sign-in has at most four chats open at once (four tabs, say), and
    an account at most 32 across all its sign-ins. A conversation still
    being created, or not yet closed after its chat ended, counts too, so
    signing out and in again frees nothing until it is closed. A chat
    past either limit is refused until one closes.
  - Each chat presents the token to the daemon bound to the identity
    provider that issued it. If the provider's configuration is changed
    to another issuer or client meanwhile, the chat is refused and the
    token is not sent to the new one.
  - If the account's access is revoked while it is signed in, its open
    conversations can't be closed with its own authority, and nobody else
    can reach them. The daemon closes such a conversation once its chat
    has been gone for `wss.orphaned_session_timeout_secs` (30 minutes by
    default), if it is idle, has no reply or approval pending, and its
    saved transcript is up to date. Setting the timeout to `0` turns this
    off, and what the dashboard could not close then stays live until the
    daemon restarts.
  - If the daemon takes more than ten minutes to create a chat's
    conversation, the chat gives up, but the dashboard keeps waiting for
    the daemon's answer, and the creation counts against the account's 32
    until it comes. A conversation created after all is closed when it
    arrives, and then no longer counts, whatever the timeout above. A
    conversation the dashboard could not close, or whose creation lost its
    connection to the daemon before the answer came, counts until the next
    daemon reload or restart, since the dashboard can't tell when the
    daemon lets it go.
  - A closed conversation's transcript is kept, but the dashboard can't
    open it. Use zerocode when an account needs history that outlasts its
    chat.

Sign-out ends the session in this ZeroClaw and drops its cookie, also
when the session had already ended; it does not sign the user out of the
identity provider. When a session ends (sign-out, expiry, or a
check it fails) or loses the access a page needs, that page's sockets and
event streams close at once. Signing in again in the same browser ends
the session it held before, whichever account that was, so a later
sign-out leaves nothing of that browser signed in. That holds when two
tabs sign in at once too: browser marker cookies (`zc_browser_<tag>`, or
`__Host-zc_browser_<tag>` over HTTPS at the root; random, `HttpOnly`,
`SameSite=Lax`, and never a credential) tie the browser's sessions
together. Each marker has a name of its own, so a sign-in whose start
is answered late adds a marker rather than replacing one. The sign-in
that finishes later ends the other, and sign-out ends every session
that shares a marker with the browser. A request admitted
before its session ended that has not yet taken effect, such as a
configuration change whose body is still arriving, is refused. A chat
turn already running finishes unattended, as when a viewer leaves (a
scoped account's stops instead, since its conversation is closed), and a
tool approval it was waiting on is refused; nothing the closed socket
sent afterwards is acted on. A
reload also closes the sockets a session opened, and the dashboard
reconnects them under the new configuration. A request that still reaches
the gateway generation being replaced, over a connection that stayed
open, is answered `503` with `"reason": "gateway_restarting"`, and the
dashboard retries it.

Sessions are held in memory, and a session's token is wiped when the
session ends. Expired sessions are swept every minute even when no
request arrives. A daemon restart signs everyone out, while a reload
keeps sessions and verifies each one again against the new configuration
before its next use. At most 256 sessions
are held at once, eight per account; at the cap a new sign-in is refused
rather than signing somebody else out.

Things to know before turning it on:

- Pairing keeps working alongside sign-in, and stays the way back in if
  the identity provider is unreachable. The desktop app keeps pairing.
- An administrator can pair devices from the dashboard like any operator.
  Removing their access at the identity provider does not revoke devices
  they paired; revoke those on the pairing page.
- Conversations and memory created before sign-in belong to the shared
  operator. Accounts without `admin` never see them.
- Over plain HTTP on the gateway's own machine, browsers do not keep
  cookies apart by port, so any other local web server the browser visits
  is sent the session cookie. Use HTTPS, or sign in only from a browser
  you don't use for other local development servers.
- Taking a provider off `gateway.dashboard_sign_in.oidc`, or emptying it,
  ends the sessions it issued: at the next request, and within a minute
  if none arrives (at once when a reload applies the change). Offering it
  again does not bring them back, except when it is offered again within
  that minute with no request in between. If the `[oidc.<alias>]` entry is pointed at another issuer or
  client, its sessions end as well, without their tokens being sent to
  the new provider.
- `session_lifetime_secs` caps sessions that start after it is set;
  sessions already signed in keep the cap they started with. To end them
  sooner, take the provider off `gateway.dashboard_sign_in.oidc`.
- An identity provider that cannot be reached, or answers with an error,
  when a session's token is verified again ends that session (it fails
  closed). That happens at each introspection revalidation deadline, and
  in either validation mode after a configuration reload or a change to
  the OIDC, user or permission-profile configuration. The dashboard
  reports the session as expired; users sign in again once the provider is
  back (#12096).

## Credential lifecycle

- **Expiry** ends the connection's authorization at the deadline; the
  client re-initializes with a fresh token.
- **Introspection revalidation**: OIDC introspection identities carry a
  revalidation deadline; past it, the next operation is refused until the
  client re-initializes (which re-verifies against the IdP).
- **Pairing revocation** applies before the connection's next operation.
- **Log subscriptions**: an open `logs/subscribe` or `events/subscribe`
  stream is rechecked on every delivery and ends at the first one after its
  credential expires, its pairing is revoked, or its principal loses
  `Logs:Read` or administrator rights.
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

Every authenticated principal gets PRIVATE memory: their memory operations
read and write a per-principal plane whose owner travels in every storage
statement, composed with the agent, namespace and tenant dimensions (the
same key under two agents is two rows). The plane follows the principal's
identity, not the admin bypass: a named administrator's memory is their
own private plane, so promoting or demoting a user never hides their notes
or redirects their writes. Only the unauthenticated shared operator is on
the shared plane by default; a caller with the admin bypass may name
`plane = "shared"` on a `memory/*` request explicitly, which is audited,
and a scoped principal cannot.

The two planes are untouchable from each other in both directions: a
shared write can neither name a private row's storage key nor overwrite a
private row, a private write never converts a shared row, ordinary exports
and the markdown snapshot carry shared rows only, and the legacy bulk
purges reach shared rows only. Private rows are exported and purged
through owner-carrying operations.

A session created by a principal has its memory handle pinned to that
principal's private plane for the session's whole life, whoever prompts it
later (an administrator restoring a reaped session restores it on the
durable owner's plane), so the memory tools and per-turn recall inside a
scoped session never touch the shared plane. There is no grant that opens
the shared plane to a scoped session. Private writes pass the same content
scanning and policy gates as shared writes, and private operations are
audited with the full scope when memory auditing is enabled. On memory
backends without principal support
(markdown, lucid, postgres, qdrant today) private memory fails closed with a
clear denial rather than silently un-scoping, which for a scoped session
means its memory tools refuse.

## Migrating from [security.nevis]

The Nevis IAM integration was removed. It was never wired into any
authentication path, so enabling it never authenticated anyone; retiring
it changes no live behavior. `[security.nevis]` is no longer a config key:
schema V4 retires it. While a config still holds the table, every command
prints a warning naming it, and none of its values are loaded. The table is
removed from `config.toml` by `zeroclaw config migrate`, and by the next
save that follows an edit, whether a full save or the incremental save the
CLI, dashboard and RPC use for a single edit. An incremental save runs only
when something was edited, so an untouched config keeps the table until one
of those happens. The removal touches only that table; comments and other
sections keep their bytes. Nothing is converted automatically: the
replacement stack needs values (an audience, profile grants) the old table
never held.

**ZeroClaw keeps two copies of the original.** The first time anything
rewrites a V3 file (`zeroclaw config migrate`, or the first save from the
CLI, dashboard, RPC or Quickstart), it copies the file to
`config.toml.v3.backup`. That copy is created readable only by you, is
never replaced, and is the way back to an older ZeroClaw. `config migrate`
also writes `config.toml.backup`, which the next migration replaces. Both
copies keep the retired table, including any plaintext `client_secret`.
Every save also writes `config.toml.bak` for the duration of the write and
removes it once the write is durable, so a `.bak` is not a backup.

**After you upgrade**, treat `config.toml.v3.backup`, `config.toml.backup`
and any copy you kept yourself as secret material. Delete them once you no
longer need to downgrade, and revoke the Nevis client credential at the IdP.

Field by field:

| `[security.nevis]` field | Replacement | Notes |
|---|---|---|
| `enabled` | none | Presence of an `[oidc.<alias>]` entry is what enables verification. |
| `instance_url`, `realm` | `[oidc.<alias>] issuer` | The exact issuer URL the IdP advertises in its discovery document; the daemon refuses a discovery document whose `issuer` differs. |
| `client_id` | `[oidc.<alias>] client_id` | Also used to build the `interactive_clients` / `service_clients` lists that classify who is signing in. |
| `client_secret` | `[oidc.<alias>] client_secret` | Encrypted on disk through the secret store when `[secrets] encrypt` is on. Required for `validation = "introspection"` and for `client_credentials` service sign-ins. Not copied over from the old table: set it again. |
| `token_validation = "local"` | `validation = "jwks"` | The JWKS URI comes from the issuer's discovery document. |
| `token_validation = "remote"` | `validation = "introspection"` | Confidential client required. |
| `jwks_url` | not supported | There is no custom JWKS override: the key-set URI must be advertised as `jwks_uri` by the configured issuer’s discovery document. That URI may point to a different endpoint; an unadvertised key-set URI cannot be configured. |
| (none) | `[oidc.<alias>] audience` | **Required, and new.** The `aud` the IdP puts in access tokens for this daemon; there was no equivalent to carry over. |
| `role_mapping[].nevis_role` | `[oidc.<alias>] claim_path` + `profile_map` | `claim_path` names the claim that carries the role (e.g. `groups`); each `profile_map` key is a claim value, each value a `[permission_profiles.<alias>]` name. Matching is exact, not case-insensitive as before. |
| `role_mapping[].zeroclaw_permissions` (tool names, or `"all"`) | `[permission_profiles.<alias>] allowed_tools` + `grants.tools = ["execute"]` | Profiles are deny-by-default: `allowed_tools` composes with a coarse `tools = ["execute"]` grant, and every other resource class (`sessions`, `config`, `memory`, …) must be granted explicitly. `"all"` is closest to `admin = true`, which is much broader than "all tools" was; prefer listing resource grants. |
| `role_mapping[].workspace_access` | not equivalent | There is no per-workspace grant. The nearest control is `allowed_agents`, which scopes a profile to agent aliases. |
| `require_mfa` | `[oidc.<alias>] require_mfa` and/or `required_acr` | |
| `session_timeout_secs` | `max_auth_lifetime_secs` (JWKS) / `revalidation_secs` (introspection) | |

Validate before you restart, so a typo does not lock everyone out (see
**Recovery** under *Local connections* for the way back if it does). Two
routes do that:

- Write the new `[oidc.<alias>]` and `[permission_profiles]` entries
  through the daemon's RPC config methods (zerocode's config editor). The
  daemon validates the authorization sections before anything is saved or
  swapped in, so a dangling `profile_map` target or a missing `audience`
  is refused with the reason and the previous policy stays in force.
- If you edit `config.toml` by hand, run
  `ZEROCLAW_AUTH_TOKEN="$(zeroclaw oidc login <alias>)"` from the CLI
  first: it loads the edited file, refuses an `[oidc.<alias>]` entry that
  does not validate (issuer, audience, validation mode, client fields),
  and on success performs a real sign-in against the IdP with the
  configured client, so the issuer, client id and secret are known good
  before the daemon restarts on them. This checks the entry, not the
  `profile_map` targets: a `profile_map` value that names no
  `[permission_profiles]` entry is caught only by the daemon, which then
  refuses to compile the policy and starts in the deny-all state
  described under **Recovery**, so check those names by eye before the
  restart, or use the RPC route.

Remote connections that present no credential are refused throughout;
neither route opens one.

**Rolling back** is a config-data operation, not a code revert: reverting
the binary restores the old types, but a table already removed from
`config.toml` is not restored by anything. Put back the protected copy
you took before the upgrade (or re-add the table by hand). The schema
version does not change for this retirement, so a downgraded binary loads
the restored file without a migration step.

## What this layer does not do (yet)

Consolidation and governance derive shared-plane rows and do not run for
private sessions, and administrative access into another principal's
private memory has no surfaced pathway yet (deny-by-default).
The session-data tools have no principal-aware view yet, so they are
withheld from principals without `admin` (see
[Permission profiles](#permission-profiles)) instead of being scoped to the
caller's own sessions, and scheduled agent jobs, SOP runs, and peer turns can
still call them unless the agent's risk profile excludes them.
`sops/runs` and `sops/run-detail` return the run history of every
procedure to a principal holding `Sops:Read`, whichever agents it ran as,
unlike cron history. Gateway HTTP routes outside the configuration
group keep their existing pairing checks, which a dashboard sign-in
satisfies only for an administrator, and channel identities do not
resolve into this principal model.

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

### Keycloak access-token client attribution

JWKS verification accepts a signed `azp` as the OAuth client identifier when a
typed access token has no `client_id` and the configured client declaration or
`actor_claim` establishes a human actor. The issuer, signature, token purpose,
audience, lifetime and actor-classification checks still apply. An `azp`-only
token cannot authenticate a service, including one classified by `actor_claim`;
service actors still require `client_id`. A blank `client_id`, contradictory
`client_id` and `azp`, or a client with no declaration or configured actor-claim
evidence fails closed. Introspection still requires its `client_id` response
field.
