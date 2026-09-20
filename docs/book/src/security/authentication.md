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

## Permission profiles

{{#config-fields permission_profiles}}

Profiles are deny-by-default: an unlisted resource is refused, an empty
selector list grants no instances, and broad access requires the explicit
`"*"` selector or `admin = true`. Multiple profiles merge by union.

One current limitation is deliberate: per-tool selectors are not yet
enforced inside agent sessions, so a principal whose `allowed_tools` is
constrained (neither `admin` nor `"*"`) is **refused** `session/new`
rather than silently under-enforced. Grant `allowed_tools = ["*"]` until
the session-assembly change lands.

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

## What this layer does not do (yet)

Session and memory records are not yet principal-owned (that storage
boundary is its own tracked change), gateway HTTP routes keep their
existing pairing checks, and channel identities do not resolve into this
principal model. The daemon's own uid and the shared operator retain full
access throughout, so single-operator installs behave exactly as before.
