# MCP

ZeroClaw is an MCP client: it connects to external [Model Context Protocol](https://modelcontextprotocol.io) servers and exposes their tools to the agent. Each MCP tool is namespaced as `<server>__<tool>` (for example `filesystem__read_file`), so tools from different servers never collide.

## Configure MCP

MCP support is enabled by default, but no external MCP tools are exposed until at least one server is configured under `mcp.servers` and an agent is granted that server through its `mcp_bundles` (see Per-agent server scoping below). Configure through the gateway, zerocode, or `zeroclaw config set`:

```sh
zeroclaw config set mcp.servers.filesystem.command npx
```

Set `mcp.enabled = false` to disable MCP tool loading without removing server definitions.

## Per-agent server scoping (`mcp_bundles`)

A `[[mcp.servers]]` entry only *defines* a server. Which servers an agent actually connects to is decided by that agent's `agents.<alias>.mcp_bundles`, and the model is secure by default: omission is not a grant.

- An agent with no `mcp_bundles` connects to **no** MCP servers, even when `mcp.servers` is non-empty.
- A bundle `[mcp_bundles.<alias>]` names the servers it grants by their `mcp.servers` `name`, with an optional `exclude` list. An agent's grant is the union of the servers across every bundle it references, minus any name excluded by any of those bundles (deny wins).
- An unknown bundle alias, or a bundle server name that matches no configured server, grants nothing. Both fail closed and are reported as non-fatal warnings by config validation, so a typo narrows an agent's access instead of widening it.

```toml
[[mcp.servers]]
name = "filesystem"
command = "npx"

[mcp_bundles.files]
servers = ["filesystem"]

[agents.assistant]
mcp_bundles = ["files"]   # connects to `filesystem`; an agent without this gets no MCP servers
```

- Bundle changes take effect on **session restart**. The resolver (`Config::mcp_servers_for_agent`) runs at session/agent construction time; editing `[mcp_bundles.*]` or `agents.<alias>.mcp_bundles` while a session is live does not change that session's connected servers. End and restart the affected sessions to pick up new grants.

This is the *connection* boundary (which servers an agent talks to at all). The `allowed_tools` / `excluded_tools` controls below are the per-tool *capability* boundary applied on top of whatever a granted server exposes.

## Transports

A server is reached over one of three transports (the `transport` field):

| Transport | When to use | Required fields |
|---|---|---|
| `stdio` (default) | A local process you spawn (a Node.js or Python MCP server) | `command`, optional `args`, `env` |
| `http` | A remote server speaking MCP over HTTP POST | `url`, optional `headers` |
| `sse` | A remote server speaking MCP over HTTP + Server-Sent Events | `url`, optional `headers` |

`env` (stdio) and `headers` (http/sse) are stored as secrets; `headers` commonly carries the `Authorization: Bearer …` token for the upstream server.

For `sse`, advertised message endpoints and HTTP redirects must stay on the
configured URL's origin: the same scheme, host and effective port. Relative
endpoints and same-origin absolute endpoints are supported. Cross-origin
endpoints, HTTPS downgrades and URLs containing credentials are refused before
headers or request content reach that destination. Configure authentication in
`headers`, not URL userinfo. If a server uses a separate origin for messages,
serve that endpoint through the configured origin instead. This check binds a
destination to the configured origin; it does not restrict the operator's
initial URL or pin DNS addresses.

Add a server through the gateway, zerocode, or `zeroclaw config set` (for example `zeroclaw config set mcp.servers.filesystem.command npx`). A stdio server needs `command` plus optional `args`/`env`; an http/sse server needs `url` plus optional `headers`. The per-field commands are in the field table below.

### Example: Parallel Search

[Parallel Search MCP](https://docs.parallel.ai/integrations/mcp/search-mcp)
provides public web search and page extraction without a Parallel account or API
key. Free access is rate limited. Its Streamable HTTP endpoint uses ZeroClaw's
`http` transport:

```toml
[[mcp.servers]]
name = "parallel"
transport = "http"
url = "https://search.parallel.ai/mcp"
headers = { "User-Agent" = "ZeroClaw" }

[mcp_bundles.web]
servers = ["parallel"]

[agents.assistant]
mcp_bundles = ["web"]
```

The `User-Agent` identifies ZeroClaw project-wide so Parallel can measure
aggregate integration usage to understand adoption and support it. Keep this
header on HTTP requests if the transport changes. Preserve existing headers;
if you already set a `User-Agent`, append `ZeroClaw` to its existing project and
HTTP client tokens instead of replacing them.

Merge these entries into your existing `config.toml`, using the alias of the
agent you want to grant access. Add `"web"` to that agent's existing
`mcp_bundles` list rather than replacing its other grants. If the `web` bundle
already exists, add `"parallel"` to its `servers` list. Keep `mcp.enabled = true`
and restart the affected session after changing grants.

The agent can then use `parallel__web_search` and `parallel__web_fetch`, subject
to its normal tool authorization and approval policy. Calls send queries,
requested URLs, and any supplied objective or context to Parallel. Once granted
access, the agent may choose these tools during its work. To revoke access,
remove `"parallel"` from every bundle granted to that agent, or add it to a
granted bundle's `exclude` list, then restart the session.

## Editing servers

Three surfaces edit the same `[[mcp.servers]]` table:

- **`config.toml`**: hand-edit the keys documented below. The full table is round-tripped on save.
- **zerocode TUI** (`/config` -> `mcp.servers`): first-class per-field editor. The section shows one row per server, labeled with the server's `name`; enter a row to edit `transport`, `command` / `url`, `headers`, `env`, and `tool_timeout_secs` as individual fields. `+ Add` creates a new entry seeded with the name you supply; deleting from the alias list removes the entry. The `name` field is not edited inline because renaming the natural key mid-edit would invalidate in-flight references; use the dashboard or hand-edit `config.toml` to rename for now.
- **Web dashboard** (Config -> MCP servers -> a server): the same per-field surface as the TUI, one form per server.

`env` and `headers` are key/value maps, so both UIs show them as a container row followed by one row per entry (`env.GITHUB_TOKEN`, `headers.Authorization`, …):

- **Add an entry**: in zerocode, select the `env` (or `headers`) row showing `[+ Add entry]`, press Enter, type the name, press Enter, then type the value (input hidden) and press Enter. In the dashboard, use the name/value inputs and **Add entry** under the map. The entry is saved immediately and encrypted at rest.
- **Change a value**: edit the entry's own row like any other secret.
- **Remove an entry**: delete the entry's row (zerocode's delete-row key, or the dashboard trash button and then Save).

Values are never displayed; the container row lists only entry names. From the CLI, `zeroclaw config set mcp.servers.<name>.env.<KEY>` adds or replaces an entry, and setting it to an empty value removes it. The bare map path (`mcp.servers.<name>.env`) cannot be set directly. The same editor applies to every secret key/value map in the config (for example provider `extra_headers`).

## Server fields

Per-server fields (`[[mcp.servers]]`), generated from the schema:

{{#config-fields mcp.servers}}

`tool_timeout_secs` is an optional per-call timeout; it must be greater than 0 and is capped at 600 seconds.

### Custom CA trust

For an HTTP or SSE server whose certificate is issued by a private CA, set
`tls_ca_cert_path` to an absolute path containing one or more PEM-encoded CA
certificates. The configured certificates are added to the default trust store;
certificate-chain, expiry, and hostname verification remain enabled.

A relative, missing, unreadable, empty, oversized, non-regular, or invalid CA
file is a hard connection error for that server. The path must resolve to a
regular file no larger than 1 MiB. Symlinks are followed, so certificate
rotation and mounted-secret layouts that publish the bundle through a symlink
work as configured; the resolved file is validated after it is opened, and a
symlink that resolves to a directory, device, or FIFO is rejected. ZeroClaw
never disables verification or silently
falls back when this field is set. The configured server URL and any message
endpoint advertised by an SSE server must use `https://`; plaintext URLs and
downgrade redirects are rejected before request headers or content are sent.
The value is applied when the MCP session starts; restart the affected session
after changing it. Remove the field and restart the session to return to the
default trust store. Stdio servers ignore it.

## Remote server sign-in: OAuth (`oauth = true`)

Remote (`http`/`sse`) servers that require login can authenticate through OAuth
instead of a hand-managed `Authorization` header (config validation rejects
setting both). Set `oauth = true` on the
server; the transport then runs the authorization-code flow with PKCE (S256,
`state`-bound) against whatever the server itself advertises. Nothing is
hardcoded per provider: the protected-resource URL comes from the server's
RFC 9728 `WWW-Authenticate` / well-known metadata, the authorization server
endpoints from RFC 8414 metadata, and scopes are requested verbatim from
`oauth_scopes`. Setting `oauth` (or its sub-fields) on a stdio server is
rejected by config validation. Discovery follows the MCP contract in order:
the `resource_metadata` from a 401 challenge when present, then the RFC 9728
well-known path form, then the root well-known form, and, if no document was
found at all, one anonymous probe GET of the configured endpoint to ask the
server for its challenge, followed by one anonymous `initialize` POST when
that endpoint answers 405 (a streamable-HTTP endpoint may emit the challenge
only over POST). An advertised hint that fails the URL policy is
reported, never fetched. Authorization-server documents are read from the
RFC 8414 well-known form or the OpenID Connect `openid-configuration` forms,
with issuer identity verified against the advertised identifier in every
case. An endpoint such as a login service hosted on a different origin than
the `issuer` its document declares is accepted (RFC 8414 permits it) after
the same URL and pinned-destination checks, with a warning naming both
origins. Only when the resource challenges but publishes no protected-resource
document anywhere does ZeroClaw accept authorization-server metadata hosted
by the resource's own origin, and only if that document declares exactly
that origin as its `issuer` (logged with a warning). An error status that is
not a challenge attests only that the origin spoke, so it does not unlock that
fallback; the warning names both parties, and every candidate miss stays in the
connect error when discovery ultimately fails. Each discovery pass
runs under a 45-second budget with a bounded issuer candidate list, so a
misconfigured metadata host cannot stall the rest of the connect queue.

First sign-in: ZeroClaw registers itself as a public PKCE client through RFC
7591 dynamic registration when `oauth_client_id` is unset; if the server has
no registration endpoint, set `oauth_client_id` to a client id you registered
there by hand as a **public** (no-secret, PKCE) client with token-endpoint
auth `none` (ZeroClaw never manages client secrets; the id is then used as-is
and dynamic registration is skipped). It prints the authorization URL and also
tries to open your browser; a failed browser launch is advisory only, the
printed URL is the fallback by design. The callback is awaited on an
ephemeral loopback port (`http://127.0.0.1:<port>/callback`, never a
non-loopback listener), waiting up to 5 minutes. The callback must carry
`state`; when the authorization server advertises RFC 9207 support, the
response must also carry a matching `iss`. The exchanged grant is bound to the
canonical resource URL (RFC 8707 `resource` is sent on every token request),
so a token minted for one MCP server is never replayed at another.

Sign-in only runs from an **interactive** ZeroClaw. The entry point that
owns the human must declare the posture: the foreground `zeroclaw` run does,
while daemon workers, the gateway, the channel orchestrator, cron, eval, ACP,
delegated children, and streamed dashboard/RPC sessions never do, even when
their process owns a terminal. The declaration must also be true in fact:
both stdin and stderr attached to a terminal. Any assembly that fails either
test never prints a URL, never binds a callback port, and fails a first
sign-in with an actionable "connect once from an interactive session" error
instead. Note in particular that `zeroclaw daemon` started in the foreground
of a terminal is not interactive by this rule: its turns answer channel and
dashboard traffic, so a sign-in attempted from one of them fails fast
instead of holding that turn up to the 5-minute wait. The division is
workable, not a dead end: a running daemon re-reads the credential store
whenever it holds no live grant, so signing in once from an interactive CLI
lets already-running daemons and gateways pick the grant up on their next
connect without a restart.

Token lifecycle: tokens are refreshed proactively before expiry and once per
401; a 401 challenge that advertises a different metadata location triggers
one metadata re-discovery per server and process (RFC 9728 §5.2), later ones
are treated as token noise. On the `http` transport the request is then retried exactly once when
the method is safe to replay (`initialize`, `tools/list`, resource/prompt
reads); a `tools/call`, and every POST on the `sse` transport (the request is
bound to stream bookkeeping there), is never replayed: the call fails as a
plain HTTP 401 error while the refreshed grant applies to your next call, so
a mutation cannot run twice. A refresh response that omits
`refresh_token` keeps the stored one (RFC 6749 §6). A terminal `invalid_grant`
clears the stored grant and asks for a new sign-in. If the authorization
server behind a stored grant changes (discovered issuer no longer matches the
issuer that minted it), ZeroClaw drops the grant rather than sending its
tokens to the new server. A stored profile written by a DIFFERENT sign-in (the
interactive CLI signing the resource in while the daemon runs, for example) is
not deleted by that verdict, and when it was minted by the new server it is
adopted instead of asking you to sign in again. ZeroClaw does not call the
authorization server's revocation endpoint (RFC 7009) when it drops or replaces
a grant, so server-side sessions live out their own expiry; revoking at the
provider always wins on the next call, and client-initiated revocation is a
tracked follow-up.

Storage: grants (access token, optional ID token, refresh token, client id,
issuer, returned scope) live in the encrypted auth-profile store beside your
`config.toml`, keyed per MCP resource, and never appear in logs or errors. OAuth **refuses to run** without
encrypted secret storage: with `secrets.encrypt = false` the manager fails
closed with an actionable message instead of persisting plaintext refresh
tokens. While that sign-in exists every OTHER write to the profile store keeps
its tokens sealed even if `secrets.encrypt` is turned off afterwards: pasting a
provider token or logging out a different sign-in re-saves the same file, and a
signed-in MCP grant is written back encrypted rather than dropped into
plaintext. The requested `oauth_scopes` are sent verbatim; the scope string the
AS returns is stored but not subset-checked, so the AS is the authority on
what the token can do. A rotation the running daemon performs after you signed
the same resource in at a DIFFERENT authorization server or with a different
client (the CLI, for example) does not overwrite that sign-in: the stored
profile is left exactly as the newer one wrote it, and the daemon keeps its own
rotated token in memory. A newer sign-in at the SAME authorization server under
the same client id is a newer token set for the same party, and a rotation of
the older grant still replaces it.

Security posture (all enforced before any token leaves): MCP OAuth URLs must
use `https` except on loopback; discovery and metadata documents are fetched
with redirects disabled, and with `oauth = true` transport redirects must
stay on the configured origin (a bearer never rides a redirect, including a
same-host plaintext downgrade); DNS answers run through the crate-wide SSRF guard:
every answer must be globally routable unless the target's host is the
configured MCP host itself, cloud-metadata addresses (the whole IPv4
link-local band, the documented provider endpoints, and anything embedded
under the well-known `64:ff9b::/96` or a declared `security.nat64_prefixes`
translator) are refused in every case; the validated answers are then
pinned, closing the resolve-then-connect rebinding window; a private or
loopback target is trusted only when its host equals the configured MCP
host (the loopback spellings `localhost`, `127.0.0.1`, and `::1` count as
one identity). The confined-destination rule in the transport notes above
applies to OAuth servers too, and with `oauth = true` the configured SSE
URL must itself be scheme-safe (https, or loopback plaintext), so a
compromised server cannot move your session headers or bearer to another
host and cannot downgrade an OAuth token in transit. The MCP bearer rides
only to the configured origin; sign-in traffic itself goes to the
discovered authorization server, which may legitimately be a different
host. A runtime
HTTP proxy that would capture MCP traffic fails the connection closed rather
than routing it around the DNS pin. `oauth` is authentication only: it does
not change
`mcp_bundles`, tool risk profiles, `allowed_tools` / `excluded_tools`,
`auto_approve`, or `always_ask`.

### Example: Silpo (personal-account MCP server)

```toml
secrets.encrypt = true

[[mcp.servers]]
name = "silpo"
transport = "http"
url = "https://mcp.silpo.ua/mcp"
oauth = true
oauth_scopes = ["mcp:tools"]

[mcp_bundles.groceries]
servers = ["silpo"]

[agents.assistant]
mcp_bundles = ["groceries"]
```

**Warning:** this server fronts a personal grocery account. Its tools expose
personal data (addresses, order history, bonuses) and can mutate real state
(checkout, payment, cart). Those mutations remain fully subject to the normal
MCP capability and approval policy above: OAuth authenticates the connection,
it does not auto-approve anything. This exact shape was exercised against a
live Silpo account during development (interactive sign-in, encrypted grant
reuse on the next start, and tool reads through a headless daemon); CI still
exercises the flow against local mock servers only, so your first connection
is live on your machine through the interactive sign-in above.

### Trying the flow without a real server

Any local OAuth-capable MCP server works as a smoke test, e.g. one served at
`http://127.0.0.1:8000/mcp` (loopback is the one place plain `http` is
allowed):

```toml
[[mcp.servers]]
name = "localmock"
transport = "http"
url = "http://127.0.0.1:8000/mcp"
oauth = true
```

Run an interactive `zeroclaw`: sign-in starts when the server's transport
connects (the bearer is fetched before `initialize`), typically at session
start; complete the browser step (or paste the printed URL) and confirm the
tool list resolves. Expected failure shapes:
no authorization URL printed and no browser opened → you are not in an
interactive session (both stdin and stderr must be TTYs; piping stderr into
a log disables sign-in); the callback
never arrives → an AS configured with a different redirect URI, or a personal
firewall blocking loopback listens (wait times out after 5 minutes);
`invalid_client` → the AS rejects dynamic registration, register a public
(no-secret) client by hand and set `oauth_client_id`; a later 401 loop or
`invalid_grant` → the
stored grant was revoked server-side; the grant is cleared and a fresh sign-in
is requested automatically. Changing the AS URL behind a stored grant likewise
signs the server out by design. To sign out manually, delete the server's
`mcp:oauth@…` profile from `auth-profiles.json` in the config directory while
ZeroClaw is stopped.

Aliases of one URL share one grant: the stored grant is keyed by the
canonical MCP resource URL, not by `name`, so running two `[[mcp.servers]]`
entries with the same URL (different names, different bundles) does not
create per-user isolation; the second sign-in replaces the first
account's tokens. Use a separate ZeroClaw instance or config directory per
person until explicit credential slots or per-principal credentials exist.

## Top-level fields

{{#config-fields mcp}}

## Deferred loading

`mcp.deferred_loading` is `false` by default, so configured MCP tools are included in the model context eagerly. Set it to `true` to place only MCP tool **names** in the system prompt; the LLM calls the built-in `tool_search` tool to fetch a tool's full schema before invoking it. This keeps the initial context window small when a server exposes many tools.

## Security and approval

MCP tool calls go through the same approval gate as every other tool, governed by the agent's risk profile (`risk_profiles.<alias>`). The `tool_search` discovery step is auto-approved so deferred MCP loading can work in non-interactive sessions, but tools discovered from MCP servers still follow the normal approval policy:

- At autonomy `level = full`, uncovered tool calls do not prompt (MCP tools included). A name (or `"*"`) in `always_ask` still prompts, including under Full.
- Otherwise, an MCP tool call prompts for approval unless its **prefixed** name (`<server>__<tool>`) is in the profile's `auto_approve` list. `auto_approve = ["*"]` approves everything; an exact entry like `auto_approve = ["filesystem__read_file"]` approves just that tool.
- `always_ask` is the inverse: a name (or `"*"`) there always prompts, overriding `auto_approve` and Full autonomy.

### Authorization: `allowed_tools` / `excluded_tools`

Approval gates *when* a tool call needs a human green-light. Authorization gates *whether* the agent can call a tool at all. The two are independent.

Keep the three MCP tool controls on their own axes:

| Control | Scope | Use it for |
|---|---|---|
| `tool_filter_groups` | Prompt/context exposure | Decide which MCP tool schemas are visible to the model for a turn. |
| `auto_approve` / `always_ask` | Approval policy | Decide whether a selected MCP tool call requires operator approval. |
| `allowed_tools` / `excluded_tools` | Capability policy | Decide which prefixed tool names the risk profile may use at all. |

For runtime-discovered MCP tools the capability contract has an MCP-specific exception:

- If the risk profile's `allowed_tools` is omitted or empty, no authorization constraint applies; every discovered tool (MCP or built-in) is reachable; an empty list is the legacy unrestricted state, not deny-all. For an explicit deny-all gate, set the sibling `deny_all_tools = true`: it denies MCP tools too (there is no `__` auto-admit under deny-all) and cannot be combined with a nonempty `allowed_tools`.
- If `allowed_tools` is non-empty, any MCP tool whose name contains `__` (the `<server>__<tool>` convention) is auto-admitted into the effective allow-list without being listed there individually. Non-MCP built-ins still need an exact entry.
- `excluded_tools` always subtracts, including from the auto-admitted MCP set. To block a single MCP tool like `filesystem__write_file` while keeping the rest of the `filesystem` server reachable, put it in `excluded_tools`.

The rationale: before this exception, every agent that pinned an `allowed_tools` list to lock down its built-in surface would silently lose every MCP tool, even ones the operator explicitly configured. The cost is that the deny-list is now the operator's primary lever for blocking destructive MCP capabilities under an allow-list-pinned profile.

If you want the strict pattern from before this change, where you only admit MCP tools you list explicitly with no `__` auto-admit, combine an explicit `allowed_tools` entry with an `excluded_tools` entry per destructive sibling you need blocked:

```toml
[risk_profiles.assistant]
allowed_tools = [
  "file_read",
  "filesystem__read_file",
]
# Block the destructive sibling that would otherwise be auto-admitted via
# the `__` exception above.
excluded_tools = [
  "filesystem__write_file",
]
auto_approve = [
  "filesystem__read_file",
]
```

The MCP `__` auto-admit exception is scoped to the **risk profile**'s `allowed_tools` only. Caller-supplied per-run allow-lists, like a cron job `allowed_tools` or any other narrowed invocation that passes an explicit list into the runtime, are still treated as strict explicit-list intersections, with no `__` auto-admit on top. A cron job that narrows itself to `allowed_tools = ["cron_add"]` will not surface `filesystem__write_file` to the model even when the agent's risk profile would otherwise auto-admit it via the `__` convention; the per-run narrowing remains a reliable capability boundary regardless of how many MCP servers are configured.

`auto_approve` alone does not hide a tool from the model; it only answers the approval question after the model selects that tool. Use `tool_filter_groups` to reduce prompt noise and `allowed_tools` / `excluded_tools` to enforce a capability boundary.

See [Autonomy levels](../security/autonomy.md) for the full per-profile field surface, and the [Config reference](../reference/config.md#mcp) for every MCP field and default.

## MCP Resources and Prompts

In addition to MCP **tools**, ZeroClaw exposes MCP **resources** and **prompts**
from connected servers.

### Tools

Two built-in tools are available (subject to your agent's tool access policy):

- `mcp_resources`: `action: "list"` (optional `server`, `cursor`) lists
  resources; `action: "read"` with `uri` (prefixed `<server>__<uri>`) returns
  contents.
- `mcp_prompts`: `action: "list"` lists prompts; `action: "get"` with `name`
  (prefixed `<server>__<name>`) and optional `arguments` returns the rendered
  prompt messages.

Servers that do not advertise resource/prompt capabilities are skipped, and calls
against them return a clear "does not support" error.

### Pinning resources into context

Each MCP server entry accepts an optional `pinned_resources` field: a list of
resource URIs to read once at startup and inject into the system prompt. Set it
through the same config surfaces used to define the server (the gateway, zerocode,
or `zeroclaw config set`, as shown under [Configure MCP](#configure-mcp)), naming
the resources you want the agent to always have on hand. The field defaults to
empty, so servers without it are unaffected.

Pinned content is read once per run (no live refresh) and is labeled
`trust="untrusted-external"` so the model treats it as data, not instructions.

### Embedded resource blobs in tool results

When an MCP `tools/call` result includes a content item shaped as
`type: "resource"` with a nested `blob` (base64), ZeroClaw does **not** dump that
base64 into the model context. Instead it materializes the bytes under the
session workspace `uploads/` directory (same shared helper and 10 MB limit as
ACP inbound `resource.blob`) and redacts the blob from the model-facing tool output, replacing it
with a `materialized` reference: a `[Document: …]` marker for
non-image mimes, or the plain saved path for image mimes, with the
image declared as an attachment on the tool result rather than
written into the text as a marker. This is
gated by content shape, not by tool name. Materialization does not auto-deliver
the file to ACP clients; the agent still calls `deliver_file` when outbound
delivery is needed. See [ACP `session/prompt` blob intake](../channels/acp.md#sessionprompt).

The two other binary MCP content shapes are mapped as follows. A `type: "image"`
item (base64 `data` + `mimeType`) is materialized the same way and its content
item is rewritten to a text item carrying the plain saved path,
with the image declared as an attachment on the tool result, which the
multimodal pipeline lifts into a native provider image part; the item's
`annotations`/`_meta` and other non-binary fields are preserved. Only the raster
formats the vision pipeline accepts are materialized: PNG, JPEG, WebP, and GIF.
The on-disk extension is derived from the declared `mimeType` (canonicalized to
its case-insensitive essence, parameters dropped) when it names one of those
types, otherwise from sniffing the decoded bytes; the extension must match the
bytes because the loader prefers a path's extension over its magic. An image
whose type is neither a supported declared type nor a recognized supported
signature degrades to `[attachment unavailable: …]` and is not written. A
`type: "audio"` item is **not** materialized in this path, because no provider
resolves an audio path into content parts today, so its `data` is stripped to a
non-materializing `[audio attachment: <mime>]` placeholder. In every case the raw base64 never
reaches the model, including for a malformed image/audio item whose `data` is
empty or not a string.

Because the result comes from an untrusted server, two per-call bounds are
enforced before any payload is decoded, hashed, or written: at most **64**
materializable binary items (resource blobs plus valid image items) per
`tools/call` result, and an estimated aggregate decoded size of at most **10 MiB**
across all of them. A result that exceeds either bound has every such item
degraded to an `[attachment unavailable: …]` marker and writes nothing to disk,
so an array of many empty or tiny items cannot force per-item work. Each
individual payload is still bounded by the same 10 MB per-file limit.

Separate from these *decoded* budgets, each HTTP/SSE MCP server also has a
transport-level response-body cap, `max_response_bytes`, enforced on the raw
*encoded* wire bytes before the body is parsed so a server cannot force an
unbounded read. It is not the 10 MiB decoded limit above: it defaults to
**16,078,168 bytes**, the base64 expansion of the 10 MiB decoded aggregate plus
JSON-RPC envelope headroom, so a valid near-limit blob still reaches
materialization. Set `max_response_bytes` on a server to override it; `0` or
leaving it unset uses that default.

### Security

Resource and prompt content originates from the configured MCP server and is
treated as **untrusted**: it is provenance-wrapped, secret-scrubbed, and
length-bounded before entering context. Access to `mcp_resources` / `mcp_prompts`
and to specific servers is governed by your agent's tool access policy (risk
profile), and narrows correctly when delegating to subagents.

## Example: Build Remote Agent (`gbr`)

[Build Remote Agent](https://grokbuildremote.com/) is a pairing device: a phone
app spectates (and can inject into) this ZeroClaw host through free MIT
`gbr-agent`. Protocol `gbr/1`. Independent product by Linespotting AB. Not
affiliated with xAI or SpaceX.

Run `gbr-agent` on the **host**. Do not copy it into a sandbox. Attach only
loopback Bot API `http://127.0.0.1:8788` or stdio `gbr-mcp`. Phone is spectator
and veto. Never paste mailbox keys as plaintext in `config.toml`. ZeroClaw chat
channels are not `gbr/1`.

Loopback limits *network* exposure. It is **not** process authentication.
Another local process can call `:8788` unless you set `GBR_BOT_REQUIRE_KEY=1`
and give `gbr-mcp` the matching mailbox key through ZeroClaw secret-managed
MCP `env` (not a plaintext mailbox key in `config.toml`).

`gbr-mcp` logs tool-call arguments to `~/.gbr/logs/gbr-mcp-YYYY-MM-DD.jsonl`
at info, default retention 7 days. Secret redaction does not strip ordinary
inject text. For the safe baseline set `GBR_MCP_LOG_BODIES=0` on the `gbr`
server `env` in ZeroCode Config. Delete or disable those logs if you do not
want a second persistence surface outside ZeroClaw history.

Omission of `mcp_bundles` is not a grant: define the server **and** grant it.

Need **Node.js 20+** (`mcp/gbr-mcp` `engines.node >=20`). Pin GitHub Release
**v0.6.2** (checksum the installer, then the binary). Do not paste live
website `curl | bash`. See
[PINNED-INSTALL.md](https://github.com/LinespottingOrg/GrokBuildRemote-Agents/blob/v0.6.2/docs/PINNED-INSTALL.md).

Terminal 1 (leave this process running):

```bash
gbr-agent version    # need v0.6.2
export GBR_BOT_REQUIRE_KEY=1
gbr-agent pair
gbr-agent run
```

After pairing, obtain the mailbox key from the phone (Settings -> Bot API) or
from the host pairing file `~/.gbr/device.json` (`mailbox_key`). The pinned
`gbr-mcp` client reads `GBR_MAILBOX_KEY` from its options or process
environment; it does not load that pairing file. With `GBR_BOT_REQUIRE_KEY=1`,
a fresh shell that omits the key makes unauthorized Bot API requests.

Terminal 2 (MCP install and diagnose; `gbr-agent run` must already be up).
Paste the key into a silent prompt so the value is not stored in shell history:

```bash
git clone --branch v0.6.2 --depth 1 https://github.com/LinespottingOrg/GrokBuildRemote-Agents.git
cd GrokBuildRemote-Agents/mcp/gbr-mcp
node -v    # v20+
npm install --ignore-scripts
IFS= read -rs GBR_MAILBOX_KEY   # paste key, Enter; no echo, no history
export GBR_MAILBOX_KEY
export GBR_MCP_LOG_BODIES=0
node bin/gbr-mcp.js --diagnose
```

The source tag is pinned, but this release has no npm lockfile, so dependency versions are resolved at install time. `--ignore-scripts` prevents dependency lifecycle scripts from running during installation; it does not make the downloaded dependencies trusted or prevent their code from running when the MCP server starts.

The `export` lines above apply only to this diagnostic shell. They do not
configure the ZeroClaw-spawned `gbr` server. Define the server and grant it:

```toml
[[mcp.servers]]
name = "gbr"
command = "node"
args = ["/absolute/path/to/GrokBuildRemote-Agents/mcp/gbr-mcp/bin/gbr-mcp.js"]

[mcp_bundles.gbr]
servers = ["gbr"]

[agents.assistant]
mcp_bundles = ["gbr"]
```

Then set both `GBR_MAILBOX_KEY` and `GBR_MCP_LOG_BODIES=0` on that server's
`env` through ZeroCode Config (`/config` -> `mcp.servers` -> `gbr` -> `env`).
The mailbox key is a secret field: use the masked prompt (encrypted secrets
store) or a 1Password `op://vault/item/field` reference. Do not put the raw
key in `config.toml`. The CLI equivalent is:

```sh
zeroclaw config set mcp.servers.gbr.env.GBR_MAILBOX_KEY
zeroclaw config set mcp.servers.gbr.env.GBR_MCP_LOG_BODIES 0
```

All MCP environment values use ZeroClaw's masked secret prompt, so the
trailing `0` is not consumed as the value. Enter `0` at that prompt.

Restart the affected session after changing bundles or `env`. HTTP without MCP,
after `gbr-agent run` with `GBR_BOT_REQUIRE_KEY=1`:

```bash
IFS= read -rs GBR_MAILBOX_KEY
export GBR_MAILBOX_KEY
curl -sS -H "X-GBR-Key: $GBR_MAILBOX_KEY" http://127.0.0.1:8788/health
curl -sS -H "X-GBR-Key: $GBR_MAILBOX_KEY" http://127.0.0.1:8788/v1/sessions
```

Docs: [BOT-API.md](https://github.com/LinespottingOrg/GrokBuildRemote-Agents/blob/v0.6.2/docs/BOT-API.md)
