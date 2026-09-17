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

A client may forward its shell environment in `initialize` so the
daemon's subprocesses see its `PATH` and credential sockets (see
[Environment variable pass-through](../zerocode/environment.md)). That
snapshot is kept only for the trusted shared operator on a local
connection. A roster principal, and every remote connection, gets the
daemon's own environment instead.

#### Recovery

There is no remote recovery path. A remote authentication bypass is never
offered, so every route back from a lockout runs on the host that runs the
daemon. Which route applies depends on whether the authorization policy
still compiles.

**Locked out of a policy that compiles.** Authorization is live and the
local trusted path is intact. Connect locally as the daemon's own uid:
with `security.trust_daemon_uid = true` (the default) that account is the
trusted shared operator whatever the roster says. Repair the offending
entry, for example `zeroclaw config set users.alice.uid 1001`, and the
change is compiled and published at save time.

**Locked out by a deny-all accepted state.** The policy did not compile,
so the accepted state refuses every principal before resolution runs, the
daemon's own uid and the shared operator included. No RPC repairs it:
`config/set` and `config/reload` are refused along with everything else.
Edit `config.toml` directly as its owner, then restart the daemon so the
repaired sections are compiled and published. The daemon ignores `SIGHUP`,
so a restart is the step that reloads it.

If `security.trust_daemon_uid` is set to `false`, the first route is gone
too and both states repair the same way: edit `config.toml` as its owner
and restart. Turn the setting off only where that is acceptable.

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

## Permission profiles

{{#config-fields permission_profiles}}

Profiles are deny-by-default: an unlisted resource is refused, an empty
selector list grants no instances, and broad access requires the explicit
`"*"` selector or `admin = true`. Multiple profiles merge by union.

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
   `auth_token`. A referenced file that any other account can read is
   refused rather than used.

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
audited with the full scope. On memory backends without principal support
(markdown, lucid, postgres, qdrant today) private memory fails closed with a
clear denial rather than silently un-scoping, which for a scoped session
means its memory tools refuse.

## What this layer does not do (yet)

Consolidation and governance derive shared-plane rows and do not run for
private sessions; administrative access into another principal's private
memory has no surfaced pathway yet (deny-by-default); gateway HTTP routes
keep their existing pairing checks; and channel identities do not resolve
into this principal model. The daemon's own uid and the shared operator
retain full access throughout, so single-operator installs behave exactly
as before.
