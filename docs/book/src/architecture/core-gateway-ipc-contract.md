# Core-to-gateway IPC contract

| | |
|---|---|
| Issue | #11000 (tracker #7432 G1; decision record through #8691 / ADR-007) |
| Status | **Proposed.** Nothing here is accepted. Every decision is *proposed* until the maintainers accept it in the companion [ADR-019](decisions/ADR-019-gateway-core-ipc-contract.md); the register in §17.0 tracks each one. Items marked **OPEN** are decisions for the maintainers and are listed in §17. |
| Baseline | `upstream/master` **f0ae8c8bd8** (`feat(rpc): bound the local transport and add chunked uploads (#11171)`). Every `path:line` below is at that commit unless it names a PR head. |
| PR heads read | See §15.1 (one row per PR with the head SHA that was read). |
| Config shape | Written against today's key names: the split needs no key renames or moves; only the verified-inert `[gateway.pairing_dashboard]` is retired at config V4. §13 states the canonical owner of every key the contract reads. |

**Path shorthand.** `rt/` = `crates/zeroclaw-runtime/src/`, `api/` = `crates/zeroclaw-api/src/`, `gw/` = `crates/zeroclaw-gateway/src/`, `cfg/` = `crates/zeroclaw-config/src/`, `ch/` = `crates/zeroclaw-channels/src/`, `zc/` = `apps/zerocode/src/`, `root/` = the repository root crate (`src/`, `Cargo.toml`).

**Normative words.** MUST, MUST NOT, SHOULD, MAY as in RFC 2119. A MUST that describes current code is a regression guard: a change that breaks it is a protocol change (§7). A MUST that describes new behaviour is a requirement on the named work item.

**Status tags** used on every operation, event and rule:

| Tag | Meaning |
|---|---|
| `[master]` | Implemented at f0ae8c8bd8. Cited. |
| `[PR #N]` | Implemented on that open PR's head (SHA in §15.1). Not merged; may change. |
| `[proposed]` | Required by this contract; not implemented anywhere. Owner work item named. |
| `[OPEN Dn]` | Needs a maintainer decision (§17, ADR-019 "Open decisions for the maintainers"). The contract states the options and a recommendation, never a silent default. |

---

## 0. Summary

1. **Transport.** The gateway talks to the core over the existing local IPC listener: a Unix domain socket or a Windows named pipe carrying NDJSON-framed JSON-RPC 2.0 (§3). No second local server, no HTTP in the core. `[master]`
2. **Three documents, three jobs.** (a) This contract is the normative prose for framing, handshake, identity, lifecycle and semantics; (b) the OpenRPC document generated from `zeroclaw-rpc-proto` (#11165) is the machine-readable method/notification schema set; (c) OpenAPI 3.1 describes only the gateway's external HTTP/WS/SSE surface and is served only by the gateway. OpenAPI does not describe NDJSON framing and nothing in this contract implies it does (§2).
3. **Authority stays in the core.** Every domain operation the gateway performs is an RPC the core authorizes with `Method::authz()` plus its per-method selectors and ownership predicates (§5). The gateway may *narrow* (rate limits, body caps, bind policy) but never *grant*.
4. **Identity.** The gateway forwards each end user's own credential on a connection dedicated to that credential; there is no on-behalf-of parameter anywhere (§5.3). Ingress that carries its own credential (generic `/webhook`, `/sop/*`, plugin and vendor webhooks) is forwarded as *evidence* that the core verifies (§12). Public routes (health, metrics, A2A cards, the config schema, pairing redemption and posture) are served through the gateway's service connection by one allowlisted method each (DEC-25). The gateway's own service identity is **OPEN D1**; pairing-disabled installs are **OPEN D2**, with D2-A fully specified if chosen (DEC-30).
5. **Server endpoint verification before any credential.** A client MUST verify the operating-system identity of the endpoint it connected to before it sends a reusable credential, on every connection (§5.5). This replaces the custom `server_proof` HMAC of the earlier design. A gateway under a separate OS account needs the IPC-directory layout of §5.5.1; which boundaries v0.9.0 supports is **OPEN D11**.
6. **Discovery.** Resolution order is fixed and shared by all clients through one implementation (§4). Two current divergences are defects the contract requires fixed: zerocode ignores `ZEROCLAW_DATA_DIR`, `ZEROCLAW_WORKSPACE` and the Homebrew data directory, and the Windows pipe name is derived with `std`'s unstable `DefaultHasher`.
7. **Versioning.** Protocol version 1 is additive-only, except that security corrections (fail-closed narrowing with a stable refusal signal) and conformance corrections ship inside it and are registered (V-5, V-6; Appendix D classifies every changed method: the original parity PRs' 27 and the gateway route ports' changes, C.6); method-level capability detection uses the `capabilities` list `initialize` already returns; a disjoint protocol refuses with structured data; product-version skew policy is **OPEN D6** (§7).
8. **Turn lifetime.** On master an RPC disconnect cancels the turns that connection started (§9.2). A separate gateway needs session-owned turns (#11185) before its restarts can leave agent work healthy. Every turn gets a core-assigned random-UUID `turn_id`, and the new, capability-advertised `session/cancel-turn` targets it, so a retried cancel never stops a later turn, on an older core or after a core restart (DEC-22). The detached-approval policy is **OPEN D4**.
9. **Webhooks.** Plugin webhooks cross as raw bounded requests into the core's `PluginWebhookRegistry` (native vendor routes join them only if D7 chooses option (b), §12.9); verification and parsing stay in the owning channel or plugin (§12). Concurrent ingress calls on one connection are correlated and cancelled by a gateway-chosen `delivery_id` (DEC-23); generic ingress keys commit at admission and never on a refusal (DEC-24). **Settled here:** webhook deduplication/idempotency is **core-owned** (§12.6), and `gateway.webhook_secret` is **verified in the core** from the forwarded header (§12.7). These keys keep their `gateway.*` names; the core becomes their canonical owner and only reader, and the gateway never reads them (§13).
10. **"No HTTP server in the core"** is defined precisely in §14.3, including three existing core listeners that need a ruling (**OPEN D8**).
11. **Completeness.** The operation catalog (§8) covers all 97 master methods, the 49 the original parity PRs add, the two the gateway route ports add and every proposed method. Appendix C gives the wire shape of every payload the OpenRPC document leaves untyped: the master methods (C.1, C.2), every proposed method (C.3), every method the original parity PRs add, at the heads in §15.1 (C.4), and what the gateway route ports add and change (C.6). Appendix E gives every method's error codes. A PR that changes one of its shapes before it merges updates C.4 in the same review cycle (SCH-6). The two methods master gained after the baseline (#10621: `agents/delete-preview`, `agents/delete`) are in C.5 and E.4; every other count in this document is at the baseline.

---

## 1. Scope

In scope, per #11000's acceptance criteria:

- every operation and event a separate `zeroclaw-gw` needs to serve the supported dashboard and API, including messages/streaming, sessions, installed plugins, agent status, memory and cron (AC1), plus config, pairing, SOP, logs/events, workspace/files, catalog/tools, canvas, cost, skills, personality, quickstart and system operations the current gateway serves (§8, §15);
- how OpenAPI 3.1 relates to the transport (AC2, §2);
- caller identity, authorization ownership, credential handoff, endpoint discovery, version compatibility, errors, cancellation, reconnect and process lifecycle (AC3, §4–§11);
- plugin webhook registration, dispatch and response semantics across the boundary (AC4, §12);
- crate, feature and endpoint names mapped to the current implementation, and supported Unix and Windows behaviour (AC5, §14);
- the review record and open decisions (AC6, ADR draft and §17).

Out of scope: the remote WSS plane's own policy (`[wss]`, `rt/rpc/wss.rs`) except where it shares the dispatcher; vendor channel migrations to plugins (#8850); desktop UI; a second local server or gateway-local copies of domain state (#11001 non-goals).

This contract does not by itself approve a new transport, auth model, authority boundary or rename (#11000 risk note). Where it proposes one, the ADR records it as proposed and names the decision.

Typed scope (DEC-28): the contract gives wire shapes and error lists for all 97 master methods, every method it proposes, the 49 methods the original parity PRs add and the two the gateway route ports add, the last two pinned to the PR heads in §15.1 (§8, Appendices C, including C.6, and E).

## 2. The three documents: framing spec, OpenRPC, OpenAPI 3.1

| Document | Describes | Source of truth | Served by |
|---|---|---|---|
| This contract (to land as `docs/book/src/architecture/core-gateway-ipc-contract.md` or merged into `rpc-socket.md`) | NDJSON framing, connection states, limits, handshake, credential routing, endpoint discovery, version policy, error model, subscription and turn-lifetime semantics, reconnect, process lifecycle, webhook crossing, platform behaviour | prose, reviewed | docs site |
| OpenRPC document | every method's params/result JSON Schema, notification payloads, error codes | generated from `zeroclaw-rpc-proto` types (#11165) and drift-checked against `Method::ALL` in CI (§15.1 has the head and check name) | checked-in JSON; `rpc.discover` is **not** served (§2.2) |
| OpenAPI 3.1 document | the gateway's HTTP/WS/SSE routes for browsers and REST clients | gateway-owned `gw/openapi.rs` (`"openapi": "3.1.0"` at `gw/openapi.rs:450`, `:674`) | `zeroclaw-gw` at `/api/openapi.json`, `/api/docs` |

### 2.1 Why OpenAPI 3.1 cannot describe the core transport

OpenAPI 3.1 describes HTTP operations: a method, a path, request/response bodies per status code. The core transport has none of these. It is a single byte stream per connection, framed by newlines; either peer may send a request (`elicitation/create` flows core→client, `rt/rpc/approval_channel.rs`, `api/jsonrpc.rs:348-389`); responses correlate by JSON-RPC `id`, not by request/response pairing on a socket; unsolicited notifications (`session/update`, `logs/event`, `events/event`, `subscription/lagged`; `rt/rpc/dispatch.rs:38-43`) interleave with responses; and the connection itself carries authenticated state established by `initialize`. OpenAPI 3.1 webhooks and callbacks model HTTP requests *to* a URL, not interleaved frames on one stream. Describing the core as OpenAPI would require either an HTTP server in the core (which #7432's acceptance forbids) or a fiction.

The RFC's "OpenAPI 3.1 contract" therefore maps as follows (DEC-02, proposed):

- **The gateway's public HTTP API is specified in OpenAPI 3.1.** Its route inventory is currently partial (`docs/book/src/gateway/api.md:8`); completing it is gateway work (G3) and each route row in §15.3 names its OpenAPI status.
- **The core's IPC API is specified in OpenRPC** (the JSON-RPC analogue of OpenAPI; it uses JSON Schema for params and results) **plus this framing spec.** Neither OpenRPC nor OpenAPI describes NDJSON framing, bidirectional requests, connection-scoped authentication or subscription resumption; §3, §5, §9 and §10 do.
- **Mapping between them** is the route→method table (§15.3): every gateway route either maps to named RPC methods, or is a gateway-local transport/static route with no domain effect.

### 2.2 No `rpc.discover`

OpenRPC defines an optional `rpc.discover` method. The core does not serve it: `initialize` already returns the method list (`capabilities`, `rt/rpc/dispatch.rs:3239-3242`), and the schema document is a build artifact, not runtime state. Adding `rpc.discover` later is an additive change under §7.

## 3. Transport and framing (normative companion to the OpenRPC document)

### 3.1 Streams

- **Unix (Linux, macOS, BSD):** a `SOCK_STREAM` Unix domain socket. `[master]` `rt/rpc/local.rs:648-1020`.
- **Windows:** a byte-mode named pipe. `[master]` `rt/rpc/local.rs:1022-1092`. The same framing runs over both; the read/write loop is shared through `tokio::io::split` (`rt/rpc/local.rs:176-180`).

### 3.2 Framing

A frame is one JSON value encoded as UTF-8, followed by `\n` (LF). `[master]`

| Rule | Behaviour at f0ae8c8bd8 | Cite |
|---|---|---|
| Terminator | LF. The reader trims surrounding whitespace, so a CR before the LF is tolerated. Blank lines are skipped. | `rt/rpc/dispatch.rs:2785-2788` |
| Max frame | 8 MiB (8,388,608 bytes) including the terminator budget. Oversize: one `-32600` error with `id: null`, `data: {"reason":"frame_too_large","limit_bytes":8388608}`, then end of stream. The rest of the line is never read. | `rt/rpc/local.rs:22`, `:112-118`, `:377-410` |
| Frame deadline | A started frame must complete within 30 s of its first byte, else `-32600`, `data: {"reason":"frame_timeout","limit_ms":30000}`, then EOF. Idle time *between* frames is unbounded. | `rt/rpc/local.rs:43`, `:120-129` |
| Encoding errors | Invalid JSON → `-32700` with `id: null`. | `rt/rpc/dispatch.rs:2835-2842` |
| Envelope | Every frame MUST be a JSON **object** with `"jsonrpc": "2.0"`. A JSON array (JSON-RPC batch) is not an object and is rejected as `-32600`. Batches are not supported. | `api/jsonrpc.rs:83-159` |
| Requests | `method` string; `params` object or array when present (absent → `null`); `id` string, number or null; no `result`/`error`. | `api/jsonrpc.rs:108-130` |
| Notifications | A request without `id`. The core never replies to one, including for unknown methods and authorization failures. | `rt/rpc/dispatch.rs:2904-2931`, `:3114-3116` |
| Responses | `id` plus exactly one of `result` or `error`. An explicit `"result": null` is a success. | `api/jsonrpc.rs:132-157` |
| Malformed response-shaped frame | Logged without contents and dropped; no reply (echoing its id could complete an unrelated request in the other direction). | `rt/rpc/dispatch.rs:2855-2871`, `rpc-socket.md` "Bidirectional requests" |
| Unknown response id | Logged and ignored. | `rt/rpc/dispatch.rs:2877-2900` |

### 3.3 Bidirectional requests and id spaces

Either peer may send requests on an established connection. Each peer allocates its own outbound ids (`zc-out-<n>`, `api/jsonrpc.rs:19`, `:353-354`); correlation is directional: a peer matches a response only against its own pending map, so the same textual id may be in flight in both directions. `[master]` A client MUST answer every core→client request with exactly one response carrying the same `id`. The core→client requests at f0ae8c8bd8 are `elicitation/create` (ask-user and poll flows); §8.15 lists them.

### 3.4 Ordering and concurrency

- **Per connection, requests are handled serially** in arrival order, except `session/prompt`, which is always spawned so the read loop stays live (`rt/rpc/dispatch.rs:2946-2967`). `[master]` Consequence for clients: a slow method (for example `doctor/run`, `cron/trigger`) blocks every later request *on the same connection*. The gateway MUST NOT put independent users' requests on one connection (§5.3) and SHOULD keep long operations off the connection that carries latency-sensitive control traffic (§11.4).
- **Frames are written in the order they are queued** on the connection's single writer (`rt/rpc/local.rs:244-286`). Responses and notifications therefore interleave. A client MUST NOT assume that a notification it expects "before" a response arrives first unless this contract states that ordering for the method (the only stated one: a turn's `session/update` events for one session are written in production order, and its `turn_complete` is the last event of that turn, `rt/rpc/dispatch.rs:11469-11560`, emitted at `:6449-6457`).
- **`session/prompt`'s response is not the turn result.** It is `{}` once the turn is admitted and finishes, or an error; the terminal outcome is the `session/update` event `turn_complete` (`rt/rpc/dispatch.rs:2946-2967`; `rt/rpc/types.rs:1648-1664`). `[master]`

### 3.5 Connection states and limits

```
accepted ──initialize ok──▶ bound ──EOF / write stall / daemon reload / shutdown──▶ closed
   │                          │
   └── 30 s without a completed initialize ─▶ closed (no reply)
```

| Limit | Value | Client sees | Cite |
|---|---|---|---|
| Initialize deadline | 30 s from accept; also bounds an `initialize` whose authentication is still running | connection closed, no frame | `rt/rpc/local.rs:37`, `rt/rpc/dispatch.rs:2768-2811` |
| Write stall | 30 s per frame while output is queued | connection closed; on master this cancels the turns that connection started (§9.2) | `rt/rpc/local.rs:32`, `:209-286` |
| Open connections | `rpc.max_local_connections` (default 512; clamped to ≥1), read at listener start | one `-32004` frame, `data: {"reason":"connection_limit","limit":N}`, then EOF; at most 16 refusal notices in flight | `rt/rpc/local.rs:54`, `:131-140`, `:523-558`; `cfg/schema.rs:8329-8352` |
| Chunked upload | 1 MiB chunks, 10 MB per file, 4 staged per connection, 256 MiB process-wide, 5 min idle; local transport only | see `rpc-socket.md` "Chunked uploads" | `rt/rpc/upload.rs` |
| Drain on reload/shutdown | connections get `CONNECTION_DRAIN_GRACE` = 5.5 s to unwind, then are aborted | EOF | `rt/rpc/mod.rs:27-28`, `rt/rpc/local.rs:628-641` |

The frame cap is per frame, not a memory bound for a connection; 512 connections each holding a near-8 MiB partial frame is roughly 4 GiB before queues (a point a review of the earlier design made). Aggregate read-buffer budgeting is a `[proposed]` hardening item for #11001, not a v0.9.0 gate unless the maintainers make it one.

## 4. Endpoint discovery

### 4.1 Current behaviour (three implementations that disagree)

| Resolver | Order | Cite |
|---|---|---|
| Core (`socket_path`) | `ZEROCLAW_SOCKET` (used verbatim, **not** trimmed; an empty value yields an empty path), else platform default from `config.data_dir` | `rt/rpc/local.rs:167-172` |
| Core data directory | `ZEROCLAW_CONFIG_DIR` → `<dir>/data`; else `ZEROCLAW_DATA_DIR`; else `ZEROCLAW_WORKSPACE` (deprecated); else, **on macOS only, when the running executable is named exactly `zeroclaw` under a Homebrew prefix**, `<prefix>/var/zeroclaw/workspace`; else `~/.zeroclaw/data`. `data_dir` is computed at load time and never read from `config.toml`. | `cfg/schema.rs:109-115`, `:21263-21270`, `:21372-21402`, `:21507-21522` |
| zerocode | `ZEROCLAW_SOCKET` (trimmed; empty ignored) > `<config_dir>/data/daemon.sock` (Windows: the hashed pipe name of that directory), where `config_dir` = `--config-dir` > `ZEROCLAW_CONFIG_DIR` > `$HOME/.zeroclaw` (`%USERPROFILE%` on Windows). There is no endpoint flag. It ignores `ZEROCLAW_DATA_DIR`, `ZEROCLAW_WORKSPACE` and the Homebrew directory. | `zc/client.rs:164-214`, `zc/main.rs:117`, `:1095` |
| Platform default | Unix `<data_dir>/daemon.sock`; Windows `\\.\pipe\zeroclaw-{:x}` of `std::collections::hash_map::DefaultHasher` over the `Path` | `rt/rpc/local.rs:866-868`, `:1035-1041`; zerocode duplicates the Windows hash at `zc/client.rs:180-190` |

Three defects follow, each testable today:

1. **Data-directory divergence.** A daemon started with `ZEROCLAW_DATA_DIR=/srv/zc` listens on `/srv/zc/daemon.sock`; zerocode started with the same environment dials `~/.zeroclaw/data/daemon.sock`. The same holds for the Homebrew service layout (`<prefix>/var/zeroclaw/workspace/daemon.sock` versus `~/.zeroclaw/data/daemon.sock`).
2. **Executable-name dependence.** The Homebrew branch matches `exe_name == "zeroclaw"`. A `zeroclaw-gw` or `zerocode` executable in the same `bin/` computes a different default even when it links the same resolver.
3. **Unstable Windows pipe name.** `DefaultHasher`'s algorithm is explicitly unspecified across Rust releases. Two binaries built with different toolchains (a separately built desktop app, a distro-built gateway) can derive different pipe names for the same data directory. Today this is masked because zerocode and the daemon ship from one workspace build and zerocode refuses any daemon of a different package version (`zc/client.rs:637-638`).

### 4.2 Normative resolution order `[proposed]` (#11001 / #11186)

Every client (zeroclaw-gw, zerocode, the desktop app, `zeroclaw` CLI subcommands that dial the daemon) MUST resolve the endpoint with **one shared implementation** in `zeroclaw-rpc-client`, in this order, stopping at the first that yields a value:

1. An explicit endpoint given to the process by its launcher: a command-line flag (`zeroclaw-gw --core-endpoint <path|pipe>` `[proposed]`; zerocode has none today) or `ZEROCLAW_SOCKET` in its environment. Values are trimmed; an empty value counts as absent. The core's `socket_path` MUST apply the same trimming rule (today it does not).
2. The platform default for the data directory resolved by the **core's** precedence (`ZEROCLAW_CONFIG_DIR` > `ZEROCLAW_DATA_DIR` > `ZEROCLAW_WORKSPACE` > Homebrew > home), where the Homebrew branch recognizes the prefix from any of the shipped executables (`zeroclaw`, `zeroclaw-gw`, `zerocode`) in `bin/`, not only `zeroclaw`.

Launchers MUST pass the endpoint explicitly: in child mode the core spawns the gateway with `ZEROCLAW_SOCKET` set to the endpoint it bound (§11.3); systemd, launchd, Nix and the desktop app set it (or `ZEROCLAW_CONFIG_DIR`) in the unit/sidecar environment. Derivation (step 2) is the fallback for interactive use, not the supported path for services.

**Windows pipe name `[proposed]`.** Replace `DefaultHasher` with a specified derivation: `\\.\pipe\zeroclaw-` followed by the first 16 lowercase hex digits of SHA-256 over the UTF-8 bytes of the data directory as the core stores it (no canonicalization beyond what the core's resolver already does). This is a lockstep change for core, zerocode and rpc-client in one release; mixed 0.8.x/0.9.x pairs are already refused by zerocode's exact version gate, so no supported pair breaks. Test `CT-DISC-03` pins a known path to a known name.

**Config key.** No config key is required for discovery. `data_dir` is not a `config.toml` field (`cfg/schema.rs:109-115`), so a client never has to read `config.toml` to find the endpoint. An optional `[rpc] endpoint` override is **additive** (no schema-version change) and, if added, is read by the core only; clients learn it from their launcher, because a hardened gateway does not read `config.toml` (§13). This matches the schema-impact proposal.

### 4.3 Discovery conformance tests

| ID | Asserts | Status |
|---|---|---|
| CT-DISC-01 | For each env combination (`ZEROCLAW_SOCKET` set/empty/unset × `ZEROCLAW_CONFIG_DIR` × `ZEROCLAW_DATA_DIR` × `ZEROCLAW_WORKSPACE`), the core's bound endpoint equals the rpc-client resolution | `[proposed]` (#11001) |
| CT-DISC-02 | Homebrew layout: a fake prefix with `bin/zeroclaw`, `bin/zeroclaw-gw`, `bin/zerocode` resolves one endpoint | `[proposed]` |
| CT-DISC-03 | Windows: pinned data dir → pinned pipe name (golden) | `[proposed]` |
| CT-DISC-04 | Child mode: the spawned gateway receives the bound endpoint in `ZEROCLAW_SOCKET` | `[proposed]` (G3) |

## 5. Identity, authorization ownership and credential handoff

### 5.1 What the core authenticates today `[master]`

Credential routing is explicit and final (`rt/rpc/auth.rs:6-17`, `:668-790`):

| `initialize` presents | Transport | Result |
|---|---|---|
| `auth_token` (+ optional `auth_provider`, default `native`) | any | the named provider verifies it; its denial is final, never a fallback (`auth.rs:690-705`) |
| no token; Unix peer uid (`SO_PEERCRED`/`getpeereid`, `rt/rpc/local.rs:1014-1019`) | Local, Unix | peercred provider: the daemon's own uid with `security.trust_daemon_uid = true` (**default true**, `cfg/schema.rs:19386-19394`) → shared operator; a `[users.*].uid` roster match → that principal; otherwise denied (`auth.rs:248-289`, `:706-715`) |
| nothing (Windows pipes always; Unix only when the kernel reports no peer credential) | Local, no `[users]` roster | shared operator ("local compatibility", `auth.rs:716-728`) |
| nothing | Local with a roster | `-32010` (`auth.rs:729-733`) |
| nothing | WSS | `-32010` (`auth.rs:734-738`) |

After `initialize`, **every** privileged call passes the per-operation gate (`rt/rpc/dispatch.rs:1087-1182`): credential expiry, the revalidation deadline, native-token liveness (the retained SHA-256 must still be paired), re-resolution when the authorization generation moved, then the method's `(Resource, Verb)` grant from `Method::authz()` (`dispatch.rs:363-475`), which is arm-complete over the closed `Method` enum so an unclassified method does not compile. Handlers that wait for admission recheck authority after the wait (`dispatch.rs:1355-1400`); effects that must not race a revocation hold an `AuthorityLease` (`auth.rs:559-618`).

Consequence the contract depends on: **on Unix a same-uid process that connects without a token is the shared operator** under the default `trust_daemon_uid = true`. A gateway running as the daemon's user is operator-equivalent by operating-system construction whatever this contract says about its service profile. §5.6 states what that means for the trust model.

### 5.2 Who the gateway serves

| Caller at the gateway | Credential the gateway receives | What reaches the core |
|---|---|---|
| Dashboard/API user, native pairing | `Authorization: Bearer <paired token>` | a credential-bound connection initialized with that token, `auth_provider: "native"` (§5.3) |
| Dashboard user, OIDC browser login (#11082) | gateway session cookie → the ZeroClaw access token the gateway holds for that session | a credential-bound connection initialized with the access token, `auth_provider: "oidc.<alias>"` |
| API/service client, OIDC `client_credentials` | `Authorization: Bearer <access token>` | same as above |
| Anyone, when pairing is disabled (`gateway.require_pairing = false`) | nothing | **OPEN D2** (§5.4) |
| Vendor/plugin webhook, generic `/webhook`, `/sop/*` | route-specific evidence (signature headers, `X-Webhook-Secret`, bearer) | the raw request as *evidence* on an ingress method; the core verifies (§12) |
| The gateway itself (route snapshot, status, delivery transport) | its own identity | **OPEN D1** (§5.4) |

### 5.3 Credential-bound connections (normative)

- **ID-1 One credential per connection.** A connection initialized with credential *C* carries only requests from holders of *C*. The gateway MUST NOT send a request that an HTTP caller authenticated with *C* on any other connection, and MUST NOT use its own service connection for it, except the ingress methods of §12, which carry their own evidence.
- **ID-2 No on-behalf-of.** No method's params contain a field that selects the principal the core acts as. CT-ID-02 scans the generated OpenRPC document for `principal`, `principal_id`, `on_behalf_of`, `act_as`, `as_user`, `impersonate` in any params schema and fails on a hit. (`session/new`'s legacy `tui_id` param is accepted and ignored, `rt/rpc/types.rs:213-218`.)
- **ID-3 Re-present on every connect.** A reconnect re-runs `initialize` with the same credential. `tui_id`/`tui_sig` continuity grants nothing (`dispatch.rs:3146-3150`, `:3166-3187`).
- **ID-4 Pool key and failure.** The pool key is `(auth_provider selection, SHA-256(credential bytes))`, never the principal id: two tokens that resolve to one principal differ in expiry and revocation. A `-32010`/`-32012` on a pooled connection is returned to that HTTP caller (mapped per §8.16) and the connection is discarded; the pool never retries the request on another credential or on the service connection.
- **ID-5 No tokenless fallback (invariant; decided, proposed for acceptance).** No absent, blank, malformed, invalid or unsupported-provider HTTP credential may ever become a credential-less core connection. This covers health probes, readiness checks, reconnect loops and every error path. A credential-less connection would not fail: on Unix the core admits a same-uid peer with no token as the shared operator through the trusted daemon uid (`rt/rpc/auth.rs:259-262`, `:706-713`, default `trust_daemon_uid = true`), and on any local transport with no `[users]` roster it admits a peer with no transport credential through local compatibility (`auth.rs:716-728`); on Windows that is every pipe client, since pipes carry no peer uid (`rt/rpc/local.rs:1082-1087`). #11186's in-process transport refuses tokenless `initialize` (`TransportKind::Inproc` arm, `rt/rpc/auth.rs:739-746` @337d0a186f), but that refusal does not carry to the real socket or pipe. Required behaviour:
  - the gateway answers an HTTP request whose credential is absent, blank or for an unknown provider with 401 **without opening or reusing any core connection**, with two exceptions: the D2 anonymous mode when the core has reported pairing disabled, and the **public routes** (DEC-25), which never take a caller credential and are served only through the gateway's service connection by the one public method each route needs: `/health` → `health`, `/metrics` → `metrics/scrape`, the A2A card routes → `a2a/identity`, `OPTIONS /api/config` → `config/schema` without `path`, `/pair`/`/api/pair` → `pairing/redeem`, `/pair/code` → the pairing posture of `gateway/register` (§8.13). A public route never borrows another method, and its core handler enforces the route's own publication rule (for A2A: `[a2a.server] enabled` and the agent published, §8.13);
  - the gateway's own probes and control traffic use its service credential (D1-A or D1-B), never an empty `initialize`. D1-C is the one option that conflicts with this rule: its control connection is credential-less and resolves to the shared operator, so under D1-C the invariant holds only for HTTP-caller traffic (ID-1 already forbids serving a caller on the service connection) and the gateway alone limits the control connection to the §8.13 methods, because the core cannot tell it from the operator's own zerocode. This is one reason D1-C is not recommended;
  - on a `-32010` the pooled connection is discarded and the HTTP caller gets 401; the pool never retries tokenless, with the service credential, or with another user's credential;
  - core-side defense in depth `[proposed]`: on Windows the core reads the connecting process's user SID (`GetNamedPipeClientProcessId` + token query) and applies local compatibility only to the daemon's own SID; on both platforms a hardened deployment configures a `[users]` roster so the compatibility path is closed.
  Tests CT-ID-05 (every gateway `initialize` carries `auth_token`, outside D2's mode), CT-AUTH-05 (credential-free A2A card discovery with pairing on, through a service principal restricted to the allowlist) and CT-ID-09 (for each of: no header, a `Bearer` prefix with an empty token, garbage token, revoked token, `X-ZeroClaw-Auth-Provider` naming an unknown provider, health probe, reconnect after `-32010`: assert zero credential-less `initialize` frames reach the core, every request with a bad credential gets 401, and the probe and reconnect paths use only the service credential).
- **ID-6 Bounds.** Per-credential connections are capped (`[proposed]` default 64 credentials, 2 connections each: one stream connection for subscriptions/turn events, one request connection), evicted after 10 minutes idle, and always below `rpc.max_local_connections` with headroom for zerocode and recovery clients (§11.4).

### 5.4 The two identity decisions

**OPEN D1: the gateway's own identity.** The gateway needs a connection that belongs to no end user: to deliver webhooks when nobody is logged in, to read its effective settings, to report presence.

| Option | Mechanism | Unix | Windows | What it proves |
|---|---|---|---|---|
| D1-A (recommended) | new `service` auth provider; core-minted key file: `<data_dir>/gateway.key` (0600) in the same-account layout, `<ipc_dir>/gateway/gateway.key` (0640, the gateway group) in the separate-account layout of §5.5.1; a resolver branch pinned to that provider's provenance maps `Service{issuer: local, client_id: gateway}` to a built-in, non-editable `gateway-ingress` permission profile | yes | yes (key ACL instead of mode) | the holder of the key; narrow grants |
| D1-B | dedicated OS account mapped in `[users.gateway] uid = N` with an operator-authored narrow profile; peercred authenticates it | yes | **no**: pipes carry no peer uid today (`rt/rpc/local.rs:1082-1087`) | the OS account |
| D1-C | none: the gateway connects credential-less and is the shared operator | yes | yes (no roster) | nothing beyond the socket mode / pipe ACL |

The `gateway-ingress` profile under D1-A is the method allowlist of §8.13: `gateway/register`, `gateway/status`, `health`, `metrics/scrape`, `a2a/identity`, `config/schema` (without `path`), `webhook/deliver`, `ingress/webhook`, `ingress/sop`, `ingress/cancel`, `webhook/routes`, `pairing/redeem`, plus the `oidc/*` relays of §8.14.1 if D10-b is chosen. Nothing else. It **excludes pairing code delivery** (a service that can read or subscribe to pairing codes can exchange one for a native bearer and reconnect as the shared operator). Consequently, in hardened mode the dashboard cannot show the first-run pairing code; the operator reads it from the core: the daemon log, zerocode, or `zeroclaw gateway get-paircode`, which moves from the gateway's admin routes to the core socket (`pairing/code`, §8.8). In the default same-uid mode the gateway is operator-equivalent anyway (§5.1), so the profile limits what gateway *code* does, not what a compromised same-uid gateway can do. The ADR records this honestly rather than calling the default mode narrow.

**OPEN D2: pairing disabled.** When `gateway.require_pairing = false`, HTTP callers present nothing, and today the in-process gateway serves them with full operator authority.

| Option | Behaviour | Cost |
|---|---|---|
| D2-A (recommended once D1-A exists) | "anonymous compatibility" connection: the gateway initializes with its service credential plus `anonymous_http: true`; **the core** resolves it to the shared operator only when its own pairing authority reports pairing disabled (built from `gateway.require_pairing`, `rt/daemon/mod.rs:617-621`) and refuses with `-32010`, `data.reason: "pairing_required"` otherwise. Audited as `auth_method: gateway_anonymous`. Never a fallback after a failed credential. Connection classes stay apart: anonymous connections form their own pool (the pool key gains the binding mode: `service`, `anonymous` or a caller credential digest); a connection is never re-initialized into another mode, and the core refuses a re-`initialize` that changes the binding mode while requests are in flight (`-32600`, `data.reason: "binding_mode_change"`). Anonymous bindings end with their generation: `require_pairing` is fixed per `PairingGuard` generation (Appendix B) and a reload retires every connection (§10.2), so after pairing becomes effective the next anonymous `initialize` is refused and the gateway answers callers 401, never falling back to the service binding | one resolver branch; depends on D1-A |
| D2-B | the gateway opens credential-less connections and relies on peercred/local compatibility | no core change, but the core cannot tell an anonymous HTTP caller from the operator's own zerocode, and the gateway (not the core) decides when to do it |
| D2-C (recommended for v0.9.0 and until D1-A ships) | the separate gateway requires pairing; `require_pairing = false` stays supported only for the in-process gateway until it is removed | pairing-disabled installs, including the Docker default (`Dockerfile` bakes `require_pairing = false`), keep the in-process gateway and cannot switch to `zeroclaw-gw` yet |

Sessions created through a D2-A connection are owned by the shared operator (`owner_principal_id: None`, `rt/rpc/session.rs:108-111`), so enabling pairing later does not hide them.

**D2-A specification (DEC-30, applies only if D2-A is chosen).** The anonymous authority stays core-controlled end to end; the gateway's cached posture never grants anything.

- *Eligibility is genuine absence.* The gateway uses an anonymous connection only for an HTTP request that presents no credential at all: no `Authorization` header, no bearer WebSocket subprotocol, no `?token=`, no `X-ZeroClaw-Auth-Provider`. A blank, malformed, invalid, revoked or unsupported-provider credential is answered 401 in both pairing modes, with `data.reason: "credential_rejected"` so the dashboard can drop its stored token and retry without one. This is stricter than today, where `require_auth` returns before reading the header when pairing is off (`gw/api.rs:47-49`); it is listed under D13.
- *Handshake.* `initialize` gains `anonymous_http?: boolean` `[proposed]`, accepted only together with the gateway's service credential (`auth_provider: "service"`); with any other credential, or none, it is refused with `-32602`. The result names the binding the core chose: `binding: "service" | "anonymous"` `[proposed]`, and for `anonymous` the `pairing_generation` it was granted under.
- *Mode and revision.* `PairingPosture` (§8.13) carries `pairing_generation`, a counter the core increments whenever it builds a new pairing authority, which is once per daemon generation (`rt/daemon/mod.rs:617-621`). The gateway may *attempt* an anonymous connection only while the posture it last received says `require_pairing: false`; the core decides.
- *Activation boundary.* A change to `gateway.require_pairing` takes effect only when the next daemon generation builds its pairing authority. `PairingGuard.require_pairing` is fixed for a generation (`cfg/pairing.rs:359`), and the RPC authentication policy is compiled from other inputs (`rt/rpc/auth.rs:161-171`: OIDC, roster, permission profiles, daemon-uid trust; "the pairing authority is shared live state, not config"). Until then `require_pairing_on_reload` announces the pending change (§8.13).
- *Invalidation.* Every reload retires every connection (§10.2), so no anonymous binding survives into a generation where pairing is required. As defense in depth, independent of that retirement, the per-operation gate re-checks an anonymous binding on **every** request: if the live pairing authority requires pairing or its generation differs from the binding's `pairing_generation`, the request is refused with `-32010`, `data.reason: "pairing_required"`, and the binding is dropped. The gateway then answers the waiting HTTP caller 401 and never retries on the service binding.
- *Tests.* CT-D2-01 pairing off→on with an anonymous connection already bound: after the reload, the old connection is gone, a new anonymous `initialize` is refused with `pairing_required`, and a unit test with a forced generation mismatch shows the per-operation refusal. CT-D2-02 each of blank, garbage, revoked and unknown-provider credentials answers 401 with pairing off and with pairing on, with zero anonymous `initialize` frames. CT-D2-03 genuine absence gets the anonymous binding with pairing off and 401 with pairing on.

### 5.5 Endpoint verification before any credential `[proposed]` (#11001 / #11186)

A client MUST verify the operating-system identity of the endpoint it is connected to **before it writes any frame containing a reusable credential**, on **every** connection (not once per process).

- **Unix.** Read the connected socket's peer credentials (Linux `SO_PEERCRED`, macOS/BSD `getpeereid`/`LOCAL_PEERCRED`; tokio exposes both through `UnixStream::peer_cred()` on a client stream). The peer uid MUST equal the expected core uid: by default the client's own effective uid; in hardened mode the uid the launcher provides. In child mode the launcher also passes the core's pid and the client SHOULD check it.
- **Windows.** Call `GetNamedPipeServerProcessId` on the connected handle, open that process's token and compare its user SID with the expected SID (default: the client's own; hardened: provided by the launcher). In child mode compare the pid with the parent pid the core passed.
- On mismatch the client closes the connection without writing, and reports both identities (`core endpoint <path> is served by uid 1002, expected 501`).

**Where this is enforced.** In `zeroclaw-rpc-client`'s dial path, between opening the stream and sending `initialize`, for every dial including reconnects. Today the client opens the stream and immediately runs the handshake with the credential (`RpcClient::connect_local` → `open_local_stream` → `connect_over`, `crates/zeroclaw-rpc-client/src/client.rs:338-341`, `:420-436`, `:602-630` @337d0a186f); a peer's own report of its pid or version inside `initialize` is not authentication because a fake listener can say anything. Status: **decided (proposed)**; enforced by draft #11274 (stacked on #11186: the client checks the peer uid and an owned, non-writable socket directory on every Unix dial before any credential leaves it) and #11001 for the Windows half (CT-ID-06/07).

**Supported operating-system trust boundaries.**

| Deployment | Expected core identity the client checks | What the check proves | What it cannot prove |
|---|---|---|---|
| Same OS account (default; child mode, desktop, single-unit services) | the client's own uid (Unix) or user SID (Windows); in child mode also the parent pid | the endpoint is served by a process of this account, not another local user's squatter or a stale `ZEROCLAW_SOCKET` pointing at someone else's listener | anything about other processes of the same account: same-account malware can read `config.toml`, key files and tokens anyway |
| Separate accounts (hardened: Nix, compose, multi-user hosts) | the configured core account's uid/SID, passed by the launcher | the endpoint belongs to the core's account | same-account compromise of the core account |

The contract specifies exactly these two boundaries. The separate-account boundary needs the file-system layout of §5.5.1; which boundaries v0.9.0 supports is **OPEN D11** (recommended: same account only for v0.9.0, separate accounts once §5.5.1 and D1-A ship). A gateway on another host is out of scope (the remote WSS plane is a different transport with mTLS).

This defends against a different local user (or an untrusted `ZEROCLAW_SOCKET` path) presenting a listener to collect bearers, which is the threat the earlier `server_proof = HMAC(key, nonce)` design targeted and, as a review of that design showed, did not achieve (a fake listener that receives the service key in the credential-first handshake can compute the proof). It does not defend against malware running as the core's own user, which can read the key file and `config.toml` anyway; that limit is stated in the ADR.

Required core-side hardening that makes the Windows check meaningful `[proposed]`:
- Create **every** pipe instance, the first one and each replacement created in `accept` (`rt/rpc/local.rs:1064-1076`), with an explicit DACL: the daemon user's SID and `SYSTEM` full control; in hardened mode also the gateway account's SID read/write; no `Everyone`/anonymous entry. The comment at `rt/rpc/local.rs:1058-1062` understates the default: per the `CreateNamedPipe` documentation, a pipe created with a null security descriptor grants full control to LocalSystem, Administrators and the creator owner, and **read access to Everyone and the anonymous account**. Read access cannot send requests, but another local user can still open instances and hold connection slots until the 30 s initialize deadline.
- Keep `first_pipe_instance(true)` on the first instance (`rt/rpc/local.rs:1052`) and tokio's default `reject_remote_clients(true)`.

| Test | Asserts | Status |
|---|---|---|
| CT-ID-06 | Unix: a listener bound by another uid at the resolved path receives zero bytes from the client; the error names both uids (Linux CI with a throwaway user) | `[proposed]` |
| CT-ID-07 | Windows: a pipe served by a process whose SID differs, or whose pid differs from the child-mode parent pid, receives zero bytes | `[proposed]` |
| CT-ID-08 | Windows: every instance, including the replacement created after the first accept, carries the explicit DACL (read back with `GetSecurityInfo`) | `[proposed]` |

### 5.5.1 Separate-account layout `[proposed]` (DEC-27; not in the v0.9.0 preview unless D11 says so)

Today the layout cannot work: the listener tightens the socket's parent directory to `0700` (`rt/rpc/local.rs:880-887`) and the socket to `0600` (`:992-997`) on every bind, so another account can neither traverse to the socket nor connect, and anything placed under the `0700` data directory is unreachable whatever its own mode. Group-readable files under a private parent do not help. The layout below keeps configuration and core data private and grants the gateway account exactly three things: connecting to the endpoint, reading its bootstrap, and (D1-A) reading its key.

**Unix.** The operator names the gateway's group `G` and an IPC directory outside the data directory (systemd `RuntimeDirectory=zeroclaw`, i.e. `/run/zeroclaw`, or any path the operator chooses); both reach the core as additive settings (§13.2).

| Path | Owner : group | Mode | Why |
|---|---|---|---|
| `<ipc_dir>/` | core : `G` | `0750` | `G` may traverse and list; only the core may create, rename or unlink entries, so the gateway cannot replace the endpoint or its lock. `require_trusted_lock_dir` already accepts a directory the daemon user owns that is not group- or world-writable (`rt/rpc/local.rs:702-735`) |
| `<ipc_dir>/daemon.sock` | core : `G` | `0660` | Linux requires write permission on the socket file to connect; BSD-derived systems (macOS) ignore socket file permissions, so the directory mode is the portable control |
| `<ipc_dir>/daemon.sock.lock` | core | `0600` | unchanged |
| `<ipc_dir>/gateway/` | core : `G` | `0750` | the gateway's read-only material |
| `<ipc_dir>/gateway/bootstrap.json` | core : `G` | `0640` | §13.3 |
| `<ipc_dir>/gateway/gateway.key` | core : `G` | `0640` | D1-A only |
| `<data_dir>/`, `config.toml`, secrets | core | `0700` / `0600` | unchanged; the gateway account cannot traverse them |

Required listener change: when a gateway group is configured, the core creates or validates `<ipc_dir>` with that owner, group and mode and **fails closed** if it cannot, instead of the best-effort `0700` chmod, and secures the socket as `0660` group `G` instead of `0600`. The same code runs on every bind, including the re-bind after a supervised listener restart and every generation that binds, so a reload cannot chmod the arrangement away. With no gateway group configured, today's `0700`/`0600` behaviour is unchanged.

Authentication across the two accounts: a tokenless connection from the gateway account is **denied** on Unix, because peercred verifies only the daemon uid (when trusted) or a roster uid and never falls back to the shared operator (`rt/security/auth_provider/peercred.rs:115-137`); local compatibility applies only to transports without a peer identity (`rt/rpc/auth.rs:716-728`). The gateway therefore authenticates with its D1-A key, or as a D1-B roster uid. The gateway verifies the endpoint (§5.5) against the core's uid and checks that `<ipc_dir>` is owned by that uid and not group- or world-writable (#11274 performs this directory check).

**Windows.** Pipes carry no peer uid, and with no `[users]` roster any pipe client without a credential is the shared operator (§5.3 ID-5), so a separate-account Windows deployment MUST configure a roster (or the proposed core-side SID check) before the gateway account is granted pipe access. Every pipe instance carries the explicit DACL of §5.5 with the gateway account's SID added (read/write). The bootstrap and key live in a directory whose ACL grants the gateway SID read and nothing else; Windows grants traverse by default (the "bypass traverse checking" right), so the files' own ACLs are the control.

| Test | Asserts | Status |
|---|---|---|
| CT-ID-10 | Two accounts on Linux CI: the gateway account connects and authenticates with its key; reads `bootstrap.json` and `gateway.key`; gets `EACCES` on `config.toml` and on any path under `<data_dir>`; cannot unlink, rename or replace `daemon.sock` or its lock; a tokenless `initialize` from it is refused with `-32010`; after a core reload it still connects (modes preserved) | `[proposed]` |
| CT-ID-11 | Windows: the gateway account opens the pipe only with the DACL entry present; without a roster configured the core refuses to start in separate-account mode | `[proposed]` |

### 5.6 Trust model by deployment mode

| Mode | Gateway runs as | What a compromised gateway can do | What the contract guarantees |
|---|---|---|---|
| Child / same user (default) | the daemon's user | everything the operator can: connect credential-less (peercred → shared operator), read `config.toml` and key files | a *correct* gateway never escalates: ID-1..ID-6, no authority decisions in the gateway (§5.7), all domain effects authorized in the core |
| Hardened (separate account; recommended for Nix, compose, multi-user hosts) | its own account; cannot read the core's config, data directory or secrets; reads only its own bootstrap and, under D1-A, its key, both in the group-readable IPC directory of §5.5.1 | only what the service profile allows, plus whatever end-user credentials pass through it while compromised | the service profile is the gateway's whole standing authority; pairing codes are never delivered to it; every user operation needs that user's credential |

### 5.7 Authorization ownership (normative)

**AUTH-1.** The core decides every allow/deny on domain state: sessions, turns, approvals, memory, cron, config, pairing authority, SOP, skills, personality, plugins, channels, files, cost, logs/events, system operations. The decision uses the principal bound to the connection, `Method::authz()`, the method's selectors (agent entitlement, config write paths, tool ceilings, memory plane, session ownership) and, for ingress, the verified evidence.

**AUTH-2.** The gateway MAY apply *transport* policy that only narrows: per-client rate limits (`gateway.rate_limit_max_keys`, `pair_rate_limit_per_minute`, `webhook_rate_limit_per_minute`), body and header size caps, request timeouts, CORS/CSRF, TLS, forwarded-header trust (`gateway.trust_forwarded_headers`), bind policy (`allow_public_bind`), loopback-only routes (`allow_remote_admin`), WebSocket keepalive. None of these can make a request succeed that the core would refuse.

**AUTH-3.** After G3 the gateway crate contains no `RpcInboundAuth`, `PairingGuard`, grant evaluation, principal resolver, secret comparison or vendor signature verification. Two tests enforce it (CT-AUTH-03), and they check different things: (a) a **transitive** dependency ratchet over the feature-resolved normal dependency graph of the built `zeroclaw-gw` binary (`cargo metadata` resolve, all targets), which must contain none of `zeroclaw-runtime`, `zeroclaw-config`, `zeroclaw-channels`, `zeroclaw-tools`, `zeroclaw-providers`, `zeroclaw-memory`, `zeroclaw-plugins`, `zeroclaw-hardware`. A direct-dependency check is not enough: at #11165's head `zeroclaw-rpc-proto` depends on `zeroclaw-config` unconditionally (`crates/zeroclaw-rpc-proto/Cargo.toml:19` @f74d25a0f4), whose pairing and secret modules are always compiled. Passing (a) therefore requires moving every `zeroclaw-config` wire type the proto crate uses into the proto crate or a wire-only crate that `zeroclaw-config` then uses, so the proto crate drops that edge. The complete inventory at f74d25a0f4 (`crates/zeroclaw-rpc-proto/src/types.rs:19-21`, `:701`, `:707`, `:767`, `:773-774`, `:1121`): `CostSummary` with `ModelStats` and `AgentCostStats`; `BuilderSubmission` with `SelectorChoice<T>`, `QuickstartPeerGroup`, `AgentIdentity` and `QuickstartPersonalityFile`; `ConfigFieldEntry` with `PropKind` and `ConfigTab`; `AliasSource`; `MapKeyKind`; `SectionShape`; and the `From<MapKeySection>` conversion, which moves to the config side; `zeroclaw-api`, which holds wire and trait types but no authority evaluation, stays allowed. (b) A symbol grep over the gateway crate's own sources for the authority symbols above. Until G3, the in-process gateway keeps its current checks as defense in depth; each route drops its local check only when the route is served by an RPC whose core authorization covers the same rule (the route table in §15.3 names both).

**AUTH-4.** The current gateway's authority decisions and where each goes are inventoried in §5.8.

### 5.8 Authority the gateway exercises today, and where each piece goes

Everything below runs inside the gateway process at f0ae8c8bd8. Each row names the post-split owner and the method that replaces it (§8 tags its status).

| Decision today | Where | Credential it accepts | After the split |
|---|---|---|---|
| Config and onboarding routes: full `RpcInboundAuth::authenticate` (native and OIDC), per-write re-authorization under the config lock | `principal_gate::config_route_auth` on the config/admin router (`gw/principal_gate.rs:495-570`; `gw/lib.rs:829-923`); verification presents every HTTP request as `TransportKind::Wss` (`gw/principal_gate.rs:123-142`) | native bearer, or `X-ZeroClaw-Auth-Provider: oidc.<alias>` + bearer | the caller's credential-bound connection; the core authorizes `config/*` (§8.7). The gateway stops compiling and publishing policy (`publish_accepted` from `gw/principal_gate.rs:107-118`) |
| Most `/api/*`: `require_auth`, **open when pairing is off**, native bearer only | `gw/api.rs:43-72`, the check at `:47-49`; 91 of the 174 route rows (§15.3), 93 production call sites in 15 files | native bearer | credential-bound connection per route (§8); D2 for the pairing-off case |
| WS chat, SSE, canvas, plugin list, ACP, SOP-run stream (6 route rows): direct `is_authenticated`/`authenticate_and_hash`, also skipped when pairing is off | `gw/ws.rs`, `gw/sse.rs`, `gw/canvas.rs`, `gw/api_plugins.rs`, `gw/acp.rs`, `gw/ws_sop_runs.rs` | native bearer | credential-bound connection; ACP stays **OPEN** in scope (§8.14) |
| Generic `/webhook`, `/sop/*` truth table | `gw/lib.rs:3172-3268`, `:3355-3374` | pairing bearer and/or `X-Webhook-Secret` | `ingress/webhook`, `ingress/sop` in the core (§12.5, §12.7) |
| Pairing: code minting and redemption, token persistence to `gateway.paired_tokens`, device registry, admin token file | `gw/lib.rs:2682-2820` (`/pair`), `:2822-2849` (`persist_pairing_tokens`), `:4866-5066` (`/admin/paircode*`), `gw/api_pairing.rs:333-720` | pairing code, admin token, bearer | core pairing authority (§8.8); the gateway keeps only the pairing UX and per-client limits. Today RPC has **no** pairing method (`rt/rpc/dispatch.rs:86-217`) |
| Admin: loopback-only routes, admin-token routes, `/admin/reload` gate with `allow_remote_admin` | `gw/lib.rs:4684-4782` | loopback peer, admin token, bearer | loopback/remote-admin stays an edge rule (AUTH-2); the reload itself is `config/reload` authorized in the core |
| `/ws/nodes`: `nodes.auth_token` (an encrypted secret) or pairing fallback, live config read | `gw/nodes.rs:213-279` | node token or bearer | core-verified node admission `[proposed]`, or an explicit deferral (§8.14) |
| WebAuthn ceremonies: challenge state in gateway memory, unbounded, consumed before verification; success grants nothing | `gw/api_webauthn.rs:20-27`, `:124-142`, `:230-234` | bearer | deferral recommended (§8.14) |
| Vendor webhook verification (WhatsApp, Linq, Nextcloud, Gmail) against gateway-built instances holding their secrets | `gw/webhook_ingress.rs:403-454`; `gw/lib.rs:1430-1610`, `:4114-4670` | vendor signatures and tokens | **OPEN D7** (§12.9) |
| A2A `/a2a/{alias}`: `require_auth`, turn run in the gateway | `gw/a2a.rs:457-474`, `:602-603` | native bearer (open when pairing is off) | credential-bound `session/run-once` or an A2A method in the core (§8.14) |
| Self-upgrade: `require_auth` + `gateway.allow_self_upgrade` | `gw/version.rs:402-418` | bearer | `system/upgrade` in the core (#11182) |

State and process globals that cross in-process today and must be replaced, not copied, by the split (each silently breaks if left):

| Crossing | Where | Replacement |
|---|---|---|
| a by-value `Config` with every `#[secret]` already decrypted (`cfg/schema.rs:22685-22694`), plus `Arc<RwLock<Config>>` in `DaemonInboundAuthority` | `rt/daemon/mod.rs:642-701`; `rt/daemon/registry.rs:16-61` | edge settings by bootstrap projection and `gateway/register` (§13.3); nothing else |
| `DaemonInboundAuthority { PairingGuard, Arc<RpcInboundAuth>, Arc<RwLock<Config>> }` | `rt/daemon/registry.rs:22-41` | none: authority is not shared |
| `EventBus` (also carries broadcast-only pairing codes and QR payloads to SSE subscribers, `gw/sse.rs:21-30`) | `rt/daemon/mod.rs:572-574`, `:691` | `events/subscribe` on credential-bound connections (unscoped principals only, §10.1); pairing codes never reach a gateway connection |
| `CanvasStore`, SOP engine, SOP audit logger, SOP driver handles, `PluginWebhookRegistry` | `root/src/main.rs:7206-7248` | `canvas/*` (#11182), `sops/*` (#11169), `webhook/deliver` (§12.3) |
| process globals: config write lock, `CostTracker`, pricing `bind_config`, log broadcast hook, health registry, WhatsApp `PENDING_APPROVALS`, live channel registry used for approval routing | `cfg/write_lock.rs:36-39`; `gw/lib.rs:1408`, `:1418`, `:1872-1874`; `ch/whatsapp.rs:17-19`; `rt/agent/loop_.rs:113` | each is core-only after the split; gateway-run turns disappear (G3), so their approval-routing dependency disappears with them |
| built by the gateway from config: model provider, memory, runtime adapter, per-agent tool registries with MCP connections, every channel instance (`register_channels_for_tools`), session backend, tunnel, node registry and mDNS, device registry, WebAuthn manager with `SecretStore`, per-connection `AcpSessionStore` | `gw/lib.rs:1064-1405`, `:1430-1652`, `:1693-1725`, `:1890-2051`; `gw/acp.rs:72` | none of these may exist in `zeroclaw-gw`; tunnel moves to the core with an operator-configured upstream; nodes/WebAuthn per §8.14 |

### 5.8.1 "Local transport" stops meaning "a local human" `[proposed]` (DEC-18)

Several checks treat `TransportKind::Local` as evidence that the caller sits at the machine: forwarded environment is retained only for local admin connections (`rt/rpc/dispatch.rs:1915-1921`); chunked upload and path-based `file/attach` are local-only; #11182 makes `pairing/new-code` and `pairing/revoke-all` local-transport-only; and #10246, merged after the baseline, refuses a *remote* resume of a session that holds local channel capabilities while letting local reconnects through, deciding "remote" by transport (f151c2e00d). Once a gateway relays remote browsers over the local socket, every relayed request arrives as `Local`. Rule: a check that exists to mean "the operator at this machine" is re-expressed as **"the peer runs as the daemon's own OS account"** (Unix: peer uid equals the daemon uid; Windows: client SID equals the daemon's, `[proposed]` with the SID check of ID-5). In the default same-account deployment a relayed request still passes, which matches §5.6: that gateway is operator-equivalent anyway. In hardened mode the gateway's account differs and these operations are refused for relayed callers, which is the intended narrowing. Checks that exist for resource reasons (chunked upload bounds) keep the transport test. CT-AUTH-04 covers each such method from a separate-account client.

### 5.9 Credential handoff (normative)

- **HAND-1.** The gateway holds end-user credentials in memory only, for as long as the HTTP session or connection that presented them needs a pooled connection; it never writes them to disk, never logs them (redaction test CT-HAND-01 greps gateway logs from the conformance run for every test credential), and never sends them anywhere but a verified core endpoint (§5.5).
- **HAND-2.** Credentials cross from gateway to core only inside `initialize` (`auth_token`) or, for ingress, as evidence fields of §12 methods. They are never placed in any other method's params.
- **HAND-3.** The core sends credential material to the gateway in exactly one case: the new bearer returned by the pairing exchange (§8.8), which the gateway relays to the browser that presented the code and does not retain. Config reads over RPC return secrets masked (`config/get` full read, `rt/rpc/types.rs:682`); CT-HAND-02 asserts no `#[secret]` field value appears in any config method result.
- **HAND-4.** The core never retains a bearer beyond `initialize`: it keeps the native token's SHA-256 for liveness checks and non-secret evidence (`rt/rpc/auth.rs:46-82`). An OIDC binding whose verifier policy changes must re-`initialize` (`auth.rs:275-277`, `:500-515`); the gateway handles the resulting `-32010` by re-initializing with the current token, or by returning 401 when it holds none.

## 6. Handshake: `initialize`

### 6.1 Wire shape `[master]`

Field names are **snake_case** except `clientCapabilities`: the `rpc_type!` macro applies `rename_all = "snake_case"` (`rt/rpc/types.rs:22-41`) and `client_capabilities` carries an explicit `rename` (`:62-67`). No param struct uses `deny_unknown_fields`, so a misspelled field is ignored, not rejected.

```jsonc
// client → core
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "protocol_version": 1,              // default 1 when absent (types.rs:49-50, :82-84)
  "auth_token": "<bearer>",           // optional (types.rs:68-72)
  "auth_provider": "native",          // optional; "native" | "peercred" | "oidc.<alias>" (types.rs:73-78)
  "clientCapabilities": {"elicitation": {"form": true}},   // optional object (types.rs:62-67; read at dispatch.rs:3139-3144)
  "tui_id": "…", "tui_sig": "…",      // optional reconnect continuity; grants nothing (dispatch.rs:3146-3150)
  "env": {}                           // optional; see §6.3
}}
// core → client
{"jsonrpc":"2.0","id":1,"result":{
  "protocol_version": 1, "server_version": "0.9.0", "server_pid": 4242,
  "daemon_instance_id": "…",         // [proposed] random UUID fixed for the daemon process's life (DEC-22, §10.2)
  "tui_id": "…", "tui_sig": "…",
  "capabilities": ["initialize","status", …],   // every Method::ALL wire name (dispatch.rs:3239-3242)
  "auth_methods": ["native","peercred","oidc.corp"],
  "principal_id": "…",
  "commands": [ … ]                   // TUI command catalogue, always serialized (types.rs:118-124)
}}
```

Errata this contract adopts for `rpc-socket.md` (a docs fix under #11001): the page shows `{"protocolVersion":1}` and a result with `protocolVersion`/`serverVersion`, and `session/update` params with `sessionId`/`toolCallId`/`rawInput`. The wire uses `protocol_version`, `server_version`, `session_id`, `tool_call_id`, `raw_input` (`rt/rpc/types.rs:47-126`, `:1596-1700`; the notification is built from the typed enum at `rt/rpc/dispatch.rs:11469-11560`). The documented request "works" only because the unknown `protocolVersion` is ignored and `protocol_version` defaults to 1.

### 6.2 Semantics `[master]` unless tagged

- The first frame on a connection MUST be `initialize`; any other method gets `-32010` (i18n key `rpc-auth-first-call-initialize`, `dispatch.rs:1099-1105`).
- Version check first: `protocol_version != 1` → `-32011` with a message only, no `data` (`dispatch.rs:3129-3137`). `[proposed]` add `data: {"reason":"protocol_version","server_supported":[1],"client":N,"server_version":"…"}` (§7.3).
- Authentication before any registry mutation (`dispatch.rs:3146-3164`); routing per §5.1.
- `capabilities` lists **every** method the core implements, not the methods this principal may call. A client learns authorization only by calling. `[proposed]` keep it that way: an authorization-filtered list would disclose policy and go stale at the next policy generation.
- Re-`initialize` on a bound connection is allowed (`initialize` is the handshake sentinel, `dispatch.rs:363-374`). A successful one replaces the binding; a failed one returns the error and **leaves the previous binding in place** (`self.auth` is assigned only on success, `dispatch.rs:3226`). OIDC revalidation relies on this (§5.9 HAND-4).
- Each successful `initialize` registers a TUI entry (`dispatch.rs:3191-3206`) visible to `tui/list`; a gateway pool therefore appears there as many entries. `[proposed]` `clientCapabilities.client_kind: "gateway"` lets `tui/list` label them; it grants nothing.

### 6.3 Environment forwarding

`env` is retained only for a **local** connection whose grants include `admin` (`dispatch.rs:1889-1921`), and sessions created on that connection spawn tools with it. **The gateway MUST NOT send `env`**: its own environment is not the user's, and anything derived from an HTTP request is attacker-controlled. CT-HS-03 asserts the gateway's `initialize` frames carry no `env`.

## 7. Version compatibility

### 7.1 Today `[master]`

| Mechanism | Behaviour | Cite |
|---|---|---|
| Protocol version | integer `1`, exact match, `-32011` otherwise | `rt/rpc/dispatch.rs:36`, `:3129-3137` |
| Product version | `server_version = CARGO_PKG_VERSION` in `initialize` and `status` | `dispatch.rs:3255-3258`, `:3470-3473` |
| zerocode gate | refuses any daemon whose `server_version` differs from its own package version | `zc/client.rs:525-558`, `:637-638` |
| zerocode's protocol value | sends `jsonrpc::ACP_PROTOCOL_VERSION` (also `1`) as `protocol_version`: equal today by coincidence, not by construction | `zc/client.rs:1746`, `:2030`; `api/jsonrpc.rs:291` |
| Method discovery | `capabilities` = `Method::ALL` | `dispatch.rs:3239-3242` |
| Unknown method | `-32601` | `dispatch.rs:2907-2920` |

### 7.2 Compatibility rules `[proposed]` (normative once accepted)

- **V-1 Additive-only within a protocol version.** Allowed without a version bump: a new method; a new optional param with a default equal to the old behaviour; a new result field; a new notification method; a new `session/update` `type`; a new `data` member on an error; a new `reason` value. A client MUST ignore unknown result fields, unknown notification methods and unknown `session/update` types rather than fail. This constrains typed clients: the `SessionUpdateEvent` enum (`rt/rpc/types.rs:1596-1597`) has no catch-all variant, so a client that deserializes into it fails on a new type. The proto crate MUST give client-side event enums an `Unknown`/other arm; at #11165's head the proto enum has nine variants and no catch-all (`crates/zeroclaw-rpc-proto/src/types.rs:1485-1593` @f74d25a0f4).
- **V-2 What needs a new protocol version or an opt-in capability.** Removing or renaming a method or field, changing a field's type, or any other change to an existing call's observable semantics that V-5 or V-6 does not cover. Example: session-owned turns (#11185) change what a disconnect does. That change is carried by an opt-in client capability, not by redefining v1 for old clients (§9.3).
- **V-5 Security corrections stay inside v1 (DEC-29).** A change that makes the core refuse, withhold or re-check something the documented authority model never granted (ADR-017; AUTH-1; the method's `authz()`, selectors and ownership predicates; the credential rules of §5) ships inside protocol 1, without a capability or a version change: keeping the old behaviour for old clients would keep the hole open for them. Conditions, all required: (1) it only narrows and never widens anyone's authority; (2) it fails closed: when the check cannot be evaluated the answer is a refusal; (3) every new refusal carries a stable signal, the existing code (`-32010`, `-32012`, `-32003`, `-32602`) plus a `data.reason` listed in the compatibility register (Appendix D); (4) the change is entered in that register with the affected methods, who is affected, and the migration for the supported clients (zerocode, the gateway, the desktop app) and for configuration; (5) the release notes repeat the entry. A client MUST treat the new refusal of a call it used to make as final for that credential (ID-4), never as a reason to retry with another credential.
- **V-6 Conformance corrections and widenings.** A method that starts doing what its documented params or its HTTP twin already promised (for example `cron/add` honouring `prompt`, `job_type`, `session_target`, `model`, `allowed_tools` and `delete_after_run`, which master accepts and ignores) is a conformance correction: allowed within v1, registered and release-noted as V-5 requires. A change that **widens** what a principal may do (for example #11185 letting the owning principal cancel from any client) is not a security correction: it is allowed within v1 only as the implementation of an accepted authorization rule (here TL-3), and it is registered as a widening.
- **The register.** Appendix D classifies all 27 existing methods the open PRs change (§15.1) as additive (V-1), capability-gated (V-2), security correction (V-5), conformance correction or widening (V-6). None needs protocol 2.
- **V-3 Negotiation.** The integer stays. A future protocol 2 adds an optional `min_protocol_version`; the core answers with the highest version both support and keeps serving 1 for a full minor release after 2 ships. No protocol 2 is proposed for v0.9.0.
- **V-4 Required versus optional features.** A gateway computes the method set each of its routes needs (the route table in §15.3). At startup it compares that set with `capabilities`:
  - a missing **mandatory** method (`initialize`, `status`, `health`, and the D1 identity methods if D1-A is chosen) → the gateway refuses to serve domain routes and serves one diagnostic page naming both product versions and the missing method;
  - a missing **route** method → that route answers `503` with `{"error":"core_capability_missing","method":"<wire name>","core_version":"…"}`, logged once.
  Security properties are never an optional feature: if the core cannot perform endpoint verification or the identity methods the gateway was configured for, the gateway does not start, whatever the protocol overlap.

### 7.3 Product-version skew **OPEN D6**

| Option | Rule | Consequence |
|---|---|---|
| D6-a | exact `server_version` equality (zerocode's rule) | simplest; every patch release must update both binaries together; a Homebrew/Scoop partial upgrade strands the dashboard |
| D6-b (recommended) | same `major.minor`; patch skew allowed with a dashboard banner; cross-minor refused with a diagnostic naming both versions and which binary is older | matches V-1; needs the V-4 route-level degradation to be real |
| D6-c | protocol version + capability check only | maximal flexibility; a semantic change inside v1 (a V-1 violation) would go unnoticed |

The desktop bundle checks exact equality regardless: the release workflow builds the app and its sidecar from one commit and stamps the app version from `Cargo.toml` (`.github/workflows/release-stable-manual.yml:442-452`). zerocode's exact gate (`zc/client.rs:637-638`) is independent of this decision; relaxing it is a separate zerocode change.

`-32011` gains structured `data` `[proposed]`: `{"reason":"protocol_version","client":N,"server_supported":[1],"server_version":"0.9.0"}` so the refusing side can name the older binary.

## 8. Operation and event catalog

### 8.0 How to read this section

Each table lists one domain. Columns:

- **Params / Result**: wire field names (snake_case unless noted), `?` = optional. Field types are normative in the OpenRPC document where it types them.
- **OpenRPC P/R**: how #11165's generated document describes params/result: `T` typed schema, `X` external (named type owned by `zeroclaw-runtime`/`zeroclaw-api`/`zeroclaw-config`, no schema exported), `U` untyped (free-form JSON), `–` none. Counts at #11165 head: params 52 T / 17 X / 7 U / 21 none; results 62 T / 18 X / 17 U; 57 of 97 methods are fully described.
- **Retry**: the contract's retry class, which clients MUST follow. `R` read: safe to repeat. `R*` read that opens a subscription: a repeat opens a second one, which the client must cancel. `W` idempotent write: repeating after an unknown outcome converges on the same state (the result may differ, for example `deleted: false`), **and** the request names its target by an identity that cannot come to denote a different object between the two sends: a core-generated id (cron job id, `run_id`, `upload_id`, `subscription_id`, `turn_id`), or a key whose last-writer-wins semantics the caller accepts (memory keys, config paths, skill and personality files, canvas ids). A replay can still overwrite a change another client made in between, as with HTTP `PUT`/`DELETE`. `N` not idempotent: after an unknown outcome the client MUST NOT repeat; it reads state (`session/state`, `cron/list`, `sops/runs`, …) to learn what happened (§10.2 step 6). **Session ids are not stable targets**: `session/new` creates a session under a caller-chosen id that does not exist yet (`rt/rpc/dispatch.rs:3661-3681`, "a brand-new id passes because it exists nowhere yet"), so a removed session's id can be re-created, and `session/cancel`/`session/abort` act on whatever turn is current (`dispatch.rs:6701-6715`; #11185 keeps this, `rt/rpc/session.rs:1790-1798` @6713ff419b). Every method that targets a session only by `session_id` is therefore `N`, including `session/cancel` and `session/abort`. Targeted cancellation is a separate pair of methods, `session/cancel-turn` and `session/abort-turn` (DEC-22, §9.3 TL-3), which are `W`: a client uses them only after it sees them in `initialize`'s `capabilities`, because an older protocol-1 handler would silently ignore an extra `turn_id` field (`SessionIdParams` has only `session_id`, `rt/rpc/types.rs:201-205`, and unknown fields are not denied) and cancel whatever turn is current.
- **Status**: `[master]` with the handler line in `rt/rpc/dispatch.rs`, or `[PR #N]`.
- **Dispatcher tests**: tests in `rt/rpc/dispatch.rs` that reference the method (a name-based scan of the test module: count and one example). These run the dispatcher directly, not over a socket. Socket- and pipe-level coverage is in §15.2.

Schema requirements for the split `[proposed]` (#11001 / #11165):

- **Params must be an object.** A method whose params are a struct requires `params` to be a JSON object, even when every field is optional: an absent `params` becomes `null` (`api/jsonrpc.rs:200-201`), and `parse_params` refuses `null` with `-32602` (`rt/rpc/dispatch.rs:11024-11026`). Clients MUST send `{}` in that case; methods whose params column is `—` ignore `params`. serde also accepts a positional array for a struct, in field order; that is accidental and not part of the contract, so clients MUST send objects. `[master]`
- **SCH-1.** Every method a gateway route calls MUST have `T` params and `T` result in `zeroclaw-rpc-proto` before that route moves out of process. An `X` side names a runtime type the gateway cannot link without linking the runtime; a `U` side has no schema in the document. **The wire shapes themselves are specified now, in Appendix C**, for every `X` and `U` payload a gateway route uses (derived from the serde definitions at f0ae8c8bd8, with each type's source line); the proto crate's schemas MUST match Appendix C, and a change to either is a protocol change under §7.
- **SCH-2.** Correct `session/prompt`'s result: the core replies `{}` when the turn ends and discards the `SessionPromptResult` it builds (`rt/rpc/dispatch.rs:2946-2962`), while the document declares `SessionPromptResult` with required `session_id`, `stop_reason`, `content` (#11165 `method.rs:306`). The contract adopts `{}`; the terminal outcome is `turn_complete` (§9.1). Nothing in the runtime reads `Method::contract()`, so the drift check cannot catch this class of mismatch; a dispatcher test that asserts each method's actual response against its declared result schema is required (CT-SCH-02).
- **SCH-3.** Add `-32004 CONNECTION_LIMIT_REACHED` to the error table (missing from `error_codes::ALL` at #11165 head although the listener sends it, `rt/rpc/local.rs:131-140`).
- **SCH-4.** Describe core→client requests (`elicitation/create`) and notifications in the document, not only in prose and a non-standard `x-notifications` key.
- **SCH-5.** Give each method an `errors` list of the ZeroClaw codes it can return. The lists are specified in Appendix E (master and proposed methods); the document MUST carry them.
- **SCH-6.** Appendix C.4 and E.3 type the 49 methods the open PRs add, at the heads in §15.1. A PR that adds or changes a method adds, in the same PR, its typed params and result and its error list to `zeroclaw-rpc-proto` and the generated document, matching Appendix C, and a PR whose shapes move before it merges updates C.4 and E.3 in the same review cycle; once merged, the proto crate and the document, which must agree with the contract, are the machine-readable form (DEC-28).

### 8.1 Core and system

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `initialize` | Handshake | {protocol_version?, auth_token?, auth_provider?, clientCapabilities?, tui_id?, tui_sig?, env?} | {protocol_version, server_version, server_pid, tui_id?, tui_sig?, capabilities?, auth_methods?, principal_id?, commands?} | T/T | N | `[master]` `dispatch.rs:3126` | 28: `a_reconnect_survives_the_displaced_connections_teardown` |
| `status` | System:Read | none; `[PR #11382]` `{overview?, agent?}` | {server_version, protocol_version, active_sessions, session_ids, config_dir?, config_file?, config_kind?, local_ipc_endpoint?, shell_profile?}; `[PR #11382]` `overview?` (C.6) | –/T | R | `[master]` `dispatch.rs:3453` | 10: `cron_runs_serves_a_scoped_principal_its_own_jobs_history` |
| `health` | System:Read | none | `HealthSnapshot` + `process: ProcessStats` (Appendix C); `[PR #11345]` adds `components.gateway.bound_addr?` (C.6) | –/U | R | `[master]` `dispatch.rs:3483` | none at the baseline; #11345 adds `health_reports_the_gateway_bound_address_only_once_bound` |
| `doctor/run` | System:Execute | none; `[PR #11382]` `{static_only?}` (C.6) | {results, summary{ok,warnings,errors}, log_path?, timed_out_phase?} | –/X | R | `[master]` `dispatch.rs:3495` | 4: `doctor_omits_log_path_after_config_set_to_rolling_without_reload_when_writer_disabled` |
| `cert/renew` | Handshake | {csr_pem} | {cert_pem, ca_chain_pem, device_id, not_after, relay_profile} | U/U | N | `[master]` `dispatch.rs:3273` | 10: `cert_renew_gates_on_ledger_status` |
| `metrics/scrape` | System:Read | none | {content_type, text} | n/a | R | `[PR #11182]` | P+GP: `metrics_scrape_equals_the_metrics_route_without_prometheus` |
| `system/restart` | System:Execute | {component} | {component: "daemon", restarting: true}: a daemon **reload**, which drops every connection | n/a | N | `[PR #11182]` | admin-only; `system_restart_restarts_only_the_daemon_and_needs_a_supervisor` |
| `system/upgrade` | System:Execute | {version?, auto_restart?} | {handoff_id} | n/a | N | `[PR #11182]` | admin-only; `system_upgrade_honors_allow_self_upgrade` |
| `system/upgrade-status` | System:Read | {handoff_id?} | {state, handoff_id?, phase?, log_tail?, previous_version?, target_version?, restart_mode?, restart_hint?, error?} | n/a | R | `[PR #11182]` | `system_upgrade_status_matches_the_status_route` |
| `a2a/identity` | System:Read | {agent?} | A2A `AgentCard` (camelCase, A2A wire type) | n/a | R | `[PR #11182]` | `a2a_identity_matches_the_well_known_card_routes`; public discovery through the service allowlist (§8.13, DEC-25) |
| `system/version-check` | System:Read | {force?, version?} (`SystemVersionCheckParams`) | `VersionCheckResponse` {current_version, latest_version: string\|null, is_newer, release_url?, release_notes?, published_at?, error?}; a failed check is a result with `error`, never an RPC error | T/T (#11377) | R | `[PR #11377]` | replaces `GET /api/version/check` (`gw/version.rs:173-306`): the core runs its own `zeroclaw update --check --json` with today's 1 h cache for the unforced latest check, and resolves the caller's authority again once the check finishes (C.6). In a separate gateway `current_exe()` would name `zeroclaw-gw`, so the check belongs in the core. #11182 moves the same check into `self_upgrade` for its `system/upgrade` methods; whichever lands second keeps one copy |

### 8.2 Sessions, messages and streaming

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `session/new` | Sessions:Create | {agent_alias, cwd?, session_id?, tui_id?, exclude_memory?, chat_mode?, interaction_surface?, keep_siblings?} | {session_id, agent_alias, message_count, workspace_dir} | X/T | N | `[master]` `dispatch.rs:3653` | 20: `authz_classification_spot_checks` |
| `session/prompt` | Sessions:Execute | {session_id, prompt, client_turn_generation?, attachments?} | `{}` on turn end (**not** `SessionPromptResult`; SCH-2) | T/T | N | `[master]` `dispatch.rs:5504` | 40: `acp_cancel_retains_provider_safe_live_history_for_follow_up` |
| `session/cancel` | Sessions:Update | {session_id} | {session_id, cancelled} | T/T | N | `[master]` `dispatch.rs:6652` | 5: `acp_cancel_retains_provider_safe_live_history_for_follow_up` |
| `session/approve` | Sessions:Update | {session_id, request_id, decision, replacement?} | {session_id, request_id, acknowledged} | T/T | N | `[master]` `dispatch.rs:7276` | 3: `approvals_authorize_against_the_bound_session_owner` |
| `session/state` | Sessions:Read | {session_id}; `[PR #11381]` `session_keys?` (C.6) | {session_id, state, turn_id?, turn_started_at?, plan?} | T/T | R | `[master]` `dispatch.rs:7071` | 3: `acp_mode_session_prompt_leaves_chat_backend_state_absent` |
| `session/messages` | Sessions:Read | {session_id, limit?, before_index?, cursor?}; `[PR #11381]` `session_keys?`, `max_bytes?` (C.6) | {session_id, messages, total?, start?, next_cursor?, has_older?} | T/T | R | `[master]` `dispatch.rs:6838` | 2: `live_acp_messages_do_not_wait_for_turn_queue_or_promote_checkpoint` |
| `session/list` | Sessions:Read | {query?, limit?} | {sessions} | T/T | R | `[master]` `dispatch.rs:6735` | 2: `can_list_sessions` |
| `session/list-acp` | Sessions:Read | none (ignored) | {sessions} | –/T | R | `[master]` `dispatch.rs:6801` | 1: `acp_turn_complete_and_list_acp_report_the_same_projected_count` |
| `session/git_branch` | Sessions:Read | {session_id} | {session_id, branch?, hash?} | T/T | R | `[master]` `dispatch.rs:6717` | none found |
| `session/configure` | Sessions:Update | {session_id, overrides?} | {session_id, overrides} | T/T | N | `[master]` `dispatch.rs:6464` | 12: `configure_refuses_an_incarnation_replaced_under_the_lock` |
| `session/close` | Sessions:Update | {session_id} | {session_id, closed} | T/T | N | `[master]` `dispatch.rs:4583` | 11: `acp_persistence_removals_cancel_after_admission_before_fallible_setup` |
| `session/kill` | Sessions:Delete | {session_id} | {session_id, killed} | T/T | N | `[master]` `dispatch.rs:4720` | 12: `acp_persistence_removals_cancel_after_admission_before_fallible_setup` |
| `session/delete` | Sessions:Delete | {session_id}; `[PR #11381]` `session_keys?` (C.6) | {session_id, deleted} | T/T | N | `[master]` `dispatch.rs:7132` | 15: `acp_persistence_removals_cancel_after_admission_before_fallible_setup` |
| `session/steer` | Sessions:Execute | {session_id, content} | {session_id, accepted} | n/a | N | `[PR #11132]` | 12: `session_steer_reaches_the_running_turn_and_turn_complete_carries_totals` |
| `session/abort` | Sessions:Delete | {session_id} | {session_id, cancelled} | n/a | N | `[PR #11132]` | 4: `session_abort_interrupts_a_turn_another_client_started` |
| `session/append` | Sessions:Update | {session_id, content} | {session_id, message_count} | n/a | N | `[PR #11132]` | 8: `session_append_and_rename_write_the_durable_and_live_session` |
| `session/rename` | Sessions:Update | {session_id, name} | {session_id, name} | n/a | N | `[PR #11132]` | 1: `session_append_and_rename_write_the_durable_and_live_session` |
| `session/run-once` | Sessions:Execute | {agent_alias, prompt, cwd?, session_id?, exclude_memory?} | {session_id, stop_reason, content, usage?} | n/a | N | `[PR #11132]` | 5: `session_run_once_runs_one_turn_and_closes_its_session`; also needs `sessions:create` |
| `session/attach` | Sessions:Read | {session_id, since_seq?, epoch?} | {session_id, subscription_id, seq, epoch, running} | n/a | R* | `[PR #11185]` | 16: `attach_replays_the_ring_and_then_streams_live` |
| `session/cancel-turn` | Sessions:Update (ownership as `session/cancel`, TL-3) | {session_id, turn_id} | {session_id, turn_id, cancelled} | n/a | W | `[proposed]` | cancels only if the session's running turn has `turn_id`; otherwise `cancelled: false`, not an error (DEC-22) |
| `session/abort-turn` | Sessions:Delete (the `session/abort` rule) | {session_id, turn_id} | {session_id, turn_id, cancelled} | n/a | W | `[proposed]` | as above, recorded with cause `operator_abort` |

### 8.3 Files and uploads

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `file/attach` | Files:Create | {session_id, files} | {files} | T/T | N (targets a session id; the stored bytes are content-addressed, so a replay into the same incarnation only deduplicates) | `[master]` `dispatch.rs:9534` | 7: `chunked_upload_lands_a_multi_chunk_payload_like_file_attach` |
| `file/upload/begin` | Files:Create | {session_id, filename?, size_bytes, sha256?} | {upload_id, chunk_bytes, max_bytes} | T/T | N | `[master]` `dispatch.rs:9740` | 8: `a_publication_racing_the_final_check_finishes_only_after_the_upload_is_stored` |
| `file/upload/chunk` | Files:Create | {upload_id, offset, data_b64} | {received_bytes} | T/T | W | `[master]` `dispatch.rs:9764` | 5: `chunked_upload_commit_rechecks_the_agent_grant` |
| `file/upload/commit` | Files:Create | {upload_id} | {ref_id, marker, workspace_path, size_bytes, deduplicated} | T/T | W | `[master]` `dispatch.rs:9781` | 15: `a_publication_racing_the_final_check_finishes_only_after_the_upload_is_stored` |
| `fs/list_dir` | Files:Read | {path, show_hidden?} | {entries, cwd} | X/X | R | `[master]` `rpc/fs.rs:121` (authorized at `dispatch.rs:1726`) | 1: `list_dir` |
| `fs/read` | Files:Read | {agent, path} | {path, size, is_text, content, encoding: "utf8"\|"base64"} | n/a | R | `[PR #11182]` | P: `an_alias_cannot_escape_the_agents_tree` |
| `fs/mkdir` | Files:Create | {agent?, path} | {created} | n/a | W | `[PR #11182]` | P: `an_overdeep_path_is_refused_before_anything_is_made` |
| `fs/rmdir` | Files:Delete | {path} | {removed} | n/a | W | `[PR #11182]` | P: `fs_delete_and_rmdir_refuse_a_path_that_resolves_to_the_root` |
| `fs/delete` | Files:Delete | {agent, path} | {removed} | n/a | W | `[PR #11182]` | P: `a_workspace_operation_withdrawn_while_it_waits_does_not_take_effect` |
| `fs/move` | Files:Update | {agent, from, to} | {from, to} | n/a | N | `[PR #11182]` | P: `a_workspace_operation_withdrawn_while_it_waits_does_not_take_effect` |
| `workspace/list` | Files:Read | {agent?, path?} | {path, entries} | n/a | R | `[PR #11182]` | P: `a_scoped_principal_cannot_reach_another_agents_workspace` |

`file/attach` with a `path`, and the three chunked-upload methods, are served on local transport only: a WSS peer gets `-32012` (`rpc-socket.md` "Chunked uploads"), and #11186's in-process transport refuses them too. A gateway that relays browser uploads therefore needs the real socket, and §5.8.1 applies to what "local" means once it does.

### 8.4 Memory

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `memory/list` | Memory:Read | {category?, session_id?, agent?, plane?}; `[PR #11376]` `content_max_chars?` | {entries, count} | T/T | R | `[master]` `dispatch.rs:7372` | 2: `memory_agent_parameter_is_bound_by_the_agent_selector` |
| `memory/search` | Memory:Read | {query, limit?, session_id?, since?, until?, agent?, plane?}; `[PR #11376]` `content_max_chars?` | {entries, count} | T/T | R | `[master]` `dispatch.rs:7401` | 2: `memory_agent_parameter_is_bound_by_the_agent_selector` |
| `memory/get` | Memory:Read | {key, agent?, plane?} | {entry?} | T/T | R | `[master]` `dispatch.rs:7439` | 2: `memory_agent_parameter_is_bound_by_the_agent_selector` |
| `memory/store` | Memory:Create | {key, content, category?, session_id?, agent?, plane?} | {key, stored} | T/T | W | `[master]` `dispatch.rs:7464` | 2: `memory_agent_parameter_preserves_owner_and_agent_boundaries` |
| `memory/delete` | Memory:Delete | {key, agent?, plane?} | {key, deleted}; `[PR #11376]` `deleted` reports whether an entry was removed (always `true` on master) | T/T | W | `[master]` `dispatch.rs:7497` | 3: `authz_classification_spot_checks` |

### 8.5 Cron

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `cron/list` | Cron:Read | none | {jobs} | –/X | R | `[master]` `dispatch.rs:7521` | 1: `cron_list_hides_jobs_owned_by_other_agents` |
| `cron/get` | Cron:Read | {id} | CronJob | T/X | R | `[master]` `dispatch.rs:7534` | 2: `cron_admin_still_reaches_ownerless_legacy_rows` |
| `cron/add` | Cron:Create | {agent, schedule, tz?, command?, prompt?, name?, job_type?, delivery?, session_target?, model?, allowed_tools?, delete_after_run?} | CronJob | X/X | N | `[master]` `dispatch.rs:7541` | 1: `cron_add_requires_the_agent_selector` |
| `cron/patch` | Cron:Update | {id, agent, name?, schedule?, tz?, clear_tz?, command?, prompt?} | CronJob | T/X | W | `[master]` `dispatch.rs:7562` | 2: `cron_patch_and_delete_cannot_reach_a_foreign_job` |
| `cron/delete` | Cron:Delete | {id} | {id, deleted}; `[PR #11376]` for the administrator grant, also the retained run history of a job whose row is gone, as the HTTP route removes it; at that head an administrator's id with neither answers `-32603`, not `-32602` `[pending fix round]` (C.6) | T/T | W | `[master]` `dispatch.rs:7604` | 1: `cron_patch_and_delete_cannot_reach_a_foreign_job` |
| `cron/runs` | Cron:Read | {id, limit?} | {runs}; `[PR #11376]` for the administrator grant, also the retained history of a job whose row is gone, as the HTTP route lists it | T/X | R | `[master]` `dispatch.rs:7622` | 2: `cron_get_runs_and_trigger_refuse_a_foreign_job_as_not_found` |
| `cron/trigger` | Cron:Execute | {id} | {id, success, status, output, duration_ms, started_at, finished_at} | T/T | N | `[master]` `dispatch.rs:7640` | 4: `authz_classification_spot_checks` |
| `cron/settings` | Cron:Read | `{}` | `SchedulerConfig` {enabled, max_tasks, max_concurrent, catch_up_on_startup, max_run_history} (Appendix C) | U/U | R | `[master]` `dispatch.rs:7663` | none found. The handler's `{patch}` branch answers not-implemented (`dispatch.rs:7670-7672`) and is **not** part of the contract; clients MUST NOT send `patch` |

**Cron settings writes** go through `config/set-many` (Config:Update plus the config write selector for exactly those paths, retry `W`), not through a cron method: `PATCH /api/cron/settings` becomes `config/set-many {sets: [{prop: "scheduler.enabled", value}, {prop: "scheduler.catch-up-on-startup", value}, {prop: "scheduler.max-run-history", value}]}` with only the fields present in the PATCH body (the same three the route writes today, `gw/api.rs:870-917`), followed by `cron/settings` to build today's response body `{status: "ok", enabled, catch_up_on_startup, max_run_history}`. This closes the route's authority gap: today it writes config after only the pairing check, outside the principal gate (§15.3 row 17).

### 8.6 Agent status and connected clients

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `agents/list` | Agents:Read | none | {agents} | –/T | R | `[master]` `dispatch.rs:8731` | none found |
| `agents/status` | Agents:Read | none | {agents} | –/T | R | `[master]` `dispatch.rs:8745` | none found |
| `tui/list` | Tui:Read | none | {tuis} | –/T | R | `[master]` `dispatch.rs:3531` | none found |
| `agents/delete-preview` | Agents:Read | {alias} | {alias, allowed, blockers, scrubs, owned_state} | n/a | R | `[master after the baseline]` #10621, `dispatch.rs:9682` @f151c2e00d | see C.5 |
| `agents/delete` | Agents:Delete | {alias} | {alias, deleted, scrubbed, warnings, error?}; a blocked delete is `deleted: false` with `error`, not an RPC error | n/a | N | `[master after the baseline]` #10621, `dispatch.rs:9719` @f151c2e00d | see C.5 |

### 8.7 Config

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `config/get` | Config:Read | {prop?} | with `prop`: `{prop: string, value: string}`, the property's display string, masked when `#[secret]` (`crates/zeroclaw-macros/src/lib.rs:2159`); without: the whole `Config` document, shaped by the config JSON Schema (`config/schema`), with every non-empty secret replaced by `"***MASKED***"` (`cfg/traits.rs:463`, `:470`) | T/U | R | `[master]` `dispatch.rs:7679` | none found |
| `config/schema` | Config:Read | {path?} | `{schema, etag, prop?}`: `schema` is the config JSON Schema document the gateway serves today; `prop` (with `path`) is `{path, kind: PropKind, type_hint, is_secret, enum_variants, category}` | n/a | R | `[proposed]` | replaces `OPTIONS /api/config` and `OPTIONS /api/config/prop` (`gw/api_config.rs:2700-2784`). Unknown `path` → `-32602`, `data.reason: "path_not_found"` (404). The form without `path` is public (DEC-25); the `path` form resolves against the live config and so reveals whether a map key exists: proposed to require the caller's credential (D13) |
| `config/set` | Config:Update | {prop, value} | {prop, set} | T/T | W | `[master]` `dispatch.rs:7696` | 48: `acp_rehydration_inside_snapshot_window_converges_on_committed_generation` |
| `config/set-many` | Config:Update | {sets} | {props, set} | T/T | W | `[master]` `dispatch.rs:7772` | 11: `config_set_many_applies_entries_in_order_so_a_later_write_wins` |
| `config/delete` | Config:Delete | {prop} | {prop, deleted} | T/T | W | `[master]` `dispatch.rs:8261` | 3: `config_delete_of_a_uid_is_rejected_and_map_key_rename_republishes_the_roster` |
| `config/validate` | Config:Read | none | {valid, error?} | –/T | R | `[master]` `dispatch.rs:8194` | none found |
| `config/reload` | Config:Update | none | {reloading} | –/T | N | `[master]` `dispatch.rs:8208` | 2: `authz_classification_spot_checks` |
| `config/list` | Config:Read | {prefix?} | {entries} | T/T | R | `[master]` `dispatch.rs:8238` | 2: `oidc_principal_writes_config_at_an_unchanged_generation` |
| `config/map-keys` | Config:Read | {path} | {path, keys} | T/T | R | `[master]` `dispatch.rs:8310` | none found |
| `config/map-key-create` | Config:Create | {path, key} | {path, key, created} | T/T | N | `[master]` `dispatch.rs:8325` | 3: `config_map_key_create_delete_and_rename_recheck_after_admission` |
| `config/map-key-delete` | Config:Delete | {path, key} | {path, key, deleted} | T/T | W | `[master]` `dispatch.rs:8375` | 4: `config_map_key_create_delete_and_rename_recheck_after_admission` |
| `config/map-key-rename` | Config:Update | {path, from, to} | {path, from, to, renamed, warnings?} | T/T | N | `[master]` `dispatch.rs:8470` | 9: `alias_rename_serializes_with_config_set` |
| `config/templates` | Config:Read | none | {templates} | –/T | R | `[master]` `dispatch.rs:8720` | none found |
| `config/sections` | Config:Read | none | {sections} | –/T | R | `[master]` `dispatch.rs:9088` | none found |
| `config/status` | Config:Read | none | {needs_quickstart, reason, has_partial_state, missing} | –/T | R | `[master]` `dispatch.rs:9221` | none found |
| `config/catalog` | Config:Read | none | {model_providers} | –/T | R | `[master]` `dispatch.rs:9243` | none found |
| `config/catalog-models` | Config:Read | {model_provider} | {model_provider, models, pricing?, local, live} | T/T | R | `[master]` `dispatch.rs:9257` | none found |
| `config/resolve-alias-source` | Config:Read | {source} | {source, values} | T/T | R | `[master]` `dispatch.rs:8300` | none found |
| `config/reload-status` | Config:Read | none | {pending_reload} | n/a | R | `[PR #11172]` | 3: `config_reload_status_turns_on_after_an_rpc_config_write` |
| `config/drift` | Config:Read | none | {drifted: [{path, secret?, drifted, in_memory_value?, on_disk_value?}]} | n/a | R | `[PR #11172]` | `parity_golden_drift` |
| `config/agent-options` | Config:Read | none | {channels, channel_types, model_providers, risk_profiles, runtime_profiles, skill_bundles, knowledge_bundles, mcp_bundles, agents} | n/a | R | `[PR #11172]` | `parity_golden_agent_options` |
| `config/section-picker` | Config:Read | {section} | {section, items, help} | n/a | R | `[PR #11172]` | `parity_golden_section_picker` |
| `config/section-select` | Config:Create | {section, key, alias?} | {fields_prefix, created} | n/a | W | `[PR #11172]` | 5: `config_section_select_refuses_a_target_outside_the_selector_before_scaffolding`; authz **Config:Create** |
| `config/delete-plan` | Config:Read | {path, key} | {path, key, allowed, blockers, scrubs, live_acp_sessions?, cascades_owned_state} | n/a | R | `[PR #11172]` | `parity_golden_delete_plan` |
| `config/init` | Config:Update | {section?} | {initialized} | n/a | W | `[PR #11172]` | `config_init_refuses_a_section_outside_the_selector` |
| `config/migrate` | Config:Update | none | {migrated, backup_path?, schema_version} | n/a | W | `[PR #11172]` | `config_migrate_needs_whole_config_write_authority` |
| `providers/refresh-context-window` | Config:Update | {provider_type, alias} | {path, context_window} | n/a | W | `[PR #11172]` | no success test (needs a live provider); authz **Config:Update** |

### 8.8 Pairing

On master the core has **no** pairing method; every code is displayed, minted or redeemed by the gateway (§5.8). #11182 adds operator methods; the redemption path and the first-run display are missing.

| Method | Authz | Params → Result | Status |
|---|---|---|---|
| `pairing/list` | System:Read, admin-only in the handler | `—` → `{devices, count}` | `[PR #11182]` |
| `pairing/new-code` | System:Create, admin-only, local-transport-only | `{rotate?}` → `{success, pairing_required, pairing_code, message}` (returns a code) | `[PR #11182]` |
| `pairing/revoke` | System:Delete, admin-only | `{device_id}` → `{message, device_id}` | `[PR #11182]` |
| `pairing/revoke-all` | System:Delete, admin-only, local-transport-only | `—` → same body as `new-code`: it also mints a new code and revokes the caller's own token | `[PR #11182]` |
| `pairing/code` | System:Read, admin-only, local-transport-only (the `pairing/new-code` rule) | `{}` → `{pairing_required, pairing_code: string\|null, message}` | `[proposed]` reads the current code without minting one: today's `GET /admin/paircode` body minus `success` (`gw/lib.rs:4866-4898`). `zeroclaw gateway get-paircode` moves from the gateway's admin routes (`root/src/main.rs:6817-6860`, `:10781-10816`) to the core socket: show → `pairing/code`, `--new` → `pairing/new-code`, `--rotate` and `--rotate-device` → `pairing/revoke-all` / `pairing/revoke` then `pairing/new-code` |
| `pairing/device-capabilities` | System:Update; the handler requires the connection to be bound by a native pairing token and writes only that token's device row | `{capabilities: string[]}` → `{capabilities: string[]}` | `[proposed]` replaces `POST /api/devices/me/capabilities` (`gw/api_pairing.rs:592-644`), which identifies the device by the bearer's SHA-256. Retry `W` (replace semantics; the target is the connection's own device). Errors: not a native device token → `-32012`, `data.reason: "native_device_token_required"`; registry disabled → `-32603`, `"device_registry_disabled"` (503); no row for the token → `-32602`, `"device_not_found"` (404). The device registry moves to the core with pairing |
| `pairing/redeem` | service profile only (D1-A method allowlist, §8.13) | `{code, client_key, device_label?}` → `{token, device_id}` | `[proposed]` replaces `/pair` and `/api/pair`. The core applies its attempt limiter keyed by `client_key` **and** a core-wide redemption budget (a `client_key` from the gateway is not a reliable identity). The returned bearer goes to the browser that presented the code and is not retained by the gateway (HAND-3). |

Rules: `pairing/new-code` and anything that returns a code are never in the gateway service profile; the event bus's broadcast-only pairing frames (`gw/sse.rs:21-30`) are never delivered to a gateway connection; the first-run code is displayed by the core (daemon log, and `zeroclaw gateway get-paircode` over the core socket), not by the gateway banner (`gw/lib.rs:1829-1838` today); the banner's recovery hint, which prints a `curl` against `/admin/paircode/new` (`gw/lib.rs:2577`, `:5276`), changes to the CLI command. Persisting `gateway.paired_tokens` happens only in the core, which ends the split where the gateway writes it today (`gw/lib.rs:2822-2849`).

### 8.9 SOP

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `sops/list` | Sops:Read | none | `Sop[]` (Appendix C; `load_sops_from_directory`, `dispatch.rs:10022-10026`) | –/U | R | `[master]` `dispatch.rs:10022` | 1: `sops_rename_moves_the_sop_and_leaves_one_copy` |
| `sops/get` | Sops:Read | {name} | Sop | X/X | R | `[master]` `dispatch.rs:10028` | none found |
| `sops/graph` | Sops:Read | {name} | SopGraph | X/T | R | `[master]` `dispatch.rs:10036` | none found |
| `sops/run` | Sops:Execute | {name, payload?, dedup_key?} | {run_id} | X/X | N | `[master]` `dispatch.rs:10047` | 6: `authz_classification_spot_checks`; #11373 refuses a procedure with an `execute` step no agent owns before dispatch (`-32602`; C.6) |
| `sops/runs` | Sops:Read | {sop?} | `{runs: SopRunSummary[]}` (`dispatch.rs:10195-10205`) | X/U | R | `[master]` `dispatch.rs:10195` | 2: `sops_runs_accepts_optional_sop_filter` |
| `sops/run-detail` | Sops:Read | {run_id} | (free-form) | X/U | R | `[master]` `dispatch.rs:10210` | 5: `authz_classification_spot_checks` |
| `sops/run-overlay` | Sops:Read | {name, run_id} | RunOverlay | X/X | R | `[master]` `dispatch.rs:10246` | none found |
| `sops/validate` | Sops:Execute | {sop, original_name?} or {name} | (free-form) | U/U | R | `[master]` `dispatch.rs:10415` | none found |
| `sops/save` | Sops:Update | {sop: Sop, original_name?} | `{saved: string}` | X/U | W | `[master]` `dispatch.rs:10433` | 4: `sop_authoring_refuses_procedures_bound_to_an_agent_outside_the_selector` |
| `sops/create` | Sops:Create | {sop: Sop} (`original_name` is accepted and ignored) | `{created: string}` | X/U | N | `[master]` `dispatch.rs:10469` | 2: `sop_authoring_checks_the_definition_as_it_will_load` |
| `sops/delete` | Sops:Delete | {name} | `{deleted: string}` | X/U | W | `[master]` `dispatch.rs:10484` | 2: `sop_authoring_refuses_procedures_bound_to_an_agent_outside_the_selector` |
| `sops/rename` | Sops:Update | {from, to} | `{renamed: string, from: string}`; local transport only | X/U | N | `[master]` `dispatch.rs:10502` | 7: `sops_rename_is_refused_over_remote_wss` |
| `sops/decide` | Sops:Execute | {name, run_id, decision: ApprovalDecision} | RunOverlay; `[PR #11373]` adds `pending_quorum?: true` | X/X | N | `[master]` `dispatch.rs:10268` | 5: `checkpoint_sop`; #11373: the approver is the authenticated identity, and an approval refusal is `-32012`, not `-32010` (not additive, Appendix D) |
| `sops/wire-draft` | Sops:Execute | `{sop: Sop, edit: WireEdit}` | `{sop: Sop, graph: SopGraph}` (`dispatch.rs:10531-10547`); a pure transform, nothing persisted | U/U | R | `[master]` `dispatch.rs:10531` | none found |
| `sops/graph-draft` | Sops:Execute | `{sop: Sop}` | SopGraph | U/T | R | `[master]` `dispatch.rs:10549` | none found |
| `sops/trigger-sources` | Sops:Read | none | TriggerSourceRegistry | –/X | R | `[master]` `dispatch.rs:10566` | 1: `sops_trigger_sources_rpc_carries_full_trigger_source_walk` |
| `sops/cancel` | Sops:Execute | {run_id, name?, reason?} | {run_id, sop_name, outcome, status, already_terminal, run} | n/a | W | `[PR #11169]` | 5: `sops_cancel_is_idempotent_by_run_id` |
| `sops/dispatch-event` | Sops:Execute | {path, payload?} | {status: "accepted"\|"blocked"\|"no_match", source, path, results} | n/a | N | `[PR #11169]` | 15: `sops_dispatch_event_starts_webhook_sops`; not the gateway path (§12.5) |
| `sops/decision-models` | Sops:Read | none | {models: [{alias, provider, model, base_url}]} | n/a | R | `[PR #11169]` | `sops_decision_models_and_graph_legend_serve_the_gateway_shapes` |
| `sops/graph-legend` | Sops:Read | none | {flow_roles, pin_classes, run_states} | n/a | R | `[PR #11169]` | as above |
| `sops/subscribe-runs` | Sops:Read | {sop?} (`SopRunsRequest`, the `sops/runs` params) | `SopsSubscribeRunsResult` {subscription_id, runs: SopRunSummary[]}: the snapshot, armed atomically with the change feed as the WebSocket does today (`gw/ws_sop_runs.rs:82-113`) | X/T (#11377) | R* | `[PR #11377]` | replaces `WS /ws/sops/runs`. Changes arrive as `sops/run-changed` (§8.15), and one can arrive before this result, so a client listens first. A lag is reported with `subscription/lagged`; the client re-subscribes and the new snapshot is the resync (run summaries are state, so no replay is defined). SOP subsystem disabled → `-32603`, `data.reason: "sop_disabled"` (today's `{type: "disabled"}` frame). Visibility is the `sops/runs` rule (any `Sops:Read` holder sees every run, as today), re-checked at every delivery (§10.1). Ends with `subscription/cancel` or the connection (C.6) |

### 8.10 Skills, personality, quickstart, locales, cost

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `skills/bundles` | Skills:Read | none | {bundles} | –/T | R | `[master]` `dispatch.rs:8855` | none found |
| `skills/list` | Skills:Read | {bundle?} | {skills} | T/X | R | `[master]` `dispatch.rs:8873` | none found |
| `skills/read` | Skills:Read | {bundle, name} | {bundle, name, frontmatter, body} | T/X | R | `[master]` `dispatch.rs:8892` | 1: `skills_surfaces_refuse_a_name_outside_the_bundle` |
| `skills/write` | Skills:Update | {bundle, name, frontmatter, body?} | {bundle, name, written} | X/T | W | `[master]` `dispatch.rs:8909` | 2: `authz_classification_spot_checks` |
| `skills/delete` | Skills:Delete | {bundle, name}; `[PR #11384]` `purge?` (C.6) | {bundle, name, deleted} | T/T | W | `[master]` `dispatch.rs:8928` | 1: `skills_surfaces_refuse_a_name_outside_the_bundle` |
| `personality/list` | Personality:Read | {agent?}; `[PR #11384]` `require_configured_agent?` (C.6) | {files, max_chars} | T/T | R | `[master]` `dispatch.rs:8945` | 3: `a_wildcard_agent_selector_cannot_address_an_unconfigured_alias_path` |
| `personality/get` | Personality:Read | {agent, filename}; `[PR #11384]` `require_configured_agent?` (C.6) | {filename, content?, exists, truncated?, mtime_ms?} | T/T | R | `[master]` `dispatch.rs:8994` | 3: `a_wildcard_agent_selector_cannot_address_an_unconfigured_alias_path` |
| `personality/put` | Personality:Update | {agent, filename, content}; `[PR #11176, #11384]` `expected_mtime_ms?`; `[PR #11384]` `require_configured_agent?` (C.4, C.6) | {bytes_written, mtime_ms?} | T/T | W | `[master]` `dispatch.rs:9033` | 3: `a_wildcard_agent_selector_cannot_address_an_unconfigured_alias_path` |
| `personality/templates` | Personality:Read | {agent?}; `[PR #11384]` `preset?`, `agent_name?`, `user_name?`, `timezone?`, `communication_style?`, `include_memory?`, `defaults?` (C.6) | {preset, files} | T/T | R | `[master]` `dispatch.rs:9065` | 3: `a_wildcard_agent_selector_cannot_address_an_unconfigured_alias_path` |
| `quickstart/state` | Quickstart:Read | none | QuickstartStateResult | –/T | R | `[master]` `dispatch.rs:9830` | none found |
| `quickstart/fields` | Quickstart:Read | {section, type_key} | {fields} | X/X | R | `[master]` `dispatch.rs:9835` | none found |
| `quickstart/validate` | Quickstart:Read | {submission} | QuickstartValidateResult (tagged `kind`) | T/X | R | `[master]` `dispatch.rs:9843` | none found |
| `quickstart/apply` | Quickstart:Execute | {submission} | QuickstartApplyResult (tagged `kind`) | T/X | N | `[master]` `dispatch.rs:10626` | 2: `quickstart_apply_rpc_survives_default_worker_stack` |
| `quickstart/dismiss` | Quickstart:Update | {run_id, surface, last_step?} | {recorded} | X/T | W | `[master]` `dispatch.rs:10671` | none found |
| `locales/list` | Locales:Read | none | {locales} | –/U | R | `[master]` `rpc/locales.rs:30` | none found |
| `locales/fetch` | Locales:Read | {locale, catalog?} | {locale, catalogs, skipped} | U/U | R | `[master]` `rpc/locales.rs:51` | none found |
| `cost/query` | Cost:Read | {agent?, from?, to?} | CostSummary | T/X | R | `[master]` `dispatch.rs:8786` | 4: `cost_query_invalid_rfc3339_bound_is_invalid_params` |
| `cost/org` | Cost:Read | none | org cost JSON (free-form) | –/U | R | `[master]` `dispatch.rs:8833` | 4: `cost_org_absent_returns_null` |
| `skills/create` | Skills:Create | {bundle, name, frontmatter, body?, no_scaffold?} | {bundle, name, directory} | n/a | N | `[PR #11176]` | path-traversal case only; no success test |
| `skills/effective` | Skills:Read | {agent} | {agent, skills, dropped?} | n/a | R | `[PR #11176]` | `skills_effective_and_slash_option_kinds_answer_over_rpc` |
| `skills/slash-option-kinds` | Skills:Read | none | {kinds} | n/a | R | `[PR #11176]` | as above |

### 8.11 Logs, events and subscriptions

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `logs/subscribe` | Logs:Read | {since_seq?, epoch?} | {subscribed, subscription_id, seq, epoch} | T/T | R* | `[master]` `dispatch.rs:9279` | 10: `a_future_since_seq_in_the_same_epoch_is_refused` |
| `events/subscribe` | Logs:Read | {since_seq?, epoch?} | {subscribed, subscription_id, seq, epoch} | T/T | R* | `[master]` `dispatch.rs:9290` | 4: `events_subscribe_requires_logs_read` |
| `subscription/cancel` | Logs:Read today (`dispatch.rs:441`); **own connection** `[proposed]` (DEC-31) | {subscription_id} | {cancelled} | T/T | W | `[master]` `dispatch.rs:9299` | 2: `a_logs_reader_can_cancel_its_own_subscription` |
| `events/history` | Logs:Read | none | {events} | –/T | R | `[master]` `dispatch.rs:9411` | 3: `assert_fixture_turn_reaches_subscriber_and_history` |
| `logs/query` | Logs:Read | {since_ts?, until_ts?, until_id?, until_line_offset?, until_segment_cursor?, severity_min?, q?, category?, action?, outcome?, trace_id?, sop_run_id?, hide_internal?, limit?}; `[PR #11382]` `field_eq?`, `report_disabled?` | {events, log_path?, next_cursor?, next_cursor_line_offset?, next_segment_cursor?, at_end, incomplete}; `[PR #11382]` `persistence_enabled`, `daemon_started_at?`, `attribution_keys?` (C.6) | T/T | R | `[master]` `dispatch.rs:9424` | none found |
| `logs/get` | Logs:Read | {id} | {event} | T/T | R | `[master]` `dispatch.rs:9497` | none found |

### 8.12 Workspace, catalog, tools, plugins, canvas, channels

| Method | Authz | Params | Result | OpenRPC P/R | Retry | Status | Dispatcher tests (`rt/rpc/dispatch.rs`) |
|---|---|---|---|---|---|---|---|
| `tools/param-options` | Tools:Read | `{domain: OptionDomain, agent?, args?}` | `{options: OptionEntry[]}` (`dispatch.rs:10579-10622`) | U/U | R | `[master]` `dispatch.rs:10579` | none found |
| `tools/list` | Tools:Read | {agent?} | {tools: [{name, description, parameters, output?, param_domains?}]} | n/a | R | `[PR #11182]` | P: `tools_list_holds_the_agent_to_the_selector`; name collides with MCP `tools/list` |
| `tools/cli-discover` | Tools:Read | none | {cli_tools} | n/a | R | `[PR #11182]` | GP: `cli_discover_matches_the_cli_tools_route` |
| `integrations/list` | Tools:Read | none | {integrations} | n/a | R | `[PR #11182]` | GP: `integrations_list_matches_the_integrations_route` |
| `plugins/list` | Plugins:Read | none | {plugins_enabled, wasm_plugins_available, plugins_dir, plugins, issues} | n/a | R | `[PR #11182]` | GP: `plugins_list_matches_the_plugins_route` |
| `canvas/list` | Canvas:Read | none | {canvases} | n/a | R | `[PR #11182]` | P: `canvas_methods_need_a_canvas_grant`; authz **Canvas:Read** (new resource) |
| `canvas/get` | Canvas:Read | {canvas_id} | {canvas_id, frame} | n/a | R | `[PR #11182]` | as above |
| `canvas/history` | Canvas:Read | {canvas_id} | {canvas_id, frames} | n/a | R | `[PR #11182]` | as above |
| `canvas/render` | Canvas:Update | {canvas_id, content_type?, content} | {canvas_id, frame} | n/a | N | `[PR #11182]` | P: `canvas_render_refuses_what_the_route_refuses` |
| `canvas/clear` | Canvas:Delete | {canvas_id} | {canvas_id, status} | n/a | W | `[PR #11182]` | as above |
| `canvas/subscribe` | Canvas:Read, plus #11182's every-agent rule for the shared store | {canvas_id} | `{subscription_id, canvas_id, frame: CanvasFrame\|null}`: the current frame, replacing today's first `frame` plus `connected` messages (`gw/canvas.rs:210-241`) | n/a | R* | `[proposed]` | replaces `WS /ws/canvas/{id}`. Frames arrive as `canvas/frame` (§8.15); lag via `subscription/lagged`, resync by `canvas/get`. Subscribing to an unknown id creates an empty entry subject to the store's cap, as today (`crates/zeroclaw-tools/src/canvas.rs:145-165`); at the cap → `-32600`, `data.reason: "canvas_capacity"` (the error #11182 uses for the same condition) |
| `channels/list` | Channels:Read | none | {channels} | n/a | R | `[PR #11182]` | P: `channels_methods_route_to_the_registered_capability` |
| `channels/relink` | Channels:Update | {channel} | {channel, outcome, removed, restart_required, note} | n/a | W | `[PR #11182]` | P: `channels_relink_of_a_channel_the_principal_does_not_own_is_refused` |
| `channels/bind` | Channels:Update | {channel_type, alias, identity} | {saved, already_bound, group, channel, restart_required, note?} | n/a | W | `[PR #11182]` | P: `channels_bind_authorizes_the_peer_group_it_writes` |

### 8.13 Gateway control and ingress `[proposed]`

These methods exist only for a gateway process, plus the public-route methods it calls on behalf of unauthenticated callers (DEC-25). Under D1-A the built-in `gateway-ingress` profile is a **method allowlist** evaluated in the core's gate in addition to the `(Resource, Verb)` grant: grants alone are too coarse (a `System:Read` grant would also admit #11182's `pairing/list`). DEC-16.

| Method | Authz (grant + allowlist) | Params → Result | Notes |
|---|---|---|---|
| `gateway/register` | `System:Update` + allowlist | `{instance_id, product_version, listen: {scheme, addr, path_prefix?}, required_methods: [..]}` → `{effective_settings: {<edge keys of §13.1>}, pairing: PairingPosture, core_version, protocol_version, missing_methods: [..]}` | one registration per service connection; presence ends when that connection closes (§12.8). The core treats `listen` as an observation, never as tunnel policy |
| `gateway/status` | `System:Read` (operator) or allowlist | `{}` → `{gateways: [{instance_id, product_version, registered_at, listen}], ingress: "reachable"\|"gateway_absent", public_base_url?}` | operator diagnostics and the desktop app's readiness probe (§11.2 step 5) |
| `gateway/settings` (notification, core→client) | service connection only | `{effective_settings, pairing: PairingPosture, config_generation}` | pushed when an accepted config write changes an edge key or any `PairingPosture` field; the gateway re-applies it (rebind rules in §11.2 step 4) |
| `webhook/deliver` | `Channels:Execute` + allowlist | §12.3 | spawned, capped per connection |
| `ingress/webhook` | `Channels:Execute` + allowlist; the effect is authorized against the evidence principal | §12.5 | spawned, capped per connection |
| `ingress/sop` | `Channels:Execute` + allowlist; effect authorized as above, plus `Sops:Execute` and, per #11220, `Tools:Execute` | §12.5 | spawned, capped per connection |
| `ingress/cancel` | `Channels:Execute` + allowlist | `{delivery_id}` → `{cancelled}` | cancels one in-flight `webhook/deliver`, `ingress/webhook` or `ingress/sop` **on the calling connection** (§12.3, §12.5); an unknown or finished `delivery_id` is a no-op answering `{cancelled: false}`. |
| `webhook/routes` | `Channels:Read` or allowlist | `{}` → `{generation, routes: [{path, plugin, channel_alias}]}` | optional diagnostics, sorted by `path`; never used for dispatch. `generation` is the route generation of this daemon generation's registry and restarts after a reload, so it is compared only within one connection (the rows #11320 implements, §12.3) |
| `health` | `System:Read` + allowlist | master method (§8.1) | public `/health`: gateway liveness, this result, and `PairingPosture` |
| `metrics/scrape` | `System:Read` + allowlist | `[PR #11182]` method (§8.1) | public `/metrics` |
| `a2a/identity` | `System:Read` + allowlist | `{agent?}` → A2A card | public A2A card routes. On the service connection the handler serves **only** what A2A publication makes public: `[a2a.server] enabled` (else `-32600`, mapped to today's 404) and, with `agent`, only a published agent (else `-32602`, 404), which #11182's handler already enforces (`rt/rpc/catalog.rs:395-418` @b0ad81bfd2). The agent-selector check #11182 applies to a named agent (`rt/rpc/dispatch.rs:1987-1996` @b0ad81bfd2) is replaced, for the service principal only, by that publication rule: a published card is public by definition, and the service profile has no agent entitlements |
| `config/schema` | `Config:Read` + allowlist, **without `path` only** | §8.7 | public `OPTIONS /api/config`; the `path` form runs on the caller's own connection (D13) |
| `pairing/redeem` | allowlist | §8.8 | public `/pair`, `/api/pair` |

**`PairingPosture`** (DEC-26) is a non-secret projection of the core's pairing authority, never a second source of policy:

```jsonc
{
  "require_pairing": true,        // effective for this daemon generation: the live PairingGuard's require_pairing()
  "paired": false,                // at least one paired token exists now (PairingGuard::is_paired)
  "pairing_generation": 7,        // increments each time the core builds a pairing authority (once per daemon generation)
  "require_pairing_on_reload": false  // present only when the persisted gateway.require_pairing differs from the effective value
}
```

`require_pairing` is fixed per `PairingGuard` instance, which is per daemon generation (`cfg/pairing.rs:359`, Appendix B), and a reload retires every connection (§10.2), so a gateway always learns a new effective value by re-registering after it reconnects; no notification is needed for it. `paired` changes live (first pairing, last revocation) and `require_pairing_on_reload` changes on a config write; both are pushed with `gateway/settings`. The gateway uses the posture only for presentation: `/health`'s `require_pairing` and `paired` fields (`gw/lib.rs:2597-2632`), `/pair/code`'s answer (`gw/lib.rs:5068-5085`), whether to show the pairing screen, and whether to attempt a D2 anonymous connection. It never authorizes anything from it: the core decides every request (AUTH-1), and a D2 anonymous `initialize` is refused by the core when pairing is effective, whatever the gateway believed. Tests: CT-GW-01 (startup with pairing on and off: `gateway/register` reports it), CT-GW-02 (`config/set gateway.require_pairing` → `gateway/settings` carries `require_pairing_on_reload`, the effective value unchanged), CT-GW-03 (after the reload the gateway reconnects and `gateway/register` reports the new effective value; `paired` flips on the first pairing and after `pairing/revoke-all`).

### 8.14 Gateway surfaces with no core path yet

| Surface today | Where | Proposed disposition | Status |
|---|---|---|---|
| `/acp` WebSocket: a gateway-hosted `AcpServer` with its own dispatcher, no principal, sessions stored with owner `None`, restore without owner scope | `gw/acp.rs:31-110`; `ch/orchestrator/acp_server.rs:127-169`, `:540-637`, `:1003-1008`, `:1158-1171` | **OPEN D10**: keep on the in-process gateway only in v0.9.0 (`zeroclaw-gw` answers 503 with a message naming the surface), or core-hosted ACP with per-operation authorization, and dedicated upgraded connections. A byte relay plus an injected trait does not by itself authorize ACP operations | not on RPC |
| `/ws/nodes` | `gw/nodes.rs:213-279` | D10: core-verified node admission, or defer; `NodeTool` is not constructed outside its tests (`gw/node_tool.rs:185-280`) | not on RPC |
| WebAuthn `/api/webauthn/*` (feature `webauthn`, not default) | `gw/api_webauthn.rs` | D10: defer with a release note; success grants nothing today (`:230-234`) | not on RPC |
| `/a2a/{alias}` and agent cards | `gw/a2a.rs:293-668` | turn via `session/run-once` (#11132) on the caller's credential-bound connection; cards via `a2a/identity` (#11182) | partial, PRs |
| OIDC enrollment relays `/api/oidc/*`, `/oidc/login/{alias}`, `/oidc/callback` (they read `[oidc.<alias>]`, client secret included, from the full config: `gw/api_oidc.rs:357-375`, `:390`) | `gw/api_oidc.rs:227-260` | §8.14.1 | not on RPC |
| `/metrics` (unauthenticated today) | `gw/lib.rs:2655-2678` | `metrics/scrape` (#11182) over the service connection; the allowlist includes it | PR |
| `/health` (public; exposes `paired`, `require_pairing`) | `gw/lib.rs:2597-2632` | gateway liveness plus core `health` over the service connection | partial |
| Tunnel (the gateway is the only production caller of `create_tunnel`; decrypted tunnel secrets in the gateway's `Config`) | `gw/lib.rs:1693-1725`; `cfg/schema.rs:15668-15762` | the core runs it, upstream from operator config or the launcher's bound endpoint, never from `gateway/register`; adapters that ignore `local_host` (Cloudflare, ngrok, Tailscale) need fixing for two-container layouts | not on RPC |
| Static dashboard assets, `/api/openapi.json`, `/api/docs` | gateway | gateway-local; no core call | n/a |

#### 8.14.1 OIDC bootstrap across the boundary **OPEN D10**

The relays are unauthenticated by design (they are how a user without a token gets one) and today build their enrollment client from the whole `[oidc.<alias>]` entry, including `client_secret` (`#[secret]`, `encrypted_secret`; `cfg/schema.rs:13896-13929`). After the split the gateway reads no config and holds no confidential-client material, so either the core performs these flows or the separate gateway does not offer them.

What the core serves (non-secret) and what stays in the core, if the flows are kept `[proposed]`:

| Method | Authz | Params → Result | Holds |
|---|---|---|---|
| `oidc/providers` | allowlist (service) or any principal | `{}` → `{providers: [{alias, provider: "oidc.<alias>", issuer, client_id, flows: ["device", "pkce"]}]}` | nothing secret: no `client_secret`, no `validation` internals, no profile maps |
| `oidc/device/start` | allowlist | `{alias, client_key}` → `{flow_id, user_code, verification_uri, verification_uri_complete?, expires_in, interval}` | device code and client secret stay in the core, keyed by `flow_id` |
| `oidc/device/poll` | allowlist | `{flow_id}` → `{status: "pending"\|"slow_down"\|"expired"\|"denied"}` or `{status: "complete", access_token, expires_in}` | token exchange with the client secret happens in the core |
| `oidc/pkce/start` | allowlist | `{alias, redirect_uri, client_key}` → `{flow_id, authorization_url}` | PKCE verifier and state in the core |
| `oidc/pkce/complete` | allowlist | `{flow_id, code, state}` → `{access_token, expires_in}` | code exchange in the core |

The returned access token goes to the browser that ran the flow and is not retained by the gateway (HAND-3 extends to it). The core applies its own enrollment budget, since the forwarded `client_key` is not an identity; today's per-client and attempt limits (`gw/api_oidc.rs:58-107`, `:291-337`) become an edge pre-filter. Options for v0.9.0: **D10-a (recommended)**: the separate gateway answers `503 {"error":"oidc_enrollment_unavailable"}` on these routes and the in-process gateway keeps them; **D10-b**: implement the five methods above for v0.9.0.

### 8.15 Notifications and core→client requests

| Name | Direction | Payload | Status |
|---|---|---|---|
| `session/update` | notification | `SessionUpdateEvent`, tagged by `type` (§9.1 table) | `[master]`; new fields in #11132/#11185 (`turn_complete` gains `usage?`, `safeguard_fallback?`; ring-delivered frames gain `subscription_id`, `seq`) |
| `logs/event`, `events/event` | notification | the bus frame plus `subscription_id`, `seq` | `[master]` |
| `subscription/lagged` | notification | `{subscription_id, from_seq, resume_seq, epoch_changed}` | `[master]` |
| `elicitation/create` | **request** core→client | `{sessionId, mode: "form"\|"url", message, requestedSchema}` (camelCase, MCP-aligned) → `{action: "accept", content}` \| `{action: "decline"}` \| `{action: "cancel"}` | `[master]` `api/elicitation.rs:63-92`; `rt/rpc/approval_channel.rs:236-243`; absent from the OpenRPC document (SCH-4) |
| `gateway/settings` | notification | §8.13 | `[proposed]` |
| `session/update` for an ingress-admitted turn | notification | the `SessionUpdateEvent` plus `delivery_id` (§12.5), sent only to the connection that made the ingress call | `[proposed]` |
| `sops/run-changed` | notification | `SopRunChanged` {subscription_id, seq, run: SopRunSummary} | `[PR #11377]` (`sops/subscribe-runs`, §8.9; C.6) |
| `canvas/frame` | notification | `{subscription_id, seq, canvas_id, frame: CanvasFrame}` | `[proposed]` (`canvas/subscribe`, §8.12) |
| `session/update` on a session ring | notification | same payload plus `subscription_id`, `seq`; gaps via `subscription/lagged` | `[PR #11185]` (`session/attach`) |

### 8.16 Error model

| Code | Name | Meaning for a gateway | HTTP mapping the gateway MUST use |
|---|---|---|---|
| -32700 | PARSE_ERROR | client bug | 500 (never the caller's fault) |
| -32600 | INVALID_REQUEST | client bug; with `data.reason` `frame_too_large` or `frame_timeout` the connection is ending | 500 / reconnect |
| -32601 | METHOD_NOT_FOUND | core lacks the method (skew) | 503 `core_capability_missing` (V-4) |
| -32602 | INVALID_PARAMS | caller input rejected by the core | 400 |
| -32603 | INTERNAL_ERROR | core failure | 500 |
| -32000 | SESSION_NOT_FOUND | | 404 |
| -32001 | SESSION_LIMIT_REACHED | | 429 |
| -32002 | SESSION_BUSY | | 409 |
| -32003 | SESSION_NOT_OWNED | | 403 |
| -32004 | CONNECTION_LIMIT_REACHED | `data: {reason: "connection_limit", limit}`, then EOF | 503, back off |
| -32005 | PRECONDITION_FAILED `[PR #11176, #11384]` (`personality/put` with a stale `expected_mtime_ms`; `data.error: "personality_disk_drift"`; #11384 takes #11176's guard over unchanged) | the caller's copy is stale | 409 (the HTTP twin answers 409) |
| -32010 | AUTH_REQUIRED | credential missing, rejected, expired, revoked, or re-verification due | 401 |
| -32011 | VERSION_MISMATCH | protocol disjoint | 503 with the V-4 diagnostic |
| -32012 | FORBIDDEN | principal lacks the grant or selector | 403 |
| -32020 / -32021 | SOP_ALREADY_EXISTS / SOP_NOT_FOUND | | 409 / 404 |
| 4001 / 4002 / 4003 | FS_NOT_FOUND / FS_PERMISSION_DENIED / FS_INVALID_PATH (numeric codes, no `data`; the `fs.*` string constants and `FsStatError` are defined but unused, `api/jsonrpc.rs:280-288`, `:602-608`) | | 404 / 403 / 400 |
| -32050 | client-side INBOUND_REQUEST_REJECTED (#11186) | the client refused a core→client request it could not queue | never crosses to HTTP |

Messages are localized (auth denials come from i18n keys, `rt/rpc/auth.rs:106-137`); clients MUST NOT parse `message`.

`data.reason` values this contract introduces, with the HTTP status a gateway maps them to: `pairing_required` (`-32010`, 401), `binding_mode_change` (`-32600`, 500), `rate_limited` (`-32010`, 429), `path_not_found` (`-32602`, 404), `native_device_token_required` (`-32012`, 403), `device_registry_disabled` (`-32603`, 503), `device_not_found` (`-32602`, 404), `sop_disabled` (`-32603`, 503), `canvas_capacity` (`-32600`, 429), `duplicate_delivery_id` (`-32602`, 500: a gateway bug), `ingress_capacity` (`-32600`, 429: a generic ingress call past the per-connection cap; plugin deliveries answer the `queue_full` outcome instead, §12.3), `credential_rejected` (`-32010`, 401), `sop_credentials_required` (`-32012`, 403), `sop_dispatch_unavailable` and `needs_quickstart` (`-32603`, 503 with today's bodies), `pairing_code_invalid` (`-32010`, 401), `invalid_json` (`-32602`, 400 `{"error":"invalid_json"}`), `pairing_disabled` (`-32600`, 404), `already_registered` (`-32600`, 500: a gateway bug); Appendix D adds the V-5 refusal reasons. A gateway maps a listed `data.reason` before it falls back to the code's row above.

**Error `data` vocabulary (DEC-17, proposed).** On master a handler's `data` never reaches the wire (`process_line` sends only code and message, `rt/rpc/dispatch.rs:3118-3121`); only the listener's transport refusals carry `data`, keyed `reason` (`rt/rpc/local.rs:112-140`). Two open PRs now put handler `data` on the wire with different keys: #11172 sends `ConfigApiError {code, message, path?, op_index?}` and #11176 sends `{error: "personality_disk_drift", …}`, each through its own copy of `send_rpc_error` (the two copies collide when both merge; see §15.1). #11182 goes the other way: `channels/relink` puts its HTTP twin's failure body in `data` (`crates/zeroclaw-channels/src/control.rs:568` @b0ad81bfd2), but that head's dispatcher still sends every handler error through `send_error` with only code and message (`rt/rpc/dispatch.rs:4037`, `:10734` @b0ad81bfd2), so the body never reaches a client; its in-process test checks `err.data` before the wire. #11377 makes `process_line` itself send a handler error as it stands, `data` included (`rt/rpc/dispatch.rs:2871`, helper `:9616-9626` @8b2ed4ef5c), keyed `reason` (`sop_disabled`); it is a third copy of the same helper. The contract fixes one rule: every ZeroClaw error MAY carry `data`; when it does, `data.reason` is the stable snake_case machine code (master's precedent), and method-specific members sit beside it. #11172's `code` and #11176's `error` should be renamed to `reason` before they merge, or carry `reason` as well during a transition. `[proposed]` every ZeroClaw error carries a stable `data.reason` (for example `auth_required`, `credential_expired`, `revalidation_due`, `pairing_revoked`, `not_entitled`, `session_not_owned`), additive under V-1. #11186's client currently drops the null-id refusal frames the listener sends before closing (`-32004`, `frame_too_large`, `frame_timeout`), so callers see only "disconnected"; the client MUST surface them (CT-ERR-02).

## 9. Streaming, turn lifetime and cancellation

### 9.1 Turn events `[master]`

A turn started by `session/prompt` emits `session/update` notifications **to the connection that sent the prompt** (`forward_turn_event` writes to that connection's outbound handle, `rt/rpc/dispatch.rs:11572-11581`). Event types, wire-exact (`rt/rpc/types.rs:1596-1700`):

| `type` | Fields |
|---|---|
| `agent_message_chunk` | `session_id`, `text` |
| `agent_thought_chunk` | `session_id`, `text` |
| `tool_call` | `session_id`, `tool_call_id`, `name`, `raw_input` |
| `tool_result` | `session_id`, `tool_call_id`, `name`, `raw_output` |
| `approval_request` | `session_id`, `request_id`, `tool_name`, `arguments_summary`, `timeout_secs` |
| `context_usage` | `session_id`, `input_tokens?`, `max_context_tokens?`, `model_context_window?` |
| `plan` | `session_id`, `entries` |
| `turn_started` `[proposed]` | `session_id`, `turn_id`, `client_turn_generation?`: the first event of every turn (DEC-22) |
| `turn_complete` | `session_id`, `outcome` (`completed`\|`cancelled`\|`failed`), `content`, `client_turn_generation?`, `message_count?`, `turn_id?` `[proposed]` |
| `history_trimmed` | `session_id`, `dropped_messages`, `dropped_turns`, `kept_turns`, `reason`, `token_budget?`, `tokens_before?`, `tokens_after?`, `tokens_before_source?`, `tokens_after_source?`, `unsatisfiable_floor?` |

**Turn identity `[proposed]` (DEC-22).** Master has no event that names a turn: `client_turn_generation` is chosen by each client (zerocode counts from 1, `zc/chat.rs:3505`), so two clients can use the same value, and `session/state.turn_id` is only readable by polling (`rt/rpc/dispatch.rs:7071-7130`). The core assigns every turn a `turn_id` that is a random UUID (version 4 or 7), never a counter, so an id from one daemon incarnation can never name a turn of another; it announces it in `turn_started` before any other event of that turn, repeats it on `turn_complete`, and returns the same value from `session/state` while the turn runs. Today `session/state.turn_id` is the live session's turn-generation counter rendered as a string (`rt/rpc/dispatch.rs:7094`); it becomes that UUID, which clients already treat as opaque. `initialize` also reports `daemon_instance_id` `[proposed]`, a random UUID fixed for the life of the daemon process, so a client can tell a daemon restart from a reconnect to the same process (§10.2). `turn_started` is a new event type, which clients that follow V-3 ignore, and `turn_complete.turn_id` is an added field, so both are additive under V-1.

Open PRs change this surface additively (V-1 holds; no new `SessionUpdateEvent` variant in any PR):

- `turn_complete` gains optional `usage` (`TurnUsageTotals`: token and cost totals, `usage_by_provider`) and `safeguard_fallback` (`{fallback_kind, requested_model, served_model}`) `[PR #11132; older copy in #11185]`.
- A new cancel cause `operator_abort` appears in a cancelled `turn_complete.content` `[PR #11132]`.
- On a session-lifetime connection (`clientCapabilities.turn_lifetime: "session"`, result `turn_lifetime`) the turn's frames go to a per-session ring and reach viewers with `subscription_id` and `seq` added; gaps use the existing `subscription/lagged` `[PR #11185]`.
- `session/append` publishes a bus event `{"type":"message", …, "source":"rpc"}` that reaches `logs/subscribe` `[PR #11132]`.

### 9.2 What ends a turn today `[master]`

| Event | Effect | Cite |
|---|---|---|
| The prompting connection closes (EOF, write stall, listener cancel) | the turn is cancelled with cause `ConnectionClosed`, given 5 s to unwind, then aborted | `rt/rpc/dispatch.rs:6081-6092`; `rt/rpc/session.rs:49-52`; `rt/rpc/turn.rs:149-160`, `:287` |
| `session/cancel` | cancels the active turn **only if the caller's `tui_id` owns the session**, else `-32003` | `dispatch.rs:6652-6715` |
| `session/kill`, `session/delete`, `session/close` | cancel then remove (kill/delete signal cancellation before storage work) | `rpc-socket.md` "ACP durable lifecycle"; cause `AdminKill` / `SessionRemoved` (`session.rs:39-53`) |
| Daemon reload or shutdown | every RPC connection is retired and drained (cancel + join, 5.5 s budget), so every RPC-started turn is cancelled | `rt/daemon/mod.rs:1183-1196`; `rt/rpc/local.rs:628-641` |
| Credential revoked / grants narrowed mid-turn | the running turn is not interrupted by the per-operation gate; the next privileged call is refused (`dispatch.rs:1087-1182`). Queued work is rechecked on admission (#11234 is open to extend this to queued session ownership) | |

Consequences for a separate gateway on master: restarting or crashing the gateway cancels every turn started through it; a browser tab reconnecting through a different pooled connection cannot `session/cancel` the turn it started (different `tui_id`); and a daemon reload drops every gateway connection.

### 9.3 Required turn-lifetime contract `[proposed]` (#11001 AC3; builds on #11185)

**TL-1 Session-owned turns.** A turn belongs to its session, not to the connection that submitted it. Closing any connection, including every gateway connection at once, MUST NOT cancel a turn *for a client that opted into session lifetime* (`clientCapabilities.turn_lifetime: "session"`, or #11185's equivalent). A client that does not opt in keeps today's connection-lifetime behaviour for turns it starts (V-2), so zerocode 0.8.x semantics do not change silently.

**TL-2 Viewers.** Any connection whose principal may read a session MAY attach as a viewer and receive that session's live `session/update` events, with replay from a sequence number. Producers never wait for a viewer; a slow viewer lags and is told so. Attach re-checks ownership against current grants at attach and at every delivered event (the same rule `deliver_subscription` applies, `dispatch.rs:10849-10964`).

**TL-3 Cancellation is explicit, principal-scoped and targeted.** `session/cancel` is authorized by session ownership (the principal), not by the `tui_id` that happened to submit the turn. An operator path (`session/abort`, `Sessions:Delete`, #11132) cancels a turn another client started; it is authorized like other session mutations (the owner, or the admin bypass) and recorded with cause `operator_abort`. Both act on whatever turn is current and are retry class `N`. Targeted cancellation uses two new methods (DEC-22), `session/cancel-turn` and `session/abort-turn`, which take `{session_id, turn_id}`, cancel only if the session's running turn has that id, and otherwise answer `{session_id, turn_id, cancelled: false}` (not an error), so a replay can never stop a later turn. They are distinct methods, not a new parameter on the old ones, because a protocol-1 handler that predates them parses `session/cancel`'s params permissively and would cancel the current turn while ignoring the target (`rt/rpc/dispatch.rs:6653`, `:6701`); a method name is what `initialize`'s `capabilities` can advertise, so support is proven before use, and an older core answers `-32601` instead of cancelling the wrong turn. A client that may retry (every gateway) MUST use the targeted methods when `capabilities` lists them and MUST NOT retry the untargeted ones; it MUST NOT send `turn_id` to `session/cancel` or `session/abort`. #11185's incarnation guard on abort (`abort_session_generation`, `rt/rpc/session.rs:1800-1805` @6713ff419b) protects a lookup race inside one call; it does not distinguish two turns of one incarnation, which is what `turn_id` adds.

**TL-4 Lifecycle matrix.** Each event below has exactly one defined outcome. What #11185 at 6713ff419b already does: closing the prompting connection does not cancel a session-owned turn; the turn is counted in the generation drain and cancelled at retirement with cause `daemon_retired`; the owner principal may `session/cancel` from any client; `session/close`/`kill`/`delete` end the session's viewers (`dispatch.rs:3035-3061`, `:6311-6319`, `:6953-6963`, `:4825`, `:5012`, `:7957` @6713ff419b).

| Event | Required outcome (TL) |
|---|---|
| viewer detaches (EOF) | nothing happens to the turn; the viewer set shrinks |
| last viewer detaches | nothing happens; pending approvals follow **OPEN D4** |
| explicit `session/cancel` by the owner principal | turn cancelled, `turn_complete{outcome:"cancelled"}` to all viewers |
| operator `session/abort` | same, audited with the operator principal |
| `session/close` / `kill` / `delete` | cancel first, then the durable effect already documented |
| owner's grant revoked mid-turn | the turn continues to completion or its next privileged step; viewers bound to the revoked principal stop receiving events at the next delivery check; queued prompts are refused at admission |
| daemon reload | every in-flight turn is cancelled with a recorded cause before the generation retires; the drain proof at `rt/daemon/mod.rs:1183-1196` must keep counting session-owned turns, not only connections (#11185 does) |
| daemon shutdown | same as reload |
| `--ephemeral` daemon | "last client disconnected" counts connections, as today (`rt/daemon/mod.rs:287-330`); a detached session-owned turn keeps the daemon alive until it ends (**[proposed]** new rule; on master the question does not arise because a disconnect cancels RPC-started turns) |

**TL-5 Retry.** Test CT-TL-06, three legs: (a) start turn A, send `session/cancel-turn` with A's `turn_id` and drop the response, start turn B in the same session, replay the request → `cancelled: false` and B keeps running; (b) against a core whose `capabilities` lacks `session/cancel-turn` (a protocol-1 build from before DEC-22), the client sends no targeted cancel and never retries `session/cancel` (checked by a unit test on the pool's retry table); (c) restart the core between the lost response and the replay → the reconnect sees a new `daemon_instance_id` and drops the pending retry (§10.2), and a replay forced past that rule still answers `cancelled: false` because A's UUID names no turn of the new daemon. `session/prompt` is not idempotent. A client that loses the connection after sending it MUST NOT resend automatically; it reconnects, attaches, and reads `session/state` (whose `turn_id` and `state` say whether a turn is running, `rt/rpc/types.rs:421-434`) and `session/messages` to learn whether the prompt was admitted. `client_turn_generation` (`types.rs:277-280`) lets the client recognize its own turn's `turn_complete`.

### 9.4 Approvals and elicitation **OPEN D4**

Today an approval is bound to the connection: `RpcApprovalChannel` holds that connection's outbound handle, emits `approval_request` on it and sends `elicitation/create` as a core→client request on it (`rt/rpc/approval_channel.rs:28-34`, `:174-198`, `:236-243`); `session/approve` answers by `request_id` (`rt/rpc/types.rs:1404-1420`). The in-process gateway WebSocket denies every parked approval immediately when its viewer leaves (#10538's chosen semantics).

PR #11185 at 6713ff419b already implements D4-a for session-owned turns: an `approval_request` published with no viewer attached is denied at once as `Unreachable`, a pending one is denied when the last viewer detaches, and `elicitation/create` choice prompts are declined (`crates/zeroclaw-runtime/src/rpc/approval_channel.rs:181`, `:199`, `:236-266` @6713ff419b). Accepting D4-b would change that PR before it merges.

A separate gateway makes gateway restarts routine, so the policy for an approval raised while no viewer is attached is a product decision:

| Option | Rule |
|---|---|
| D4-a | deny immediately (today's #10538 semantics); every gateway restart denies pending approvals |
| D4-b (recommended) | park until the approval's existing deadline; replay the pending request to any authorized viewer that attaches; deny at expiry with cause `timeout`; the deadline is never reset by reattach; an explicit user cancel ends it immediately |
| D4-c | split by cause (gateway absent vs viewer left). Not recommended: socket EOF order during a crash is not evidence of intent |

Whatever is chosen, a pending approval becomes **session-owned control state** with a stable `request_id`, an absolute deadline, exactly one accepted answer, and an authorized-responder rule (a read-only viewer is not an approver: answering requires `Sessions:Update` on that session, as `session/approve` does today).

## 10. Subscriptions and reconnect

### 10.1 Subscriptions `[master]`

`logs/subscribe` and `events/subscribe` (#11167) open a subscription on the calling connection and return `{subscribed, subscription_id, seq, epoch}` (`rt/rpc/types.rs:1445-1455`). Frames arrive as `logs/event` / `events/event` with `subscription_id` and `seq` injected into params (`dispatch.rs:10933-10941`). A gap yields `subscription/lagged {subscription_id, from_seq, resume_seq, epoch_changed}` (`types.rs:1471-1485`); resume with `{since_seq, epoch}` (`types.rs:1426-1443`); `since_seq` from another epoch replays what the new hub holds after an `epoch_changed: true` lag; `since_seq` ahead of the head in the same epoch is `-32602` (`dispatch.rs:9365-9377`). `subscription/cancel {subscription_id}` → `{cancelled}`.

Rules the gateway depends on:

- **Global streams are unscoped-only.** `logs/*`, `events/*` require the caller to be an administrator or the shared operator; a scoped principal is refused (`require_global_stream_access`, `dispatch.rs:9322-9337`; reason at `:10966-10975`). The gateway therefore cannot offer the global event stream to a scoped OIDC user; per-session streams (TL-2) are the scoped path.
- **Every delivery is re-authorized.** The delivery task checks credential liveness and, when the policy generation moved, the method's grant before each frame; the first refusal ends the stream (`dispatch.rs:10849-10964`).
- **Ring bounds.** 2,048 frames or 4 MiB per source; 16 MiB across sources; oldest evicted and reported as a lag (`rt/rpc/subscription.rs:49-56`).
- **Subscriptions die with their connection** (tokens are children of the connection token, `dispatch.rs:9380-9383`) and with the hub: the hub is rebuilt per daemon generation (`rt/daemon/mod.rs:918-922`), so a reload changes the epoch.
- **Ending a subscription is connection-scoped cleanup (DEC-31, `[proposed]`).** Today `subscription/cancel` is classified `(Logs, Read)` (`dispatch.rs:441`), although its handler only removes an entry from the calling connection's own map (`dispatch.rs:9299-9308`); #11185 keeps both (`dispatch.rs:463-468`, `:9994-10002` @6713ff419b). A caller that opened a subscription under another grant, a `Sessions:Read` viewer (`session/attach`), a `Sops:Read` holder (`sops/subscribe-runs`) or a `Canvas:Read` holder (`canvas/subscribe`), therefore cannot end it without also holding `Logs:Read`, and closing the connection instead would end its other subscriptions and in-flight calls. The contract gives `subscription/cancel` its own `Method::authz()` classification, *own connection*: the gate requires a bound connection whose credential is still live (the per-operation liveness check) and no `(Resource, Verb)` grant; the handler acts only on subscriptions the calling connection owns. An id owned by another connection, or unknown, answers `{cancelled: false}` and reveals nothing. This widens who may call the method only to the owner of the subscription, under this rule (V-6), and is registered in Appendix D. #11377 implements `sops/subscribe-runs` without DEC-31: its gateway cancels on close, and for a holder of `Sops:Read` without `Logs:Read` the refused cancel takes the connection out of the gateway's pool, so the subscription ends with the connection. Tests CT-SUB-01 (a `Sessions:Read`, a `Sops:Read` and a `Canvas:Read` principal, none holding `Logs:Read`, each cancel their own subscription on a connection that also carries another subscription, which keeps streaming) and CT-SUB-02 (cancelling another connection's `subscription_id` answers `cancelled: false` and that subscription keeps streaming).

### 10.2 Reconnect procedure (normative for clients) `[proposed]`

After any connection loss a client MUST, in order:

1. Re-resolve the endpoint (§4) and re-verify it (§5.5). The core may have restarted on a new pid.
2. `initialize` with the same credential (ID-3). Treat `-32010`/`-32012` as final for that credential; do not retry with another.
3. Compare `daemon_instance_id` (or, from a core that does not report it, `server_pid` and `server_version`) with the previous connection. A different value means a daemon restart: every in-memory assumption is void, and the client drops every pending retry that names a transient id (`turn_id`, `subscription_id`, `upload_id`, `delivery_id`), including retries of class `W`; it reads state instead (DEC-22).
4. Re-open subscriptions with the last `{since_seq, epoch}`; handle `epoch_changed`.
5. Re-attach to sessions it was viewing (TL-2) and read `session/state` before assuming a turn is still running.
6. Never replay a request of retry class `N` whose response was lost (§8.0): that includes every method that targets a session only by `session_id`, because the id can be re-created, and untargeted `session/cancel`/`session/abort`. Replay `W` requests only as sent, with the same target ids.

Backoff: jittered exponential from 100 ms to 10 s while the endpoint is missing or refuses; the gateway serves `503 core_unavailable` in the meantime (§11.2). What survives what:

| State | Connection loss | Daemon reload | Daemon restart |
|---|---|---|---|
| Subscriptions | lost (re-open with `since_seq`) | lost; new epoch | lost; new epoch |
| In-memory session owner | kept (`rt/rpc/local.rs:612-618`) | rebuilt from durable storage (`SessionStore` is per generation, `rt/daemon/mod.rs:776-777`) | same as reload |
| Running turn (master) | cancelled | cancelled | killed |
| Running turn (TL-1 opt-in) | kept | cancelled (TL-4) | killed |
| Chunked uploads | discarded (connection-owned) | discarded | discarded |
| Pending core→client requests | fail on the core side | fail | fail |
| Webhook idempotency (§12.6) | kept (core-owned) | kept if held at process scope | lost |

## 11. Process lifecycle

### 11.1 Core `[master]`

- The local listener is registered unconditionally by `zeroclaw daemon` (`root/src/main.rs:7312-7322`) and supervised with backoff (`rt/daemon/mod.rs:942-976`). Readiness is reported when the socket is bound and secured (`rt/rpc/local.rs:470-479`); the daemon waits for it before reporting started (`rt/daemon/mod.rs:1132-1142`).
- The in-process gateway is registered whenever the `gateway` feature is compiled (`root/src/main.rs:7206-7207`) and receives the daemon's pairing guard, inbound-auth state, live config, event bus, TUI registry and reload controls in-process (`rt/daemon/mod.rs:642-701`; the plugin webhook registry through the starter closure, `root/src/main.rs:7207-7214`). There is no switch to run the daemon without it (`GatewayConfig` has no `enabled`/`mode` field, `cfg/schema.rs:7772-7911`).
- A reload (`config/reload`, `dispatch.rs:8208-8235`, or the gateway's `/admin/reload`) shuts the in-process gateway, then retires **every RPC connection** before the next generation starts (`rt/daemon/mod.rs:1183-1196`). For a separate gateway this is a connection loss (§10.2).
- `--ephemeral` exits 1 s after the last counted connection closes (`rt/daemon/mod.rs:287-330`). Local and WSS connections are counted; the relay and enroll starters ignore the counter they are handed (`root/src/main.rs:7462`, `:7562`).

### 11.2 Gateway startup: bind first `[proposed]` (G3)

1. Read bind settings from the launcher (flags/env) or a core-generated, non-secret bootstrap projection (§13.3; in the separate-account layout at `<ipc_dir>/gateway/bootstrap.json`, §5.5.1). Never from `config.toml` in hardened mode.
2. Bind the HTTP listener immediately; serve `503 {"error":"core_unavailable"}` on every domain route and a static "starting" page until step 4.
3. Resolve and verify the core endpoint (§4, §5.5); `initialize` the service connection (D1).
4. Fetch effective settings (`gateway/register`, §8.13); if they differ from the bootstrap in a way that needs a rebind (host, port, TLS, `path_prefix`), rebind once and close the old listener after in-flight requests finish; then serve.
5. Print one machine-readable readiness line on stdout, `READY <scheme>://<addr><path_prefix>`, for the launcher (child mode, desktop). The launcher MUST verify it by an authenticated probe through its own RPC connection (`gateway/status`), never by trusting the line or an unauthenticated `/health`.

### 11.3 Supervision modes **OPEN D5**

| Mode | Who starts `zeroclaw-gw` | Use |
|---|---|---|
| `child` | the core spawns it (endpoint in `ZEROCLAW_SOCKET`, core pid for §5.5), restarts it with backoff, stops it on shutdown | default for existing single-unit installs (systemd, launchd, Docker, Homebrew) so they keep serving the dashboard |
| `external` | an outside supervisor (second systemd unit, Nix, compose, desktop app); the core never spawns it | hardened and desktop layouts |
| `disabled` | nobody | headless |

Adding `gateway.mode` (or an equivalent) is **additive** config ; its name and default are a maintainer decision (D5). The desktop app owns both processes in `external` mode (#11004 AC4 per-process ownership).

### 11.4 Restart isolation `[proposed]` (G3 acceptance)

- Killing the gateway (SIGKILL) MUST NOT cancel a turn started through it once TL-1 is in force (test CT-LIFE-01: start a turn through the gateway, kill the gateway, assert `turn_complete{outcome:"completed"}` reaches a viewer that re-attaches, and no `ConnectionClosed` cause).
- The gateway MUST reserve its connection budget: at most `pool cap + 2` (service + ingress) connections, leaving `rpc.max_local_connections` headroom for zerocode and recovery clients.
- Long operations (`doctor/run`, `cron/trigger`, config writes waiting on the config lock) MUST NOT run on the service or ingress connection (§3.4 serial dispatch).
- The core reports whether ingress is reachable (§12.8): while no gateway is registered, `health` shows `ingress: gateway_absent` for every webhook-driven channel (#11002 AC4).

## 12. Webhooks and ingress across the boundary

### 12.1 Today `[master]`

| Route | Verification | Where it runs | Idempotency | Cite |
|---|---|---|---|---|
| `GET/POST /plugin/{path}` (WASM channel plugins; mounted only with `plugins-wasm`) | per-client rate limit in the gateway; vendor authentication and challenge replies **in the plugin guest** | gateway looks up the route in the in-process `PluginWebhookRegistry` and `try_send`s a `RawWebhook` to the channel worker in the core; 10 s reply deadline | gateway-owned reservation store, called by the worker mid-delivery | `gw/plugin_webhook.rs:18-201`; `gw/lib.rs:2271-2274`; `api/webhook.rs:145-268`; `crates/zeroclaw-plugins/src/wasm_channel.rs:614-719` |
| `POST /webhook` (generic) | gateway: rate limit, then pairing bearer if `require_pairing`, then `X-Webhook-Secret` if `gateway.webhook_secret` is set; one config snapshot per request | gateway: SOP-first dispatch, then `?agent=` validation, then an agent turn run **inside the gateway** | gateway `record_if_new` on `X-Idempotency-Key`, injective length-prefixed key | `gw/lib.rs:3107-3116`, `:3120-3268`, `:3284-3345`, `:3377-3560` |
| `POST /sop/{*rest}` | same truth table; SOP dispatch fails closed when no control is configured | gateway drives the daemon-shared SOP engine | gateway `record_if_new`, per-path namespace | `gw/lib.rs:3093-3095`, `:3355-3374`; `gw/api_sop_webhook.rs` |
| `/whatsapp[/{alias}]`, `/linq[/{alias}]`, `/nextcloud-talk[/{alias}]`, `/webhook/gmail` | vendor verification in the gateway, against channel instances **the gateway builds itself** with their secrets | agent turn **inside the gateway** (`webhook_ingress::dispatch_verified_webhook`) | per vendor | `gw/lib.rs:3068-3091`, `:1446`, `:1489`, `:1534`, `:1608`, `:4304`, `:4442`, `:4557` |

The standalone `run_gateway` passes an empty registry, so a gateway run outside the daemon answers 404 on every `/plugin/*` (`gw/lib.rs:965-973`).

### 12.2 Principles (normative)

- **ING-1 Routing state never leaves the core.** The core owns the plugin route table and its generations, the idempotency store, deadlines and the body re-check. The gateway is route-agnostic for `/plugin/*`: it forwards every request and maps the typed outcome. Registration does not cross the boundary, so there is no gateway route table to go stale.
- **ING-2 Headers and bodies are opaque evidence.** The gateway forwards the method, raw query, every header whose value is UTF-8 (names lowercased; today's rule, `gw/plugin_webhook.rs:112-120`) and the exact body bytes. It does **not** apply a header allowlist: vendor signatures use arbitrary headers, and Gmail's credential is in `Authorization` (`gw/lib.rs:4606-4615`). The core never treats a forwarded header as *its own* trust input (for example forwarded-for addresses) except the named evidence fields of §12.5, which it verifies itself.
- **ING-3 Vendor and route credentials are verified in the core** (the plugin guest, the owning channel, or the core's ingress handler), never in the gateway. The gateway holds no vendor secret and no `webhook_secret`.
- **ING-4 Bounded, non-blocking and correlated.** Ingress calls (`webhook/deliver`, `ingress/webhook`, `ingress/sop`) are spawned in the core, as `session/prompt` is (§3.4), under a per-connection cap, on a gateway connection reserved for ingress; they never share the serial request path of the gateway's control connection. Each call carries a gateway-chosen **`delivery_id`**, unique among the in-flight ingress calls on that connection (1..=64 bytes; a duplicate is refused with `-32602`, `data.reason: "duplicate_delivery_id"`). The `delivery_id`, not the JSON-RPC id, correlates everything the call produces besides its response: every stream event (§12.5), cancellation (`ingress/cancel`), and core log records. It carries no authority and is never an idempotency key. Its scope is the connection: an id means nothing on any other connection and dies with its own, so a reconnect starts a fresh scope; the gateway generates random 128-bit ids, which makes reuse within a scope practically impossible, and the core refuses one that is still in flight. DEC-23.

### 12.3 `webhook/deliver` `[proposed]` (#11003)

Direction client→core, request. Authz `(Channels, Execute)` under `Method::authz()` (the `Resource::Channels` and `Verb::Execute` variants exist, `api/grants.rs`); under D1-A the `gateway-ingress` profile holds it; the shared operator passes regardless.

```jsonc
// params
{
  "path": "fixture",            // one segment; the core re-validates the host grammar: 1..=64 bytes of the allowed set (rt/plugin_runtime.rs:510-515)
  "method": "POST",             // "GET" | "POST"; anything else → reject bad_request (parity: HEAD/others are 405 at the gateway, gw/plugin_webhook.rs:51-66)
  "query": "a=1&b=2",           // raw, no leading '?', never synthesized (api/webhook.rs:148-151)
  "headers": [{"name": "x-sig", "value": "…"}],   // ordered; lowercase names; UTF-8 values only
  "body_b64": "…",              // exact bytes; over 64 KiB (gw/lib.rs:177 MAX_BODY_SIZE) or not standard base64 → -32602, a gateway bug
  "deadline_ms": 10000,         // the core clamps to 10 000 (gw/plugin_webhook.rs:18)
  "delivery_id": "…"            // ING-4: gateway-chosen, unique among this connection's in-flight ingress calls
}
// result: exactly one of
{"outcome":"ack"}
{"outcome":"reply","body":"<≤ 4096 UTF-8 bytes>"}
{"outcome":"reject","reason":"not_found"|"queue_full"|"unavailable"|"unauthorized"|"bad_request"|"invalid_response"|"timeout"|"cancelled"}
```

| Result | HTTP at the gateway | Today's equivalent |
|---|---|---|
| `ack` | 200, empty body | `WebhookOutcome::Ack` (`gw/plugin_webhook.rs:147`) |
| `reply` | 200 `text/plain`, body as given | `WebhookOutcome::Body` (≤ 4 KiB, `api/webhook.rs:165`) |
| `not_found` | 404 | registry miss (`gw/plugin_webhook.rs:109-111`) |
| `queue_full` | 429 | `TrySendError::Full` (`:138-140`) |
| `unavailable` | 503 | `TrySendError::Closed`, `WebhookReject::Unavailable`, dropped reply (`:141-143`, `:184-196`, `:200`) |
| `unauthorized` | 401 | `WebhookReject::Unauthorized` (`:158-170`) |
| `bad_request` | 400 | `WebhookReject::BadRequest` (`:171-183`) |
| `invalid_response` | 502 | `WebhookReject::InvalidResponse`, oversize body (`:148-157`) |
| `timeout` | 504 | `WebhookReject::Timeout`, elapsed deadline (`:197-199`) |
| `cancelled` | 503 | new: the delivery was cancelled by `ingress/cancel` or by its connection's close; the gateway's own caller is usually gone by then |
| RPC transport error, connection lost, core down | 503 `webhook unavailable` | new: "core unavailable" |
| `-32012` on the call | 503, and a gateway ERROR log (a deployment error, not the caller's) | new |
| `-32602` on the call (an oversize or undecodable `body_b64`) | 500, and a gateway ERROR log: the gateway's body layer refuses 64 KiB first, so this is the gateway's bug | new; `[proposed]`: #11322's forwarder answers 503 today (Implementation status below) |

**Implementation status.** The plugin webhook series #11319, #11320 and #11322 (drafts, heads in §15.1) is the intended implementation of this method. It does not conform yet, and the method stays `[proposed]` until it does.

- **Contract choices taken from the series.** Each revises this section's earlier proposal, which no release shipped; they are revisions of a proposal, not additions to a published method:
  - headers are `{name, value}` objects, replacing ordered `[name, value]` pairs (`crates/zeroclaw-rpc-proto/src/types.rs:1661-1663` @7962184961);
  - `cancelled` joins the `reject` reasons (the series' outcome, `:1714`);
  - an oversize or undecodable `body_b64` is `-32602`, replacing the `payload_too_large` reason;
  - `webhook/routes` answers `{generation, routes: [{path, plugin, channel_alias}]}` under `Channels:Read`, replacing `{generation, paths}` under `System:Read` (`:1749-1768`; `rt/rpc/dispatch.rs:12002-12022` @7962184961).
- **Remaining wire differences in the series.** The shape above stays the target; each is work the series does before it conforms:
  - **names:** `plugin-webhook/dispatch`, `plugin-webhook/cancel`, `plugin-webhook/routes` and `request_id` stand for `webhook/deliver`, `ingress/cancel`, `webhook/routes` and `delivery_id`;
  - **result shape:** the series answers a flat outcome (`{"outcome":"cancelled"}`, `{"outcome":"not_found"}`, …) where this section has `{"outcome":"reject","reason":…}` (`crates/zeroclaw-rpc-proto/src/types.rs:1692-1726` @7962184961). Renaming does not reconcile the two; the series normalizes its result to the `reject`/`reason` union;
  - **`-32602` at the gateway:** #11322's forwarder treats it as an unexpected core error, logs ERROR and answers the shared 503 (`crates/zeroclaw-gateway/src/plugin_webhook/forward.rs:236-249`, `:147-160` @d15729ebff), where the table above answers 500. It also logs a `-32010`/`-32012` refusal at WARN, where the table asks for ERROR (`:120-132`). Owner: #11322, with a forwarder change and a test;
  - **deadline:** the request has no `deadline_ms` (`types.rs:1670-1685`); the core waits a fixed 10 s counted from enqueue (`crates/zeroclaw-api/src/webhook.rs:171`; `crates/zeroclaw-infra/src/plugin_webhook.rs:60-61`, `:108` @7962184961);
  - **cap:** 1024 in-flight deliveries per connection (`rt/rpc/dispatch.rs:16` @7962184961), where **Execution** below caps at the route queue depth, 64.
- **Still open; the series does not settle them:** the ingress connection's identity (D1: the series admits it as the shared operator); the cap and deadline alignment (64 and an optional clamped `deadline_ms`, as here, or the series' 1024 and fixed deadline); and process-scope deduplication (§12.6, DEC-12: the series' reservation store is created per daemon generation).

No diagnostic text crosses the boundary: guest details stay in core logs, matching today's rule that the gateway drops them (`gw/plugin_webhook.rs:158-196`). The plugin WIT reply has no plugin-controlled status or headers (`wit/v0/channel.wit` `webhook-response::reply`), so "response headers" in #11003 AC1 is satisfied by: none, beyond the content type.

**Execution.** The core spawns each delivery (as it spawns `session/prompt`, `dispatch.rs:2946-2967`) under a per-connection cap equal to the route queue depth (64, `rt/plugin_runtime.rs:653`); past the cap the call answers `queue_full` immediately instead of parking the connection. Deliveries count toward the connection's drain on shutdown.

**Cancellation (decided, proposed).** Each delivery is one JSON-RPC request with its own `delivery_id`. The core cancels a delivery when (a) its clamped deadline passes, (b) the gateway sends `ingress/cancel {delivery_id}` because that delivery's HTTP caller went away (the per-request equivalent of today's drop guard, `gw/plugin_webhook.rs:121-122`), or (c) the connection that carried it closes. Many deliveries share one ingress connection, so (b) is required: without it the only way to cancel one delivery would be to close the connection and cancel all of them. (c) cancels only deliveries that arrived on that connection; they all belong to the gateway instance that just lost its connection, whose HTTP callers are gone with it; nothing started from any other connection is touched. `ingress/cancel` for an unknown or finished `delivery_id` is a no-op answering `{cancelled: false}`. A delivery cancelled before its reservation commits is rolled back (the worker owns commit and rollback, `crates/zeroclaw-plugins/src/wasm_channel.rs:614-719`; a waiter treats a vanished owner as rollback, `api/webhook.rs:71-75`), so the vendor's retry is processed; a delivery cancelled after the commit has already been enqueued and is not re-run. A vendor retry after a lost response finds the reservation committed and gets `ack` (duplicate ignored), within the idempotency window of §12.6.

### 12.4 Generation fence `[proposed]` (bug fix, can land in-process now)

`PluginWebhookRegistry::get` clones the route's sender (`api/webhook.rs:234-237`); `start_generation` clears the map but cannot revoke a clone taken before it (`:223-231`). A delivery looked up before a reload can therefore be sent into the retiring generation's worker. Required: the registry stores `(generation, sender)` and the send re-checks the generation under the registry lock, or `get` returns a handle that fails once a newer generation starts. Tests: lookup-before-reload/send-after (→ `unavailable` or redelivery to the new owner, never the old one), and an unchanged path re-registered in the new generation keeps dispatching (not `not_found`).

**Unload and replacement (decided, matches current code).** The route table changes only at a channel-supervisor generation: the supervisor calls `start_generation()` on each start and publishes its whole validated map through its lease (`rt/plugin_runtime.rs:652-683`; `api/webhook.rs:222-268`). Unloading or replacing a plugin therefore takes effect at the next reload that restarts the channel supervisor, and not before. Runtime replacement without a reload is not part of this contract; a later design (for example #11261's staged replacement, which today touches only the plugin host) must publish a new generation through the same lease to change routes. With the fence above, no delivery accepted after a new generation starts reaches a retired generation's worker, and an unchanged path re-registered in the new generation keeps dispatching.

### 12.5 Generic `/webhook` and `/sop/*`: `ingress/webhook` and `ingress/sop` `[proposed]` (G3)

These routes carry a **route-level credential** (pairing bearer and/or `X-Webhook-Secret`) in addition to the body. Forwarding the bearer on a credential-bound connection is not enough, because the core would never see the secret requirement. They cross as evidence:

```jsonc
// ingress/webhook params (POST /webhook)
{
  "evidence": {
    "bearer": "<value after 'Bearer ' in Authorization, or absent>",
    "webhook_secret": "<X-Webhook-Secret value, trimmed, or absent>"
  },
  "client_key": "<the gateway's rate-limit client key>",   // informational; the core applies its own auth-attempt limiter keyed by it
  "idempotency_key": "<X-Idempotency-Key, trimmed, or absent>",
  "session_id": "<X-Session-Id, trimmed, or absent>",
  "agent": "<?agent= query value, or absent>",
  "body": {"message": "…"},
  "stream": false,                                          // true when the HTTP caller asked for the streaming response
  "delivery_id": "…"                                        // ING-4
}
// ingress/sop params (POST /sop/{*rest}): same evidence, idempotency_key and delivery_id, plus
{ "path": "/sop/<rest>", "body_b64": "…" }
// result, exactly one of:
{"kind": "chat", "session_id": "…", "content": "…", "model": "…"}  // the turn's final text and today's model label (below); with stream: true, content is also streamed
{"kind": "sop", "status": "accepted"|"blocked", "source": "webhook", "path": "…",
 "results": [SopDispatchEventEntry, …]}          // C.4; "sop" is null on a blocked_unsafe entry that selected no SOP
                                                            // today's body; HTTP 422 when every result is blocked, else 200 (gw/api_sop_webhook.rs:113-168)
{"kind": "duplicate"}                                       // HTTP 200 {"status":"duplicate","idempotent":true,"message":…}, today's body (gw/lib.rs:3317-3345)
{"kind": "sop_no_match", "path": "/sop/<rest>"}             // ingress/sop only: HTTP 404 {"error":"no_matching_sop","path":…} (gw/api_sop_webhook.rs:214-223, :239-247)
```

The core evaluates **exactly today's truth table** (`gw/lib.rs:3172-3268`, composition rule at `:3160-3170`), from one policy snapshot per request (pairing authority + secret), in the core:

| `gateway.require_pairing` | `gateway.webhook_secret` set | Admitted when | SOP dispatch allowed | Chat fallback allowed |
|---|---|---|---|---|
| true | yes | bearer paired **and** secret matches | yes | yes |
| true | no | bearer paired | yes | yes |
| false | yes | secret matches | yes | yes |
| false | no | always | **no** (fail closed, `gw/lib.rs:3355-3374`) | yes (today's default-open chat path) |

Then, as today: SOP-first dispatch before any chat-only inspection (`gw/lib.rs:3405-3431`); `?agent=` validated only on the chat path (`:3434-3462`); idempotency via the core store with the same injective key encoding (`gw/lib.rs:3284-3310`, `:3317-3345`); `X-Session-Id` read at `:3469`; the chat turn runs in the core. Principal of the resulting work: the principal the bearer resolves to when a bearer was verified; otherwise the webhook-origin owner rule, **OPEN D3**. `session_id` continuation is honoured only if that principal may access the session (ownership predicate, as `session/prompt` applies), else `-32003`; under D3's "no owner" answer, a secret-only caller cannot continue a session.

**Provenance.** Today the gateway itself stamps the `/webhook` chat turn `TurnOrigin::Interactive` (`gw/lib.rs:3036`; enum at `api/ingress.rs:36-53`), and transport stamping is still phase 1 (`Transport::Internal` everywhere, `api/ingress.rs:16-32`). After the split the core's `ingress/*` handler is the trusted entry code that stamps the turn's ingress envelope (ADR-018: provenance is stamped by entry code, never by message content or a caller's claim). No gateway-supplied field selects the origin, transport or trust class. Which origin a webhook turn carries (`Interactive` today) is part of D3.

Responses carry the same HTTP bodies the gateway returns today. Errors map `-32010` → 401 with today's messages and `-32012` → 403.

**HTTP translation of the chat result.** A non-streamed chat answers `200 {"response": content, "model": model}`, today's body exactly (`gw/lib.rs:3533`); `session_id` is not added to it. `model` is the label today's handler computes before the turn from canonical config, which only the core can now read: the configured model of the resolved agent's model provider, otherwise the default model, otherwise `"<unresolved>"` (`gw/lib.rs:3484-3501`). It is the *configured* model, as today, not the model that served the turn; a safeguard fallback shows the served model only in the stream's `turn_complete.safeguard_fallback`. A streamed chat (`stream: true` and `Accept: text/event-stream`, `gw/lib.rs:3514-3522`) keeps today's server-sent events: the gateway accumulates the tagged `agent_message_chunk` text into cumulative `event: token` frames `{"text": …}` (`gw/lib.rs:3586-3590`), reconciles the cumulative text with the final `content` exactly as `reconcile_sse_final_response` does (`gw/lib.rs:3613-3626`), and ends with `event: done` `{}` on a completed turn or `event: error` `{"message": …}` on a failed one (`gw/lib.rs:3592-3596`, `:3899`); a stream whose caller left sends nothing further. `needs_quickstart` before a stream answers the JSON 503 as today (`gw/lib.rs:3755-3765`).

**`ingress/sop` checks and outcomes, in order** (today's `handle_sop_webhook`, `gw/api_sop_webhook.rs:173-249`): the truth table; the fail-closed SOP credential rule; an unparseable body → `-32602`, `data.reason: "invalid_json"` (400 `{"error":"invalid_json"}`); no SOP engine or audit → `-32603`, `data.reason: "sop_dispatch_unavailable"` (503); no SOP matches the path → the result `{kind: "sop_no_match", path}` (404 `{"error":"no_matching_sop","path":…}`) **before any reservation**, so no key is touched; then the reservation and dispatch. If the match disappears between that check and dispatch (a SOP unloaded in between, today's second no-match branch at `:239-247`), the reservation is rolled back and the result is again `sop_no_match`. `ingress/sop` never falls through to chat; only `ingress/webhook` does, when no SOP matches `/webhook`. The core's attempt limiter answers `-32010` with `data: {reason: "rate_limited", retry_after_secs}`, which the gateway maps to 429 with `retry_after` as today.

**Streaming correlation (DEC-23).** Several streamed calls run at once on the one ingress connection, and a call without `session_id` learns its generated session id only from its final response, so the session id cannot route stream events. With `stream: true`, every `session/update` the core emits for the turn this call admitted carries the call's `delivery_id` and goes only to the calling connection; the first is `turn_started` (DEC-22), which also names the session and the turn. The gateway routes each tagged event to the HTTP caller that owns that `delivery_id` and never infers ownership from the session id; `zeroclaw-rpc-client` hands every notification to one broadcast channel, independent of response ids (`route_frame`, `crates/zeroclaw-rpc-client/src/client.rs:307-315` @337d0a186f), so the gateway's ingress client demultiplexes that channel by `delivery_id`. Tagging covers exactly the turn this call admitted: in a continued session, turns started by anyone else are never tagged with it, and other viewers of the session (TL-2) receive the same events without the tag. The `delivery_id` confers nothing: the turn runs as the principal resolved from the evidence (a verified bearer's principal, otherwise the D3 owner rule), and only the connection that sent the call ever receives its tag.

**Cancellation and lifetime.** An ingress call ends when its response is sent, `ingress/cancel {delivery_id}` arrives, or its connection closes; the last two are how the HTTP caller's departure reaches the core, as today's handler future is dropped when its caller leaves. What cancelling does depends on how far the call got:

| Point reached | `ingress/cancel` or connection close |
|---|---|
| before admission (evidence checks, SOP matching, agent resolution, readiness) | the call ends with no effect; its idempotency reservation is rolled back |
| chat turn admitted and running | the turn is cancelled with cause `ingress_caller_gone`; the key stays committed (below) |
| SOP run created | the run is **not** cancelled: a run is durable work that outlives the webhook that started it, as today; cancelling a run is `sops/cancel` |

A gateway restart therefore cancels only the calls in flight on its own ingress connection; turns and runs started from any other connection, including a dashboard user's turn in the same session, continue. This is the ingress exception to TL-1: an ingress chat turn is bound to its delivery, not to its session, because the HTTP caller is waiting for its response (D12 asks whether that should change). Viewers are separate from deliveries: a dashboard viewer that attaches to the session (TL-2) and later detaches changes nothing for the delivery, and cancelling the delivery ends the turn for every viewer. Plugin deliveries (`webhook/deliver`) have no stream: they end at the enqueue (§12.3), and the agent turn that later handles the message belongs to the channel, so neither `ingress/cancel` nor connection loss after the enqueue affects it. Tests CT-ING-10 (two concurrent streamed calls without `X-Session-Id` each receive only their own events), CT-ING-11 (one HTTP caller disconnects: its turn alone is cancelled, the other stream completes), CT-ING-12 (gateway killed with two streams in flight: both turns cancelled with `ingress_caller_gone`, a dashboard turn in one of those sessions completes, and the restarted gateway receives no event tagged with an old `delivery_id`).

**Idempotency acceptance point (DEC-24).** Today the generic routes record the key as consumed before dispatch (`record_if_new`, `gw/lib.rs:415-452`, called at `:3420` before SOP dispatch and at `:3464` before chat execution), and the code says so ("the idempotency key above is already consumed for this attempt", `:3428-3431`). A request refused after that point, for example `503 needs_quickstart` (`:3548-3560`), still consumes its key: a retry after the operator configures a model gets `duplicate` and is never processed. The ingress operations instead use the reservation half of the store for generic keys too: **reserve** after the evidence is verified; **commit** at the acceptance point, which is when a chat turn is admitted (agent resolved, a model provider configured, the turn registered) or when at least one matched SOP `started`, `deferred` or `coalesced` the event; **roll back** on every refusal or cancellation before that point (validation, `needs_quickstart`, capacity, SOP results that are all `skipped`, `blocked_unsafe` or no match, `ingress/cancel`). A concurrent duplicate waits for the reservation's outcome, as plugin waiters do. What each outcome means for a retry with the same key:

| Outcome of the first request | A retry with the same key within the TTL |
|---|---|
| refused or cancelled before the acceptance point | processed afresh |
| admitted, completed | `duplicate` |
| admitted, then failed, or cancelled because its caller left | `duplicate`: the key means "admitted once", as today (D12) |
| response lost after admission (gateway or connection died) | `duplicate` |
| core restarted | processed again: the store is in memory (§12.6) |

Test CT-ING-13: a first request refused with `needs_quickstart` → configure a model provider and reload → the retry with the same key is processed.

Which core operation owns each check after the split (decided, proposed):

| Check (today in the gateway) | Today | Owner after the split |
|---|---|---|
| per-client webhook rate limit | `gw/lib.rs:3177-3190` | gateway edge (AUTH-2); the core adds its own bound on in-flight ingress calls |
| pairing bearer, auth-attempt limiter | `gw/lib.rs:3198-3236` | `ingress/webhook` / `ingress/sop` in the core, against the core's pairing authority; attempt limiter keyed by the forwarded `client_key` plus a core-wide budget |
| `X-Webhook-Secret` constant-time match | `gw/lib.rs:3238-3259` | same core operation (§12.7) |
| one policy snapshot per request (no straddling a rotation) | `gw/lib.rs:3135-3170` | same core operation: pairing and secret read once under one snapshot |
| SOP dispatch requires a configured credential (fail closed) | `gw/lib.rs:3355-3374`; `gw/api_sop_webhook.rs:184-191` | same core operation |
| SOP-first dispatch before chat | `gw/lib.rs:3405-3432` | `ingress/webhook` |
| `?agent=` validation (chat path only) | `gw/lib.rs:3434-3462` | `ingress/webhook` |
| `X-Idempotency-Key` dedupe, injective key | `gw/lib.rs:3284-3345`; `gw/api_sop_webhook.rs:227-236` | core-owned store (§12.6), called by the ingress operation after verification |
| `X-Session-Id` scope / continuation | `gw/lib.rs:241-255`, `:3469` | `ingress/webhook`, under the D3 owner rule |
| the chat turn itself | in the gateway (`gw/lib.rs:2976-3043`) | the core |

**Consequence for #11169.** Its `sops/dispatch-event` documents "the caller authenticates the delivery and applies idempotency". That is acceptable for an authenticated principal that holds `sops:execute` (an operator tool), and **not** as the gateway's path for `/sop/*`: the gateway must use `ingress/sop` so the secret/pairing conjunction and idempotency stay in the core.

### 12.6 SETTLED: webhook deduplication/idempotency is core-owned

Decision (DEC-12, proposed; settled here because it decides whether config keys must move at V4): **the core owns every ingress idempotency store.** Reasons, each from code:

1. The plugin worker calls the store synchronously in the middle of a delivery: reserve, enqueue, commit (`crates/zeroclaw-plugins/src/wasm_channel.rs:614-719`). The worker runs in the core; a gateway-held store would need a core→gateway round trip inside every delivery.
2. The store is rebuilt per gateway run today (`gw/lib.rs:1677-1684`), so every gateway restart and reload already wipes dedup state. After the split, gateway restarts become routine; a vendor retry that straddles one would be processed twice.
3. With the credential truth table in the core (§12.5), the dedup decision for `/webhook` and `/sop/*` happens after verification in the core; splitting it across processes would let an unauthenticated duplicate consume a key.

Specification:

- One store instance at **daemon-process scope** (created outside the reload loop), so dedup survives gateway restarts and config reloads. It keeps one mechanism for both kinds of key: reservations (`begin`/`commit`/`rollback` with waiters, `gw/lib.rs:455-541`). The generic routes stop using `record_if_new` (`gw/lib.rs:415-452`), which commits before dispatch (§12.5, DEC-24). Generic keys keep their encoding: length-prefixed and namespaced (`gw/lib.rs:3284-3310`). Plugin keys gain the owning endpoint instance (last bullet). The process-scope move lands together with, or after, the instance-scoped plugin key; until both land the store keeps today's per-run lifetime.
- **Guarantees, stated per kind of key.** Both hold per `(route, key)` inside the TTL window, bounded by `max_keys`, for the life of the core process; both are lost on core restart; neither is exactly-once.
  - *Plugin deliveries:* the reservation commits when the message is enqueued on the channel's inbound queue (the agent turn runs later); a delivery that failed, timed out or was cancelled before that is rolled back and a vendor retry is processed; after the commit a retry is acknowledged as a duplicate. Messages without an id are not tracked (`wasm_channel.rs:622-624`).
  - *Generic `/webhook` and `/sop/*`:* "admitted once" (DEC-24): the key commits at admission; refusals never consume it; admitted work that later fails or is cancelled because its caller left keeps the key, so a retry returns `duplicate` (today's behaviour; D12 asks whether to keep it). This is not at-least-once completion.
- **Config.** `gateway.idempotency_ttl_secs` and `gateway.idempotency_max_keys` keep their names (HTTP-ingress settings can stay under `[gateway]`), but their canonical owner and only reader becomes the core; today only the gateway reads them (`gw/lib.rs:1677-1684`). No V4 transform is needed. `gateway.webhook_rate_limit_per_minute` stays an edge setting read by the gateway.
- **Key scope and retention across reloads (decided, proposed).** Because the store now outlives reloads, a path reassigned to another plugin must not inherit the previous owner's records. Plugin reservation keys therefore include the owning endpoint's instance identity: `plugin-webhook:` + hex SHA-256 over `"zeroclaw-plugin-webhook\0" ‖ instance_id ‖ "\0" ‖ path ‖ "\0" ‖ message_id`, where `instance_id` is `PluginChannelEndpoint::instance_id()` (`crates/zeroclaw-plugins/src/endpoint.rs:61`). Today's key omits the instance (`gw/plugin_webhook.rs:20-29`); it was safe only because every reload rebuilt the store. Records expire by TTL; a reload neither clears nor migrates them, and a different owner's key space is disjoint. Generic `/webhook` and `/sop/*` keys keep their namespaced encoding.

### 12.7 SETTLED: `webhook_secret` is verified in the core

Decision (DEC-13, proposed): the gateway forwards the presented `X-Webhook-Secret` value as evidence (§12.5); **the core** compares it in constant time against its configured secret (hash both, then `constant_time_eq`, as `gw/lib.rs:3238-3259` does today). The gateway process never reads, decrypts or receives the configured secret.

Why not the edge: an edge check needs the decrypted secret in the internet-facing process, which means either the config master key there or the core sending the plaintext secret over IPC (ING-3); and it splits the pairing ∧ secret conjunction across processes, so a buggy gateway could let a bearer-only request through while the core, which never saw the requirement, runs it. The cost is one IPC round trip per bad request; the gateway's per-client limiter (`webhook_rate_limit_per_minute`) and the core's auth-attempt limiter bound it.

Config consequence: `gateway.webhook_secret` keeps its name and `#[secret]` / `credential_class = "encrypted_secret"` attributes; its canonical owner and only reader becomes the core. The name still describes what it protects (callers of the gateway's `/webhook` and `/sop/*` routes). Renaming it is not needed for the split and would cost a schema version with the migration hazards a review of the rename option found (rename after edit, environment aliases, downgrade).

### 12.8 Ingress availability `[proposed]` (#11002 AC4)

- `gateway/register` (§8.13) records that an ingress-capable gateway is connected; the record ends when that connection closes.
- While no gateway is registered, `health` reports `ingress: "gateway_absent"` for every webhook-driven channel instance and plugin route, and `webhook/routes` (optional, read-only, generation-stamped) lets diagnostics list what would be unreachable. Never used for dispatch.

### 12.9 Native vendor routes **OPEN D7**

WhatsApp Cloud, Linq, Nextcloud Talk and Gmail push are verified, parsed and answered **inside the gateway** against gateway-built channel instances holding their secrets (§12.1). They cannot be served by a gateway without `zeroclaw-channels` and the runtime. Options for v0.9.0: (a) keep them on the in-process gateway only and have `zeroclaw-gw` answer `503` with a migration message; (b) move verify+parse into the core's channel instances behind the same registry and `webhook/deliver` (large; overlaps #8850); (c) block the separate gateway on #8850's plugin packages. Recommendation: (a) for v0.9.0, (b) as the #8850-aligned end state. Static-read side finding, not reproduced: the gateway's `GmailPushChannel` is never `listen()`ed, so its notifications are dropped while the route answers 200 (`gw/lib.rs:1608`; `ch/gmail_push.rs:440-452`). That is a bug independent of G1.

### 12.10 Ingress conformance tests

| ID | Asserts | Status |
|---|---|---|
| CT-ING-01 | Every `WebhookOutcome`/`WebhookReject` maps to the §12.3 status through the IPC path (mirror of `gw/plugin_webhook/tests.rs`) | `[proposed]` |
| CT-ING-02 | Duplicate delivery: committed → `ack`; in-flight → waits then `ack`; rolled back → redelivered | `[proposed]` (in-process tests exist in `gw/plugin_webhook/tests.rs`) |
| CT-ING-03 | Generation fence (§12.4) both cases | `[proposed]` |
| CT-ING-04 | Connection closed mid-delivery cancels guest work; deadline → `timeout` | `[proposed]` |
| CT-ING-05 | 65 deliveries in flight on one connection → the 65th gets `queue_full` without blocking a concurrent `health` on another connection | `[proposed]` |
| CT-ING-06 | Truth table (§12.5), all rows, including invalid bearer with valid secret, secret rotation between two requests, and SOP-first ordering; byte-for-byte HTTP parity of every outcome, including a successful non-streamed chat `{response, model}` and a streamed chat's `token`…`done` frame sequence | `[proposed]` (in-process tests exist in `gw/lib.rs`) |
| CT-ING-07 | The gateway crate contains no read of `webhook_secret` and no `constant_time_eq` (symbol grep) after G3 | `[proposed]` |
| CT-ING-08 | Gateway restart between a delivery and its vendor retry → one delivery | `[proposed]` |
| CT-ING-09 | Core unavailable → 503 at the gateway; gateway absent → `health` shows `gateway_absent` | `[proposed]` |
| CT-ING-10 | Two concurrent streamed `ingress/webhook` calls without `session_id` on one connection: each HTTP caller receives only the events tagged with its own `delivery_id`, starting with `turn_started` | `[proposed]` |
| CT-ING-11 | One HTTP caller disconnects mid-stream: `ingress/cancel` cancels its turn alone (`ingress_caller_gone`); the concurrent stream completes | `[proposed]` |
| CT-ING-12 | Gateway killed with two ingress streams in flight: both turns end cancelled; a dashboard turn in one of those sessions, started on another connection, completes; after restart no event tagged with an old `delivery_id` reaches the new connection | `[proposed]` |
| CT-ING-13 | A generic request refused before admission (`needs_quickstart`) → configure a model provider and reload → the retry with the same `X-Idempotency-Key` is processed; an admitted request's retry returns `duplicate` | `[proposed]` |
| CT-ING-14 | A second call reusing an in-flight `delivery_id` is refused with `duplicate_delivery_id` and the first continues; `ingress/cancel` with a finished, unknown or other-connection `delivery_id` answers `{cancelled: false}` and cancels nothing; for both ingress families | `[proposed]` |
| CT-ING-15 | `/sop/<rest>` with no matching SOP → 404 `{"error":"no_matching_sop","path":…}` and no key reserved; a SOP unloaded between the match and dispatch → the same 404 and the reservation rolled back, so a retry after the SOP is reloaded is processed; `/webhook` with no matching SOP falls through to chat | `[proposed]` |

## 13. Configuration surface (matches the schema-impact proposal)

### 13.1 Keys the contract reads, with their canonical owner (today's names, no moves)

"Canonical owner" is the one process whose reading of the key is authoritative after the split; "reader" lists who may read it. No key changes name or section for the split. The only V4 change in this area is retiring the inert `[gateway.pairing_dashboard]` (never read; pairing uses fixed limits, `cfg/pairing.rs:10-12`, `:305`).

| Key (today's name) | Canonical owner | Readers after the split | Contract role |
|---|---|---|---|
| `gateway.require_pairing`, `gateway.paired_tokens`, `[gateway.pairing_code]` | core | core only; the gateway receives a read-only `PairingPosture` from `gateway/register` and `gateway/settings` (§8.13), never the keys | the single pairing authority the core already builds from these fields (`rt/daemon/mod.rs:617-621`; `rt/rpc/auth.rs:300-305`, `:381-386`); §8.8; the truth table of §12.5; D2 |
| `gateway.webhook_secret` | core | core only | §12.7 |
| `gateway.idempotency_ttl_secs`, `gateway.idempotency_max_keys` | core | core only | §12.6 |
| `gateway.session_persistence`, `gateway.session_ttl_hours` | core (sessions are core state) | core | location unchanged |
| `gateway.allow_self_upgrade` | core (authorizes a binary swap) | core | two-binary semantics belong to G5 |
| `gateway.host`, `port`, `tls.*`, `path_prefix`, `allow_public_bind`, `web_dist_dir`, `trust_forwarded_headers`, `rate_limit_max_keys`, `pair_rate_limit_per_minute`, `webhook_rate_limit_per_minute`, `request_timeout_secs`, `long_running_request_timeout_secs`, `websocket_ping_interval_secs`, `allow_remote_admin`, `check_updates` | gateway (edge policy) | gateway; the core reads host/port/prefix only to print URLs | AUTH-2: may narrow, never grant |
| `rpc.max_local_connections` | core | core | §3.5 |
| `security.trust_daemon_uid`, `[users]`, `[oidc]`, `[permission_profiles]` | core | core only (OIDC relays per §8.14.1) | §5.1 |

Permission profiles' stored config-write selectors keep matching these `gateway.*` paths unchanged, which is one reason the split keeps today's names instead of renaming them.

### 13.2 New keys the contract needs (all additive, no schema-version change)

| Key | Needed for | Status |
|---|---|---|
| optional `[rpc] endpoint` | core-side override of where it binds; clients learn it from their launcher (§4.2) | optional; not required for v0.9.0 |
| gateway supervision mode (`gateway.mode = child\|external\|disabled` or equivalent) | §11.3 | **OPEN D5** (name, default) |
| separate-account layout: the IPC directory, the gateway's Unix group, the gateway's Windows account (for the pipe DACL and the bootstrap ACL) | §5.5.1 | `[proposed]`, only if D11 admits separate accounts; names proposed as `[rpc] endpoint` (above, placing the socket in `<ipc_dir>`), `[rpc] gateway_group` (Unix) and `[rpc] gateway_account` (Windows). Core-side settings, because the core must apply them on every bind |
| the expected core uid/SID a client checks (§5.5) | §5.5 | launcher flags/env, not config: it is the client's input |

Nothing in this contract requires a key to move or be renamed. The endpoint is discoverable without any key (§4); on Unix the default is `<data_dir>/daemon.sock`, on Windows a data-directory-derived named pipe (`rt/rpc/local.rs:1035-1041`), and `ZEROCLAW_SOCKET` overrides both.

### 13.3 How a separate gateway gets its settings

A hardened gateway does not read `config.toml` (parsing selected fields still opens the whole file, including authority keys and ciphertext). It gets bind settings from its launcher (flags/env) or from a **core-generated, non-secret projection**, `bootstrap.json`, written by the core on start and on every accepted config generation and containing only the edge keys of §13.1; effective settings and the pairing posture then come from `gateway/register` (§8.13). Where the projection lives depends on the layout: in the separate-account layout, `<ipc_dir>/gateway/bootstrap.json` (§5.5.1), outside the private data directory, because a group-readable file under a `0700` directory is unreachable; in the same-account layout it may stay under `<data_dir>`, since the gateway can read everything there anyway. The projection is a materialized view of canonical config, never an editable policy source. In the default same-user child mode the core passes the bind settings as flags.

## 14. Names, features, endpoints and platforms

### 14.1 Name mapping (#11000 AC5)

| Name in RFC #5574 / older prose | Exists at f0ae8c8bd8? | Current implementation | Name this contract uses |
|---|---|---|---|
| `zeroclaw-kernel` | no crate | the `zeroclaw` binary (root crate, composition in `src/`) plus `zeroclaw-runtime` | "the core"; binary `zeroclaw` |
| `ipc` feature | no | the local listener is compiled with the runtime and registered unconditionally by `zeroclaw daemon` (`root/src/main.rs:7312-7322`) | no feature |
| `kernel.sock` | no | `<data_dir>/daemon.sock`; `\\.\pipe\zeroclaw-<hash>` (`rt/rpc/local.rs:866-868`, `:1035-1041`) | "the core endpoint" |
| `zeroclaw-gw` | no | library crate `zeroclaw-gateway`, run in-process by the daemon's gateway starter behind root feature `gateway` (`root/Cargo.toml:296-300`; `rt/daemon/mod.rs:642-701`) | `zeroclaw-gw`: a `[[bin]]` in `crates/zeroclaw-gateway` `[proposed]` (G3) |
| wire-contract crate | open PR | `zeroclaw-rpc-proto` (#11165) | same |
| client crate | open PR | `zeroclaw-rpc-client` (#11186) | same |
| "IPC protocol" | yes | NDJSON JSON-RPC 2.0, `RPC_PROTOCOL_VERSION = 1` (`rt/rpc/dispatch.rs:36`) | "core IPC protocol v1" |
| OpenAPI 3.1 contract | partial | gateway-generated `"openapi": "3.1.0"` document with a partial route inventory (`gw/openapi.rs:450`; `docs/book/src/gateway/api.md:8`) | the gateway's HTTP contract only (§2) |

Features that put gateway code into the core artifact today (`root/Cargo.toml`): `gateway` (in `default`, `:250-257`, `:296-300`, pulls `zeroclaw-gateway`, `axum`, `tower`); `observability-prometheus` (in `default`) forwards `zeroclaw-gateway/observability-prometheus` **without `?`**, so enabling it pulls the gateway crate even when `gateway` is off (`:348-351`); `embedded-web` likewise forwards `zeroclaw-gateway/embedded-web` without `?` (`:369`). G3 must fix both forwards to `zeroclaw-gateway?/…` before a core artifact can be built without the gateway crate.

### 14.2 Unix and Windows behaviour

| Property | Unix | Windows | Status |
|---|---|---|---|
| Endpoint | `<data_dir>/daemon.sock`, `ZEROCLAW_SOCKET` override | `\\.\pipe\zeroclaw-<DefaultHasher(data_dir)>`, same override | `[master]`; stable pipe derivation `[proposed]` (§4.2) |
| Endpoint path limit | `sun_path` − 1 bytes (103 on macOS/BSD, 107 on Linux), refused before any lock file is created (`rt/rpc/local.rs:953-984`) | n/a | `[master]` |
| Exclusive ownership | lifecycle lock file `<socket>.lock` in a trusted directory; stale-socket probe; replacement never unlinked by the old owner (`rt/rpc/local.rs:679-951`) | `first_pipe_instance(true)` on the first instance (`:1049-1056`) | `[master]` |
| Access control | socket 0600, parent 0700 best effort (`:878-887`, `:992-997`); the separate-account layout of §5.5.1 `[proposed]` | default security descriptor: full control for LocalSystem, Administrators, creator owner; **read for Everyone and anonymous** (`CreateNamedPipe` docs); replacement instances created with defaults too (`:1064-1076`) | `[master]`; explicit DACL on every instance `[proposed]` (§5.5) |
| Remote clients | n/a | rejected (tokio `reject_remote_clients` defaults to true) | `[master]` via dependency default |
| Peer identity seen by the core | kernel uid (`SO_PEERCRED`/`getpeereid`), label `unix:pid=…,uid=…` (`:1007-1019`) | none; label `pipe:local` (`:1078-1087`) | `[master]`; `GetNamedPipeClientProcessId` → SID `[proposed]` as a later hardening (would make D1-B possible on Windows) |
| Server identity seen by the client | not checked today; peer uid plus socket-directory ownership in #11274 | not checked today | `[proposed]` §5.5 |
| Framing, limits, handshake | identical | identical (shared split read/write loop) | `[master]` |
| Conformance coverage today | extensive (`rt/rpc/local.rs` tests: handshake, pre-initialize rejection, permissions, ownership, stale cleanup, limits, deadlines, reload drain) | three tests: `pipe_oversized_frame_gets_frame_too_large_then_close` (`rt/rpc/local.rs:3181`), `pipe_connections_past_the_ceiling_get_a_diagnostic_and_are_closed` (`:3234`), `pipe_initialize_handshake` (`:3294`) | see §15.2 for the #11001 AC2 gap list |

### 14.3 "No HTTP server in the core artifact" defined **OPEN D8**

Tracker #7432's gateway acceptance says the core release artifact has no web assets or HTTP server code. Precisely: the `zeroclaw` binary built with the dist feature selection (`root/Cargo.toml:212-231`) links no HTTP server framework and serves no HTTP route. At f0ae8c8bd8 the core can bind these listeners:

| Listener | Protocol | Default | Disposition needed |
|---|---|---|---|
| Local IPC | NDJSON JSON-RPC over a socket/pipe; not HTTP | always | keep (this contract) |
| In-process gateway | axum HTTP/WS/SSE | on (`gateway` in `default`) | leaves the core at the G3 cut |
| `[wss]` remote RPC plane | TLS + WebSocket upgrade (an HTTP/1.1 handshake parsed by `tokio-tungstenite`), then JSON-RPC (`rt/rpc/wss.rs:921-931`, `:1084`) | off (`[wss] enabled = false`); there is no `rpc-wss` Cargo feature, the plane is always compiled and switched at runtime (`root/src/main.rs:7324-7334`) | **D8-a**: documented exception (it is the remote RPC plane, not the web surface), or move to the gateway |
| `[enroll]` certificate enrollment | TLS + a hand-rolled single-route HTTP exchange (`rt/enroll/mod.rs:1-22`, `:331-340`) | off (`[enroll] enabled = false`, requires `[wss]`) | **D8-b**: documented exception with `[wss]`, or move |
| Channel listeners: generic webhook (`channel-webhook`, in `default-channels`), Lark (`channel-lark`, in dist), LINE, voice-call | axum servers inside the core (`ch/webhook.rs:512`, `ch/lark.rs:3949-3950`, `ch/line.rs:1259`, `ch/voice_call.rs:456-469`) | on when configured | **D8-c**: register routes behind `webhook/deliver`, keep as a dated compatibility feature, or accept as channel-owned listeners outside the rule |

Recommendation: D8-a and D8-b as documented exceptions (opt-in, off by default, not the web surface, needed by the relay/WSS clients); D8-c as "channel listeners migrate behind `webhook/deliver`; until then they are a named, dated exception in the release notes", because removing them from dist without a migration is a feature removal. The artifact check (CT-ART-01) inspects the feature-resolved dependency graph of the actually built `zeroclaw` binary for `axum`/`hyper` server/`tower-http`, allowing only the exceptions the maintainers accept, plus a `strings` scan for dashboard assets.

## 15. Coverage matrix

### 15.1 Sources read

| Ref | Head read | Adds to this contract |
|---|---|---|
| `upstream/master` | f0ae8c8bd8 | 97 methods (`rt/rpc/dispatch.rs:221-336`), the transport, auth and subscription behaviour cited throughout |
| `upstream/master` after the baseline | f151c2e00d (4 commits) | #10621 adds `agents/delete-preview` and `agents/delete` (typed in C.5, errors in E.4) and additive fields `ConfigSetParams.comment?` and `ConfigMapKeyRenameResult.rewritten`; #10246 fences remote (WSS) resumes of sessions that hold local capabilities, a V-5 security correction; #11287 (cost ceilings) and #11045 (peer-agent inbox persistence) change no wire shape. Everything else in this contract stays pinned to the baseline, so its `path:line` citations and counts are at f0ae8c8bd8 |
| #11165 `zeroclaw-rpc-proto` + OpenRPC | f74d25a0f4 | the method table, 134 payload types and the handshake types move to the proto crate; `docs/book/src/architecture/zeroclaw-rpc.openrpc.json` (OpenRPC 1.3.2, Draft 7, 97 methods, 157 schemas) with a byte-for-byte drift check in the required `Installer Drift` job. Gaps: SCH-1..SCH-5 (§8.0); the proto crate's "no async I/O" claim holds only for direct dependencies (it reaches tokio, reqwest and SQLite through `zeroclaw-api`/`zeroclaw-config`) |
| #11186 `zeroclaw-rpc-client` + in-process seam | 337d0a186f | `RpcClient::connect_local`/`connect_over`, handshake, correlation, bounded inbound requests, `Backoff`; `TransportKind::Inproc` duplex that refuses credential-less `initialize`; the gateway's seam dials with no credential and idles; no route uses it; all tests in memory |
| #11132 turn parity | fad6b15824 | `session/steer`, `session/abort`, `session/append`, `session/rename`, `session/run-once` |
| #11149 cron authority | 42b73abfc1 | no new method; `cron/add` / `cron/patch` stop pre-approving shell commands |
| #11176 cron/memory/skills/personality/quickstart parity | c0076cfe5d | `skills/create`, `skills/effective`, `skills/slash-option-kinds` and parity changes |
| #11169 SOP parity | 908a035d3f | `sops/cancel`, `sops/dispatch-event`, `sops/decision-models`, `sops/graph-legend` |
| #11172 config parity | cee32cf254 | `config/{reload-status, drift, agent-options, section-picker, section-select, delete-plan, init, migrate}`, `providers/refresh-context-window` |
| #11185 session-owned turns | 6713ff419b | `session/attach` plus the #11132 set |
| #11182 P6 core parity (draft) | b0ad81bfd2 | `workspace/list`, `fs/{read,mkdir,rmdir,delete,move}`, `tools/{list,cli-discover}`, `integrations/list`, `plugins/list`, `a2a/identity`, `canvas/{list,get,history,render,clear}` (new `Canvas` resource), `metrics/scrape`, `pairing/{list,new-code,revoke,revoke-all}`, `channels/{list,relink,bind}`, `system/{upgrade,upgrade-status,restart}` |
| #11273 transport conformance harness (draft, opened 2026-09-30) | 4b2e7b6553 (earlier head 936f7ac287) | test-only: scenarios in `rt/rpc/conformance_tests.rs` against the real listener, over a Unix socket on Unix and a named pipe on Windows; mapped in §15.2 |
| #11274 endpoint verification in `zeroclaw-rpc-client` (draft, stacked on #11186) | 9a17ecf8e1 (later head 10dde94de3 not re-read) | the Unix half of §5.5: peer uid plus an owned, non-writable socket directory, checked on every dial before a credential is sent |
| #11275 `protocolVersion` spelling (draft) | 59e820e71d | checks the camelCase `protocolVersion` spelling in `initialize` instead of ignoring it (Appendix A erratum) |
| #11271 plugin webhook generation fence (draft) | db8d1474fa | §12.4's fence: route lookup and enqueue under one lock |
| #11269 gateway feature edges (draft) | 78f5bb4000 | `metrics` and `embedded-web` stop enabling the `gateway` feature (§14) |
| #11345 desktop RPC readiness (draft) | a59d3c0f42 | `health` gains `components.gateway.bound_addr?` (C.6) |
| #11373 SOP authoring routes through the core (draft) | cec2b83bd1 | `sops/decide`: `pending_quorum?`, the authenticated approver, `-32012` for approval refusals; `sops/run` refuses an unowned `execute` step before dispatch (C.6, Appendix D) |
| #11376 cron and memory routes through the core (draft) | e503e5cb8e | `memory/list` and `memory/search` `content_max_chars?`; a truthful `memory/delete`; `cron/delete` and `cron/runs` reach retained history for the administrator grant (C.6) |
| #11377 SOP run feed and version check through the core (draft) | 8b2ed4ef5c | `sops/subscribe-runs`, `sops/run-changed`, `system/version-check`; a handler error's `data` on the wire (C.6) |
| #11381 session messages, state and delete by exact row (draft) | a3b5f3920a | `session/messages`, `session/state`, `session/delete`: `session_keys?`, `SessionTargetParams`; `session/messages`: `max_bytes?` and the `entry_exceeds_max_bytes` refusal; a handler error's `data` on the wire (C.6, Appendix D; pending fix round) |
| #11382 status, logs, doctor and the event stream through the core (draft) | 967865aeae | `status {overview?, agent?}` and `StatusOverview`; `logs/query` `field_eq?`, `report_disabled?` and three result fields; `doctor/run {static_only?}` (C.6; pending fix round) |
| #11384 skills and personality through the core (draft) | 00a5c1ae52 | `skills/delete` `purge?`; `personality/list`, `get`, `put` `require_configured_agent?`; `personality/put` `expected_mtime_ms?` and `-32005`, as #11176; `personality/templates` overrides and `defaults?` (C.6; pending fix round) |
| #11319, #11320, #11322 plugin webhook ingress series (draft) | 765dc026d7, 7962184961, d15729ebff | the intended implementation of §12.3, not yet conforming. The contract revised its proposal to four of the series' choices (header encoding, `cancelled` reason, body refusal, `webhook/routes` rows); the series' result shape, `-32602` status, deadline, cap and names still differ (§12.3) |
| #11234, #11220, #11205 (draft) | acd370b321 (later head d5df4214ca not re-read), ec5fc1c287, 517c363f18 | no new methods; admission-time ownership recheck, `tools:execute` for SOP run/approve, authority-recheck foundation |

Totals across the original parity PRs, which are every row above except the gateway route ports (#11345, #11373, #11376, #11377, #11381, #11382, #11384) and the plugin webhook series: **49 methods added** (27 in #11182, 9 in #11172, 5 in #11132, 4 in #11169, 3 in #11176, 1 unique in #11185) and **27 existing methods changed**. Ten change the shape of their params, result or notification payload: `initialize`, `session/list`, `session/prompt` (through `turn_complete` and ring frames), `cron/add`, `cron/patch`, `personality/put`, `quickstart/validate`, `quickstart/apply`, `sops/decide`, `config/map-key-delete`. Seventeen change only semantics, authorization or error behaviour: `session/new`, `session/cancel`, `session/close`, `session/kill`, `session/delete`, `session/configure`, `session/messages`, `logs/subscribe`, `events/subscribe`, `subscription/cancel`, `cron/trigger`, `sops/run`, `config/set`, `config/set-many`, `config/delete`, `config/map-key-create`, `config/map-key-rename`. No head of that set adds a notification method or a `SessionUpdateEvent` variant; #11172 and #11176 each make handler `error.data` reach the wire (DEC-17).

The gateway route ports (#11345, #11373, #11376, #11377, #11381, #11382, #11384) add two methods and the first new notification method, `sops/run-changed` (#11377), and change nineteen existing methods: `health`, `memory/list`, `memory/search`, `memory/delete`, `cron/delete`, `cron/runs`, `sops/decide`, `sops/run`, `status`, `logs/query`, `doctor/run`, `session/messages`, `session/state`, `session/delete`, `skills/delete`, `personality/list`, `personality/get`, `personality/put` and `personality/templates` (C.6, Appendix D). Not all of these changes are additive (C.6).

Cross-PR facts the contract depends on (from a pairwise `git merge-tree` of the heads above; nothing compiled):

- **Agent deletion has two cascades.** #10621 (on master after the baseline) adds `agents/delete` with its own cascade and lifecycle lease, while #11172's `config/map-key-delete` on `agents` runs another agent cascade with its own authorization (Appendix D). One of them must own agent deletion when #11172 rebases; this contract does not choose.
- **Duplicate methods.** #11185 carries a pre-rebase copy of #11132, so both add `session/steer`, `session/abort`, `session/append`, `session/rename`, `session/run-once` with text-identical wire shapes but different handler behaviour (steer checks, which rows `append` may write, whether `run-once` is create-only, prompt admission rechecks). They conflict in 25-27 blocks of `dispatch.rs`. The contract's semantics for those five methods are #11132's, the owning PR; #11185 must rebase onto it.
- **Error `data`.** #11172 and #11176 each add an identical `send_rpc_error`; both merged define it twice (expected E0201) and use different `data` keys (`code` vs `error`). DEC-17 decides `reason`. #11377 and #11381 add a third and a fourth copy, keyed `reason`; one of the four must survive. #11384 sets `data` on its `-32005` refusal but has no such helper, so at its head the refusal reaches the client without `data` (`rt/rpc/dispatch.rs:2883` @00a5c1ae52).
- **Pairing revocation.** #11182 changes `PairingGuard::revoke_*` to require the config-write guard; #11149/#11176's one-argument call and six new #11172 tests then fail to compile. Three revocation-ordering designs are in flight (#11149, #11172, #11182); §8.8's core-owned pairing authority needs one.
- **SOP authorization.** #11169 and #11220 rewrite the SOP tool-ceiling check differently; if #11169's predicate (`admin || allowed_tools ∋ "*"`) wins the merge, #11220's `tools:execute` requirement is undone for `sops/run`, `sops/decide` and `sops/dispatch-event`. The contract takes #11220's rule: SOP execution needs `tools:execute` (`principal_tool_ceiling(grants).is_none()`).
- **Writer reservation.** #11185 (`RpcOutbound::reserve`) and #11234 (`reserve_frame`) add near-duplicate helpers; one should survive.
- **Protocol version.** Still 1 at every head, including #11185's `turn_lifetime` capability, which is opt-in and therefore V-1-compatible.
- **Grant vocabulary.** #11182 adds `Resource::Canvas`, the only new resource; operators' permission profiles need it to use canvas methods. #11176 adds the only new error code, `-32005`; #11384 defines the same constant (`crates/zeroclaw-api/src/jsonrpc.rs:276` @00a5c1ae52), so one definition survives the merge.
- **Handler authorization stricter than `authz()`.** `pairing/*`, `system/upgrade`, `system/restart` are admin-only in the handler; `pairing/new-code` and `pairing/revoke-all` are local-transport-only (see §5.8.1); canvas methods and the shared workspace need access to every agent; `session/run-once` also needs `sessions:create`; agent cron jobs are shared-operator only (#11149, #11176).
- **Transport tests.** None of the seven parity PRs, #11165 or #11186 tests over a real Unix socket, a named pipe or golden frames. #11273 is the daemon-side real-transport harness (§15.2); #11274 adds real dials through `zeroclaw-rpc-client`'s production path over Unix sockets and a Windows pipe (`client.rs:1124`, `:1170`, `:1210`, `:1283` @9a17ecf8e1); Windows SID verification and its required-CI proof are still to come. The gateway calls no RPC method in any head.

### 15.2 Transport and lifecycle conformance against #11001 AC2/AC3

| #11001 item | Unix socket today | Windows pipe today | Required `[proposed]` | #11273, both transports `[PR]` |
|---|---|---|---|---|
| initialization | `daemon_startup_socket_initialize_handshake_reports_readiness` (`rt/rpc/local.rs:1239`), `socket_rejects_before_initialize` (`:1310`) | `pipe_initialize_handshake` (`:3294`) | pre-initialize rejection on the pipe (CT-TR-01) | `initialize_binds_the_connection_and_advertises_every_method` (`:605`), `a_request_before_initialize_is_refused_and_initialize_still_succeeds` (`:644`): CT-TR-01 once the pipe leg runs |
| authenticated identity | unit-level only (`rt/rpc/auth.rs` tests, e.g. `peercred_routes_to_the_roster_principal`); no socket-level identity test found | none | CT-ID-01..08 on both (roster uid, native token, OIDC stub, explicit-credential rule, endpoint verification) | `a_paired_token_binds_its_principal` (`:747`), `an_unpaired_token_is_refused_without_falling_back_to_the_endpoint` (`:773`, the core-side half of ID-5), `with_a_roster_a_local_caller_it_does_not_name_is_refused` (`:852`), an OIDC principal through a mock introspection provider (`:805`); Unix only: peer uid → roster principal (`:878`), trusted daemon uid (`:906`). Not covered: endpoint verification, the gateway's explicit-credential rule |
| isolation / denial | dispatcher-level (`FORBIDDEN` cases in `rt/rpc/dispatch.rs` tests) | none | a denied method over each real transport (CT-AUTH-01) | `a_scoped_principal_is_held_to_its_grants` (`:805`): a method outside the OIDC principal's grants is refused with `FORBIDDEN` over each transport, the pipe included; roster grants gate each call (`:878`, Unix only) |
| compatible / incompatible versions | **no test exercises `-32011`** anywhere on master (`git grep` over runtime, zerocode, `tests/`) | none | CT-VER-01..04: v1 accept, disjoint refuse with structured data, unknown `session/update` type ignored by the typed client, missing optional method → route 503 | omitted version → 1 (`:662`); camelCase `protocolVersion` ignored → 1 (`:681`; #11275 proposes checking that spelling instead); `an_unsupported_protocol_version_is_refused_and_binds_nothing` (`:704`), the first `-32011` test: it asserts the message names both versions, not structured `data` (CT-VER-02 stays open) |
| endpoint discovery | `socket_path_length_check_matches_the_platform_bind_limit` (`:1479`), `bind_rejects_an_overlong_socket_path_before_creating_the_lock` (`:1516`); #11186 `endpoint.rs` computes paths only | none | CT-DISC-01..04 (§4.3) | `each_data_dir_gets_its_own_endpoint_and_the_daemon_reports_it` (`:927`) |
| limits and deadlines | oversized frame (`:2721`), stalled reader (`:2775`), connection ceiling (`:2858`), initialize deadline (`:2978`, `:3009`), frame deadline (`:3050`), idle allowed (`:3108`), write timeout (`:2619`) | oversized frame (`:3181`), connection ceiling (`:3234`) | the deadline cases on the pipe (CT-TR-02) | none (kept beside the listener in `local.rs`) |
| cancellation | dispatcher-level `session/cancel` tests; `cancel_closes_client_connection_despite_parked_writer_sender` (`:1880`) | none | owner-principal cancel from a second connection (TL-3, CT-TL-02) | `cancelling_a_running_turn_ends_it_as_cancelled` (`:1077`), same connection |
| disconnect / reconnect | `completed_prompt_keeps_the_local_connection_serving` (`:1954`), reload drain (`:2125`) | none | CT-TL-01 (kill the gateway mid-turn), CT-TL-03 (reattach and replay), CT-TL-04 (reload while detached) | `closing_the_prompting_connection_ends_its_turn_but_not_its_session` (`:1135`): pins master's connection-owned default, which TL-1 keeps for clients that do not opt in |
| terminal outcomes | dispatcher-level `turn_complete` tests | none | per outcome over each transport | completed with updates streamed before completion (`:976`); failed once on a provider error (`:1012`) and on an unknown session (`:1039`); cancelled (`:1077`) |
| bounded buffering | subscription hub tests in `rt/rpc/dispatch.rs` (`events_subscribe_resumes_exactly_the_missing_range`, `a_future_since_seq_in_the_same_epoch_is_refused`) | none | slow viewer does not stall a turn or another session on the same connection (CT-TL-05) | `an_expired_replay_cursor_is_told_where_to_resume_then_streams_live` (`:1217`), `a_live_subscriber_that_stops_reading_is_told_what_it_lost` (`:1276`) |
| viewer vs agent lifetime | n/a on master (connection-owned) | n/a | #11185 tests plus TL-4's matrix | none |

In the #11273 column, bare line numbers are in `rt/rpc/conformance_tests.rs` @4b2e7b6553 (the head after review changes to 936f7ac287).

Every #11186 client test runs over `tokio::io::duplex` (@337d0a186f), and no gateway code calls `connect_local` yet; #11274 adds its first real-socket tests (below). The contract requires the conformance harness to run the same cases over (a) a real Unix socket, (b) a real named pipe on the Windows runner, and (c) the in-process duplex while it exists. #11273 (`rt/rpc/conformance_tests.rs` @4b2e7b6553, draft) provides (a) and (b) on the daemon side, speaking raw NDJSON rather than going through `zeroclaw-rpc-client`. On the client side, #11186's own tests use only the in-memory duplex (@337d0a186f). #11274 exercises the production dial path `RpcClient::connect_local` over real endpoints (`crates/zeroclaw-rpc-client/src/client.rs` @9a17ecf8e1): an endpoint owned by another account never receives the credential (`:1124`), a same-account endpoint in a private directory does (`:1170`), every dial verifies the endpoint again (`:1210`), and on Windows a credential dial the client cannot verify is refused over a real pipe (`:1283`). Still unfinished: Windows SID verification itself (the Windows leg only fails closed), its proof in a required CI job, and an end-to-end client-to-daemon run. Its pipe leg runs only in the advisory Windows job, which is not part of the required gate: a named-pipe regression would not block a merge until that leg is required or mirrored in a required job (#11001 AC2).

### 15.3 Gateway route → method coverage

Source: a route-by-route read of `crates/zeroclaw-gateway/src` at f0ae8c8bd8 (174 method+path rows: 152 production `.route(` registrations, the router fallback and the prefix redirect, with `get(..).patch(..)` counted per method). "RPC today" is the class at master: **yes** = a method performs the same operation on the same state and returns the same information; **partial** = the method lacks an input, behaviour or output field, acts on different state, or needs several calls; **none** = no method. No route calls the RPC dispatcher or a client today; each "yes" is a second handler reaching the same runtime helper.

| Closed by | Rows |
|---|---:|
| master, exact | 36 |
| master, partial (the remaining difference is in the Gap column; several have a PR that claims to close it) | 41 |
| an open PR only | 54 |
| a method this contract proposes (§8.7, §8.8, §8.12, §8.13, §12) | 12 |
| no method and no PR yet | 0 |
| waits on D7 (vendor ingress, 9) or D10 (ACP, nodes, WebAuthn, OIDC relays, 13) | 22 |
| gateway-local (static assets, OpenAPI, docs, redirect, 405 stub, `/admin/shutdown` of the gateway itself, `/hooks/claude-code`) | 9 |

| # | Route | RPC today | Target after the split | Closed by | Remaining gap |
|---:|---|---|---|---|---|
| 1 | POST `/webhook` | partial | `ingress/webhook` | proposed §12.5 | truth table, SOP-first, idempotency (acceptance point, DEC-24), stream correlation (DEC-23) and the turn move to the core; D3 owner |
| 2 | WS `/ws/chat` | partial | `session/new`, `session/prompt`, `session/steer`, `session/attach`, `session/approve`, `session/cancel`/`abort`; SOP approval frames → `sops/decide` | PR #11132, #11185; master | frame-protocol translation in the gateway; steering and attach are PR-only; D4 for approvals with no viewer |
| 3 | GET `/api/sessions` | yes | `session/list` | master |  |
| 4 | GET `/api/sessions/running` | none | `session/list {running: true}` | PR #11132 |  |
| 5 | GET `/api/sessions/{id}/messages` | partial | `session/messages` (`session_keys`, `max_bytes`) | master (partial); PR #11381 | the id fallback can read another row (`rpc_<id>` before `gw_<id>`), and a transcript larger than one frame fails; per-message `created_at` comes from #11331, which #11351 and #11381 carry. `[pending fix round]` |
| 6 | POST `/api/sessions/{id}/messages` | none | `session/append` | PR #11132 |  |
| 7 | DELETE `/api/sessions/{id}` | partial | `session/delete` (`session_keys`) | master (partial); PR #11381 | the id fallback can delete another row (`session_keys` closes it); closes fully once gateway-run turns move to the core (no gateway cancel tokens left). `[pending fix round]`: a delete through `zeroclaw-gw` does not yet settle a turn the in-process gateway owns |
| 8 | PUT `/api/sessions/{id}` | none | `session/rename` | PR #11132 |  |
| 9 | GET `/api/sessions/{id}/state` | partial | `session/state` (`session_keys`) | master (partial); PR #11381 | the id fallback can answer another row's state (`rpc_<id>` before `gw_<id>`), found by #11381. `[pending fix round]` |
| 10 | POST `/api/sessions/{id}/abort` | partial | `session/abort` | PR #11132/#11185 | operator path; owner cancel must be principal-scoped (TL-3) |
| 11 | GET `/api/memory` | partial | `memory/list`, `memory/search` | master (partial); PR #11176, #11376 | category filter on search, preview length (`content_max_chars`, #11376), plane default |
| 12 | POST `/api/memory` | partial | `memory/store` | master (partial); PR #11176 | default category differs (`Core` vs `Custom("user")`) |
| 13 | DELETE `/api/memory/{key}` | partial | `memory/delete` | master (partial); PR #11176, #11376 | RPC always answers `deleted: true` (#11376 reports the removal) |
| 14 | GET `/api/cron` | yes | `cron/list` | master |  |
| 15 | POST `/api/cron` | partial | `cron/add` | master (partial); PR #11149, #11176 | RPC pre-approves shell jobs and ignores agent-job fields until those PRs land |
| 16 | GET `/api/cron/settings` | yes | `cron/settings` | master |  |
| 17 | PATCH `/api/cron/settings` | partial | `config/set-many` on `scheduler.*`, then `cron/settings` | master (partial) | two calls (§8.5); today this route writes config after only the pairing check, outside the principal gate, which the mapping closes |
| 18 | DELETE `/api/cron/{id}` | partial | `cron/delete` | master (partial); PR #11376 | a job whose row is gone but whose run history remains: the route removes the history, `cron/delete` refuses the id (#11376 serves it for the administrator grant) |
| 19 | PATCH `/api/cron/{id}` | partial | `cron/patch` | master (partial); PR #11149, #11176 | `enabled`, `uses_memory`, `shell_output_format` missing |
| 20 | GET `/api/cron/{id}/runs` | partial | `cron/runs` | master (partial); PR #11176, #11376 | retained-history fallback (#11376, for the administrator grant) and clamp |
| 21 | POST `/api/cron/{id}/run` | yes | `cron/trigger` | master | long-running: must not run on the service or ingress connection (§3.4) |
| 22 | GET `/api/config` | yes | `config/get` | master |  |
| 23 | PATCH `/api/config` | partial | `config/set-many` | master (partial); PR #11172 | `test`/`remove`/`comment` ops, drift guard, per-op warnings |
| 24 | OPTIONS `/api/config` | none | `config/schema` | proposed §8.7 | public: served through the service allowlist (DEC-25) |
| 25 | GET `/api/config/prop` | partial | `config/get {prop}` | master (partial); PR #11172 | `warnings`; secret paths reduced to `{populated}` |
| 26 | PUT `/api/config/prop` | partial | `config/set` | master (partial); PR #11172 | `comment`, `warnings` |
| 27 | DELETE `/api/config/prop` | partial | `config/delete` | master (partial); PR #11172 | `warnings` |
| 28 | OPTIONS `/api/config/prop` | none | `config/schema {path}` | proposed §8.7 | resolves against live config; proposed to require the caller's credential (D13) |
| 29 | GET `/api/config/list` | partial | `config/list` + `config/drift` | master (partial); PR #11172 |  |
| 30 | GET `/api/config/drift` | none | `config/drift` | PR #11172 |  |
| 31 | GET `/api/config/reload-status` | none | `config/reload-status` | PR #11172 | the core must own `pending_reload` (today a gateway-local flag) |
| 32 | GET `/api/config/templates` | yes | `config/templates` | master |  |
| 33 | GET `/api/config/map-keys` | yes | `config/map-keys` | master |  |
| 34 | GET `/api/config/resolve-alias-source` | yes | `config/resolve-alias-source` | master |  |
| 35 | POST `/api/config/map-key` | partial | `config/map-key-create` | master (partial); PR #11172 | skill-bundle directory creation |
| 36 | DELETE `/api/config/map-key` | partial | `config/map-key-delete` (+ `config/delete-plan`) | master (partial); PR #11172 | agent owned-state cascade, workspace archive, live-ACP refusal |
| 37 | POST `/api/config/rename-map-key` | yes | `config/map-key-rename` | master |  |
| 38 | POST `/api/config/model-providers/{type}/{alias}/refresh-context-window` | none | `providers/refresh-context-window` | PR #11172 |  |
| 39 | GET `/api/config/delete-plan` | none | `config/delete-plan` | PR #11172 |  |
| 40 | GET `/api/config/status` | partial | `config/status` | master (partial); PR #11172 | different derivation |
| 41 | GET `/api/config/agent-options` | partial | `config/agent-options` | PR #11172 |  |
| 42 | GET `/api/config/sections` | partial | `config/sections` | master (partial); PR #11172 | `ready`, `completed` derivation |
| 43 | GET `/api/config/sections/{section}` | none | `config/section-picker` | PR #11172 |  |
| 44 | POST `/api/config/sections/{section}/items/{key}` | none | `config/section-select` | PR #11172 |  |
| 45 | POST `/api/config/init` | none | `config/init` | PR #11172 |  |
| 46 | POST `/api/config/migrate` | none | `config/migrate` | PR #11172 |  |
| 47 | GET `/api/quickstart/state` | yes | `quickstart/state` | master |  |
| 48 | POST `/api/quickstart/fields` | yes | `quickstart/fields` | master |  |
| 49 | POST `/api/quickstart/validate` | yes | `quickstart/validate` | master |  |
| 50 | POST `/api/quickstart/apply` | yes | `quickstart/apply` | master | triggers a daemon reload, which drops every gateway connection (§11.1) |
| 51 | POST `/api/quickstart/dismiss` | yes | `quickstart/dismiss` | master |  |
| 52 | GET `/api/config/catalog` | yes | `config/catalog` | master |  |
| 53 | GET `/api/config/catalog/models` | partial | `config/catalog-models` | master (partial) | no `alias` param |
| 54 | GET `/api/tools` | none | `tools/list` | PR #11182 | removes the gateway's boot-time tool-registry build |
| 55 | POST `/api/tools/param-options` | yes | `tools/param-options` | master |  |
| 56 | GET `/api/integrations` | none | `integrations/list` | PR #11182 |  |
| 57 | GET `/api/integrations/settings` | none | `integrations/list` | PR #11182 (partial) | the per-entry enabled/settings view is not claimed by #11182 |
| 58 | GET `/api/cli-tools` | none | `tools/cli-discover` | PR #11182 |  |
| 59 | GET `/admin/sop/pending` | partial | `sops/runs` | master (partial) | admin gate becomes a core grant; client-side filter |
| 60 | GET `/admin/sop/logs` | partial | `logs/query {sop_run_id}` | master (partial) | `persistence_enabled`, `daemon_started_at`, `attribution_keys` |
| 61 | POST `/admin/sop/approve` | partial | `sops/decide` | master (partial); PR #11169, #11373 | approval principal is `cli(tui_id)` on master; needs the caller's principal |
| 62 | POST `/admin/sop/deny` | partial | `sops/decide` | master (partial); PR #11169, #11373 | as row 61 |
| 63 | POST `/sop/{*rest}` | none | `ingress/sop` | proposed §12.5 | #11169 `sops/dispatch-event` is not the gateway path (§12.5) |
| 64 | GET `/api/sops` | yes | `sops/list` | master |  |
| 65 | POST `/api/sops` | yes | `sops/create` | master |  |
| 66 | PUT `/api/sops/{name}` | yes | `sops/save` | master |  |
| 67 | DELETE `/api/sops/{name}` | yes | `sops/delete` | master |  |
| 68 | GET `/api/sops/{name}/graph` | yes | `sops/graph` | master |  |
| 69 | POST `/api/sops/{name}/run` | partial | `sops/run` | master (partial); PR #11373 | headless-ownership pre-check (#11373) |
| 70 | POST `/api/sops/{name}/rename` | yes | `sops/rename` | master |  |
| 71 | GET `/api/sops/runs` | yes | `sops/runs` | master |  |
| 72 | GET `/api/sops/{name}/full` | yes | `sops/get` | master |  |
| 73 | POST `/api/sops/wire-draft` | yes | `sops/wire-draft` | master |  |
| 74 | POST `/api/sops/graph-draft` | yes | `sops/graph-draft` | master |  |
| 75 | GET `/api/sops/trigger-sources` | yes | `sops/trigger-sources` | master |  |
| 76 | GET `/api/sops/decision-models` | none | `sops/decision-models` | PR #11169 |  |
| 77 | GET `/api/sops/graph-legend` | none | `sops/graph-legend` | PR #11169 |  |
| 78 | GET `/api/sops/{name}/runs/{run_id}/overlay` | yes | `sops/run-overlay` | master |  |
| 79 | POST `/api/sops/{name}/runs/{run_id}/decide` | partial | `sops/decide` | master (partial); PR #11169, #11373 | approval principal; 202 pending-quorum answer (`pending_quorum`, #11373) |
| 80 | POST `/api/sops/{name}/runs/{run_id}/cancel` | none | `sops/cancel` | PR #11169 |  |
| 81 | WS `/ws/sops/runs` | partial | `sops/subscribe-runs` + `sops/run-changed` | PR #11377 | frame translation in the gateway (#11377) |
| 82 | GET `/api/agents/{alias}/skills` | none | `skills/effective` | PR #11176 |  |
| 83 | GET `/api/skills/bundles` | yes | `skills/bundles` | master |  |
| 84 | GET `/api/skills/slash-option-kinds` | none | `skills/slash-option-kinds` | PR #11176 |  |
| 85 | GET `/api/skills/bundles/{alias}/skills` | yes | `skills/list` | master |  |
| 86 | POST `/api/skills/bundles/{alias}/skills` | none | `skills/create` | PR #11176 |  |
| 87 | GET `/api/skills/bundles/{alias}/skills/{name}` | yes | `skills/read` | master |  |
| 88 | PUT `/api/skills/bundles/{alias}/skills/{name}` | yes | `skills/write` | master |  |
| 89 | DELETE `/api/skills/bundles/{alias}/skills/{name}` | partial | `skills/delete` (`purge`) | master (partial); PR #11384 | purge mode (#11384; #11176 does not add it); a missing skill's failure status still differs `[pending fix round]` |
| 90 | GET `/api/personality` | partial | `personality/list` (`require_configured_agent`) | master (partial); PR #11384 | the route refuses an agent `[agents]` does not configure, which the core lists for the operator, found by #11384 |
| 91 | GET `/api/personality/templates` | partial | `personality/templates` (overrides, `defaults`) | master (partial); PR #11384 | template override params and the editor's defaults (#11384; #11176 adds none of them) |
| 92 | GET `/api/personality/{filename}` | partial | `personality/get` (`require_configured_agent`) | master (partial); PR #11384 | as row 90 |
| 93 | PUT `/api/personality/{filename}` | partial | `personality/put` (`expected_mtime_ms`, `require_configured_agent`) | master (partial); PR #11176, #11384 | `expected_mtime` conflict guard (both PRs; #11384 takes #11176's over) and the unconfigured-agent refusal; the 409's current state `[pending fix round]` |
| 94 | GET `/api/browse` | partial | `fs/list_dir` / `workspace/list` | master (partial); PR #11182 | root differs (install `shared/` vs absolute path) |
| 95 | POST `/api/browse/mkdir` | none | `fs/mkdir` | PR #11182 |  |
| 96 | DELETE `/api/browse/rmdir` | none | `fs/rmdir` | PR #11182 |  |
| 97 | GET `/api/agents/{alias}/workspace/list` | partial | `workspace/list` | PR #11182 |  |
| 98 | GET `/api/agents/{alias}/workspace/read` | none | `fs/read` | PR #11182 |  |
| 99 | DELETE `/api/agents/{alias}/workspace/path` | none | `fs/delete` | PR #11182 |  |
| 100 | POST `/api/agents/{alias}/workspace/move` | none | `fs/move` | PR #11182 |  |
| 101 | POST `/api/agents/{alias}/workspace/mkdir` | none | `fs/mkdir` | PR #11182 |  |
| 102 | POST `/api/upload` | partial | `file/attach` or `file/upload/*` | master (partial) | chunked upload is refused on WSS and on #11186's in-process transport; the route needs the real socket or an explicit grant for the gateway transport; image-only check and marker differ |
| 103 | GET `/api/logs` | partial | `logs/query` (`field_eq`, `report_disabled`) | master (partial); PR #11382 | attribution filters and persistence metadata (#11382) `[pending fix round]` |
| 104 | SSE `/api/events` | partial | `events/subscribe` / `logs/subscribe` | master (partial); PR #11382 | #11382 serves it through `logs/subscribe` with the route's public-frame filter at the edge; unscoped principals only; pairing frames never delivered. `[pending fix round]`: cleanup of the subscription while its subscribe call is in flight |
| 105 | GET `/api/events/history` | yes | `events/history` | master |  |
| 106 | GET `/api/cost` | partial | `cost/query` | master (partial) | bounds semantics; errors without a tracker |
| 107 | GET `/api/status` | partial | `status {overview, agent}` (+ nodes, D10) | master (partial); PR #11382 | payload differs (#11382 adds `overview`); nodes wait on D10. `[pending fix round]` |
| 108 | GET `/api/tuis` | yes | `tui/list` | master |  |
| 109 | GET `/health` | partial | gateway liveness + `health` + `PairingPosture` | master (partial) | public; the pairing fields come from `PairingPosture` (§8.13) |
| 110 | GET `/metrics` | none | `metrics/scrape` | PR #11182 | must be in the service allowlist; `/metrics` stays unauthenticated at the edge or becomes authenticated (edge decision) |
| 111 | POST `/admin/shutdown` | none | gateway-local | gateway-local | `/admin/shutdown` stops the gateway process only |
| 112 | POST `/admin/reload` | yes | `config/reload` | master | drops every gateway connection (§11.1) |
| 113 | GET `/api/health` | yes | `health` | master |  |
| 114 | GET `/api/version/check` | none | `system/version-check` | PR #11377 | the core checks its own version |
| 115 | POST `/api/version/upgrade` | none | `system/upgrade` | PR #11182 | two-binary upgrade semantics are G5's |
| 116 | GET `/api/version/upgrade/status` | none | `system/upgrade-status` | PR #11182 |  |
| 117 | GET `/api/doctor` | partial | `doctor/run {static_only}` | master (partial); PR #11382 | result set differs (#11382 adds `static_only`). `[pending fix round]`: an older core ignores it and runs live probes |
| 118 | POST `/api/doctor` | partial | `doctor/run {static_only}` | master (partial); PR #11382 | as row 117 |
| 119 | GET `/admin/paircode` | none | `pairing/code` (CLI over the core socket) | proposed §8.8 | the route leaves `zeroclaw-gw`: `zeroclaw gateway get-paircode` calls the core instead (D13) |
| 120 | POST `/admin/paircode/new` | none | `pairing/new-code` + `pairing/revoke`/`revoke-all` | PR #11182 | admin-token file and loopback gate become core grants |
| 121 | POST `/pair` | none | `pairing/redeem` | proposed §8.8 | D1 |
| 122 | GET `/pair/code` | none | `PairingPosture.require_pairing` | proposed §8.13 | public; no code is ever returned |
| 123 | POST `/api/pairing/initiate` | none | `pairing/new-code` | PR #11182 | narrower than today: needs `System:Create`, not any paired bearer |
| 124 | POST `/api/pair` | none | `pairing/redeem` | proposed §8.8 | D1 |
| 125 | GET `/api/devices` | none | `pairing/list` | PR #11182 |  |
| 126 | POST `/api/devices/me/capabilities` | none | `pairing/device-capabilities` | proposed §8.8 | the device registry moves to the core with pairing |
| 127 | DELETE `/api/devices/{id}` | none | `pairing/revoke` | PR #11182 |  |
| 128 | POST `/api/devices/{id}/token/rotate` | none | `pairing/revoke` + `pairing/new-code` | PR #11182 (partial) | atomic rotate |
| 129 | GET `/api/oidc/providers` | none | OIDC relay | OPEN D10 |  |
| 130 | POST `/api/oidc/{alias}/device/start` | none | OIDC relay | OPEN D10 |  |
| 131 | POST `/api/oidc/{alias}/device/poll` | none | OIDC relay | OPEN D10 |  |
| 132 | GET `/oidc/login/{alias}` | none | OIDC relay | OPEN D10 |  |
| 133 | GET `/oidc/callback` | none | OIDC relay | OPEN D10 |  |
| 134 | POST `/api/webauthn/register/start` | none | WebAuthn | OPEN D10 |  |
| 135 | POST `/api/webauthn/register/finish` | none | WebAuthn | OPEN D10 |  |
| 136 | POST `/api/webauthn/auth/start` | none | WebAuthn | OPEN D10 |  |
| 137 | POST `/api/webauthn/auth/finish` | none | WebAuthn | OPEN D10 |  |
| 138 | GET `/api/webauthn/credentials` | none | WebAuthn | OPEN D10 |  |
| 139 | DELETE `/api/webauthn/credentials/{id}` | none | WebAuthn | OPEN D10 |  |
| 140 | GET `/whatsapp` | none | vendor ingress | OPEN D7 |  |
| 141 | POST `/whatsapp` | none | vendor ingress | OPEN D7 |  |
| 142 | GET `/whatsapp/{alias}` | none | vendor ingress | OPEN D7 |  |
| 143 | POST `/whatsapp/{alias}` | none | vendor ingress | OPEN D7 |  |
| 144 | POST `/linq` | none | vendor ingress | OPEN D7 |  |
| 145 | POST `/linq/{alias}` | none | vendor ingress | OPEN D7 |  |
| 146 | POST `/nextcloud-talk` | none | vendor ingress | OPEN D7 |  |
| 147 | POST `/nextcloud-talk/{alias}` | none | vendor ingress | OPEN D7 |  |
| 148 | POST `/webhook/gmail` | none | vendor ingress | OPEN D7 | Gmail no-listener bug (Appendix B) |
| 149 | GET `/api/channels` | none | `channels/list` | PR #11182 |  |
| 150 | POST `/api/channels/{channel}/relink` | none | `channels/relink` | PR #11182 |  |
| 151 | POST `/api/channels/bind` | none | `channels/bind` | PR #11182 |  |
| 152 | GET `/api/plugins` | none | `plugins/list` | PR #11182 |  |
| 153 | GET `/plugin/{path}` | none | `webhook/deliver` | proposed §12.3 | D1 for the calling identity |
| 154 | POST `/plugin/{path}` | none | `webhook/deliver` | proposed §12.3 | as row 153 |
| 155 | HEAD + any other method `/plugin/{path}` | none | gateway-local 405 | gateway-local |  |
| 156 | GET `/.well-known/agents-card.json` | none | `a2a/identity` | PR #11182 | public: through the service allowlist; the handler enforces A2A publication (DEC-25) |
| 157 | GET `/a2a/.well-known/agents-card.json` | none | `a2a/identity` | PR #11182 | public: through the service allowlist; the handler enforces A2A publication (DEC-25) |
| 158 | GET `/a2a/{alias}/.well-known/agent-card.json` | none | `a2a/identity` | PR #11182 | public: through the service allowlist; the handler enforces A2A publication (DEC-25) |
| 159 | POST `/a2a/{alias}` | partial | `session/run-once` | PR #11132 | published-alias gate and A2A wire mapping stay in the gateway as translation only; authorization in the core |
| 160 | WS `/acp` | partial | ACP | OPEN D10 |  |
| 161 | WS `/ws/nodes` | none | nodes | OPEN D10 |  |
| 162 | GET `/api/canvas` | none | `canvas/list` | PR #11182 |  |
| 163 | GET `/api/canvas/{id}` | none | `canvas/get` | PR #11182 |  |
| 164 | POST `/api/canvas/{id}` | none | `canvas/render` | PR #11182 |  |
| 165 | DELETE `/api/canvas/{id}` | none | `canvas/clear` | PR #11182 |  |
| 166 | GET `/api/canvas/{id}/history` | none | `canvas/history` | PR #11182 |  |
| 167 | WS `/ws/canvas/{id}` | none | `canvas/subscribe` + `canvas/frame` | proposed §8.12 | frame translation in the gateway |
| 168 | GET `/_app/` | none | gateway-local | gateway-local |  |
| 169 | GET `/_app/{*path}` | none | gateway-local | gateway-local |  |
| 170 | GET (router fallback: any unmatched path) | none | gateway-local | gateway-local |  |
| 171 | GET {path_prefix}/ (only when `gateway.path_prefix` is set) | none | gateway-local | gateway-local |  |
| 172 | GET `/api/openapi.json` | none | gateway-local | gateway-local |  |
| 173 | GET `/api/docs` | none | gateway-local | gateway-local |  |
| 174 | POST `/hooks/claude-code` | none | gateway-local | gateway-local | logs only |

## 16. Conformance test index

Test IDs used above, grouped. "Exists" names a test at f0ae8c8bd8; everything else is `[proposed]` and owned by the named work item.

| Group | IDs | Owner |
|---|---|---|
| Transport | CT-TR-01 pipe pre-initialize rejection; CT-TR-02 pipe deadlines | #11001 |
| Discovery | CT-DISC-01..04 | #11001, #11186, G3 |
| Identity | CT-ID-01 roster uid over the socket; CT-ID-02 OpenRPC scan for on-behalf-of fields; CT-ID-03 native token liveness after revoke over a pooled connection; CT-ID-04 OIDC re-initialize on policy change; CT-ID-05 gateway sends an explicit credential on every connection; CT-ID-06/07 endpoint verification (Unix/Windows); CT-ID-08 pipe DACL on every instance; CT-ID-09 no credential-less `initialize` from any failure, probe or reconnect path; CT-ID-10/11 the separate-account layout on Linux and Windows (§5.5.1) | #11001, #11274, G3 |
| Handshake | CT-HS-01 snake_case field spellings (exists at #11165: `initialize_params_accepts_snake_case_field_names`); CT-HS-02 failed re-initialize keeps the old binding; CT-HS-03 gateway sends no `env` | #11001, G3 |
| Versioning | CT-VER-01..04 | #11001 |
| Authorization | CT-AUTH-04 local-only operations from a separate-account client; CT-AUTH-01 denial over each transport; CT-AUTH-02 service allowlist: every method outside it is refused on a service connection, and a sequence of allowed calls cannot reach operator authority (no code read, no redeem-then-reconnect escalation); CT-AUTH-03 transitive dependency ratchet over the `zeroclaw-gw` graph plus the symbol grep (AUTH-3); CT-AUTH-05 credential-free public A2A card discovery with pairing on, through a service principal restricted to its allowlist (DEC-25) | #11001, G3 |
| Handoff | CT-HAND-01 no test credential appears in gateway logs; CT-HAND-02 no `#[secret]` value in any config method result | G3 |
| Subscriptions | CT-SUB-01, CT-SUB-02: own-connection cancellation without `Logs:Read`, and no cross-connection cancellation (DEC-31) | #11001 |
| Turn lifetime | CT-TL-01..05; CT-TL-06 targeted-cancel replay against a new core, a pre-DEC-22 core and across a core restart leaves the later turn running (DEC-22) | #11185, G3 |
| Ingress | CT-ING-01..15 (§12.10): 06 byte-for-byte HTTP parity, 10–12 and 14 stream correlation and cancellation (DEC-23), 13 the acceptance point (DEC-24), 15 SOP no-match | #11003, G3 |
| Gateway control | CT-GW-01..03 pairing posture at startup, after a config write and after the reload (§8.13, DEC-26) | G3 |
| Pairing-disabled mode (only if D2-A) | CT-D2-01..03 (§5.4, DEC-30) | G3 |
| Schema | CT-SCH-01 every gateway-used method is `T/T`; CT-SCH-02 each method's actual response validates against its declared result schema and against Appendix C; CT-SCH-03 every error code a run observes is in its method's Appendix E list | #11165 follow-up, SCH-6 for PR methods |
| Errors | CT-ERR-01 every custom error carries `data.reason`; CT-ERR-02 the client surfaces null-id refusal frames | #11001, #11186 |
| Lifecycle | CT-LIFE-01 kill the gateway mid-turn; CT-LIFE-02 reload drops and the gateway reconnects with a new epoch; CT-LIFE-03 child mode passes the endpoint and pid | G3 |
| Artifact | CT-ART-01 feature-resolved dependency graph of the built `zeroclaw` binary has no HTTP server framework beyond the D8 exceptions; no dashboard assets | G3 |

## 17. Open decisions and scope

### 17.0 Decision register

Status vocabulary: **proposed** = written here, not yet reviewed to CLEAR; **reviewed** = both independent reviewers returned CLEAR on the same revision; **accepted** = the maintainers accepted it (in the ADR PR or on #8691/#11000); **open** = needs a maintainer choice. A CLEAR review makes a decision *reviewed*, not *accepted*: an implementation PR may rely only on accepted decisions (#11000 AC6). The document was reviewed adversarially over four revisions by two independent reviewers, both of whom cleared the fourth, so every DEC here is *reviewed*; none is *accepted* yet.

| ID | Decision | Section | Status | Enforced by (code, PR or test) |
|---|---|---|---|---|
| DEC-01 | Reuse the local socket/pipe listener; no second server, no HTTP in the core | §1, §3 | proposed | existing `rt/rpc/local.rs`; CT-ART-01 |
| DEC-02 | Three documents: this framing/semantics spec, OpenRPC for schemas, OpenAPI 3.1 for the gateway's HTTP only | §2 | proposed | #11165 drift check; §15.3 table |
| DEC-03 | Authority only in the core; edge policy may narrow, never grant | §5.7 | proposed | CT-AUTH-01..03; dependency ratchet at G3 |
| DEC-04 | One credential per connection; no on-behalf-of field; re-present on every connect | §5.3 ID-1..4 | proposed | CT-ID-02, CT-ID-03; gateway pool (G3) |
| DEC-05 | No absent/blank/invalid/unsupported credential becomes a tokenless core connection; public routes use only their allowlisted method on the service connection (DEC-25) | §5.3 ID-5 | proposed | CT-ID-05, CT-ID-09; core-side Windows SID check (#11001) |
| DEC-06 | Verify the endpoint's OS identity before any credential, on every dial; two specified trust boundaries (same account, separate accounts), which of them v0.9.0 supports is D11 | §5.5 | proposed | `zeroclaw-rpc-client` dial path (#11274); pipe DACL (#11001); CT-ID-06..08 |
| DEC-07 | One discovery algorithm in `zeroclaw-rpc-client`; launcher-passed endpoint; SHA-256 pipe name | §4.2 | proposed | #11001; CT-DISC-01..04 |
| DEC-08 | Protocol 1 additive-only; capability detection; mandatory vs route methods; structured `-32011` | §7.2 | proposed | CT-VER-01..04 |
| DEC-09 | Session-owned turns by opt-in capability; lifecycle matrix; explicit principal-scoped cancel; no automatic retry of `N` methods | §9.3, §8.0 | proposed | #11185 + follow-ups; CT-TL-01..05 |
| DEC-10 | `webhook/deliver` with the closed outcome enum and HTTP mapping; spawned with a per-connection cap; per-delivery `ingress/cancel` | §12.3 | proposed | #11003; CT-ING-01, -04, -05 |
| DEC-11 | Registry sends fenced to the live generation; unload/replacement take effect at the next channel-supervisor generation | §12.4 | proposed | #11271 (fence); CT-ING-03 |
| DEC-12 | Webhook idempotency core-owned at process scope, landing with (or after) instance-scoped plugin keys; plugin deliveries at-least-once to the enqueue point, generic ingress "admitted once" (DEC-24); TTL-bounded, lost on core restart | §12.6 | proposed | core-owned store PR; CT-ING-02, -08, -13; no config move |
| DEC-13 | `/webhook` and `/sop/*` checks owned by `ingress/webhook` / `ingress/sop` in the core; `webhook_secret` verified in the core; today's HTTP bodies kept byte for byte, including the chat `{response, model}` (the core supplies `model`) and `/sop/*`'s `no_matching_sop` 404, which never falls through to chat | §12.5, §12.7 | proposed | G3 ingress port; CT-ING-06, -07, -15; no config move |
| DEC-14 | One owner per piece of state; the gateway holds transport state only | §5.8 | proposed | G3 route ports; CT-AUTH-03 |
| DEC-15 | Every gateway-used method fully typed in `zeroclaw-rpc-proto`; `session/prompt` result corrected; `-32004` listed; core→client requests described; per-method errors | §8.0 SCH-1..5 | proposed | #11165 follow-up; CT-SCH-01, -02 |
| DEC-16 | If D1-A: the service profile is a method allowlist checked in addition to the resource/verb grant | §8.13 | proposed (conditional on D1) | CT-AUTH-02 |
| DEC-17 | Error `data`: when present, `data.reason` is the stable machine code; #11172's `code` and #11176's `error` renamed before merge | §8.16 | proposed | CT-ERR-01 |
| DEC-18 | "Local transport only" checks that mean "operator at this machine" become same-OS-account checks | §5.8.1 | proposed | CT-AUTH-04 |
| DEC-19 | SOP execution over RPC requires `tools:execute` (#11220's predicate survives the #11169 merge) | §15.1 | proposed | #11220 tests |
| DEC-20 | The five duplicated session methods take #11132's semantics; #11185 rebases onto it | §15.1 | proposed | merge order |
| DEC-21 | Config keys keep today's names; §13.1 records one canonical owner per key; core-owned keys are never read by the gateway; only the inert `[gateway.pairing_dashboard]` retires at V4 | §13.1 | proposed | G3 dependency ratchet (the gateway crate stops reading core-owned keys); V4 schema work |
| DEC-22 | Every turn has a core-assigned random-UUID `turn_id` announced by `turn_started`; targeted cancellation is the distinct, capability-advertised `session/cancel-turn` / `session/abort-turn` (`W`); `session/cancel`/`session/abort` and every method that targets a session only by id stay `N`; `initialize` reports `daemon_instance_id` and a restart voids pending retries of transient ids | §6.1, §8.0, §9.1, §9.3, §10.2 | proposed | runtime change; CT-TL-06 (new core, pre-DEC-22 core, core restart) |
| DEC-23 | Ingress calls carry a gateway-chosen `delivery_id` that tags their stream events and is the target of `ingress/cancel`; an ingress chat turn is bound to its delivery | §12.2 ING-4, §12.3, §12.5 | proposed | G3 ingress port; CT-ING-10..12, CT-ING-14 |
| DEC-24 | Generic ingress idempotency reserves after verification, commits at admission, rolls back every refusal before it; "admitted once" | §12.5, §12.6 | proposed | core-owned store PR; CT-ING-13 |
| DEC-25 | Public routes (health, metrics, A2A cards, config schema, pairing redemption, pairing posture) are served only through the service connection, one allowlisted method each; `a2a/identity` enforces A2A publication for the service principal | §5.3, §8.13 | proposed | allowlist in the core's gate; CT-AUTH-05 |
| DEC-26 | `PairingPosture`: a read-only projection of the core's pairing authority for presentation only; never authority | §8.13 | proposed | CT-GW-01..03 |
| DEC-27 | Separate-account layout: an IPC directory outside the data directory, group-accessible socket and gateway material, listener that applies it on every bind and fails closed; Windows roster plus DACL | §5.5.1 | proposed (applies only if D11 admits separate accounts) | CT-ID-10, CT-ID-11 |
| DEC-28 | G1 types every master, proposed and PR-added payload and error list (Appendices C and E; PR methods pinned to the heads in §15.1); every PR that adds or changes a method keeps its typed schema, the document and Appendix C in agreement (SCH-6) | §8.0, Appendices C, E | proposed | CT-SCH-02, CT-SCH-03; the proto crate's drift check |
| DEC-29 | Security corrections (fail-closed narrowing) and conformance corrections ship inside protocol 1 with a stable refusal signal and a register entry; widenings only under an accepted rule | §7.2 V-5, V-6; Appendix D | proposed | the register; release notes |
| DEC-31 | `subscription/cancel` is own-connection cleanup: no resource grant, a live bound credential, only the caller's own subscriptions | §10.1 | proposed | runtime change to `Method::authz()`; CT-SUB-01, CT-SUB-02 |
| DEC-30 | D2-A in full: genuine credential absence only, `anonymous_http` handshake field, `binding` and `pairing_generation`, activation at the next generation, per-operation invalidation | §5.4 | proposed (conditional on D2-A) | CT-D2-01..03 |
| D1–D13 | see §17.1 | | **open** | none |

### 17.1 Decisions for the maintainers

Each is listed in ADR-019 with the same number. None has a default in this contract.

| # | Decision | Recommendation | Blocks |
|---|---|---|---|
| D1 | Gateway service identity (§5.4) | D1-A: `service` provider, key file, method-allowlist profile, no pairing-code delivery | `webhook/deliver` from a separate process, `gateway/*`, `pairing/redeem` |
| D2 | Pairing-disabled compatibility (§5.4) | D2-C for v0.9.0 and until D1-A ships (pairing-disabled installs keep the in-process gateway); D2-A, as specified in DEC-30, after that | serving `require_pairing = false` installs from `zeroclaw-gw` |
| D3 | Owner of webhook- and channel-originated sessions (§12.5) | mapped sender under explicit channel policy; shared operator only as a declared legacy mode | `ingress/webhook` principal and session continuation |
| D4 | Approvals with no viewer attached (§9.4) | park until the existing deadline and replay | TL work in #11185 follow-ups, gateway chat port |
| D5 | Supervision mode and config key (§11.3) | `child` default, `external` for hardened and desktop | G3 cut, G5 |
| D6 | Product-version skew (§7.3) | same `major.minor`; desktop exact | G3 diagnostics, G5 |
| D7 | Native vendor routes in v0.9.0 (§12.9) | in-process gateway only; `zeroclaw-gw` answers 503 | G3 route set |
| D8 | Core listeners under "no HTTP server in the core" (§14.3) | `[wss]`, `[enroll]` documented opt-in exceptions; channel listeners a dated exception | G3 artifact acceptance |
| D9 | What v0.9.0 ships (§17.2) | contract + parity + in-process groundwork; process split after | release scope |
| D10 | Surfaces with no core path: ACP, nodes, WebAuthn, OIDC browser relays (§8.14) | keep on the in-process gateway for v0.9.0; `zeroclaw-gw` answers 503 with a message naming the surface | G3 route set |
| D11 | Which OS trust boundaries v0.9.0 supports (§5.5, §5.5.1) | same account only; separate accounts once §5.5.1 and D1-A ship | hardened `zeroclaw-gw` deployments |
| D12 | What an idempotency key means after admission on generic ingress (§12.5): (a) admitted once, as today: admitted work that later fails or is cancelled keeps the key; (b) commit at completion and roll back on failure or cancellation, so a retry re-runs work that may have partly executed; (c) admitted ingress turns run to completion whatever the caller does | (a) for v0.9.0: it changes nothing beyond the refusal fix of DEC-24 | the ingress port |
| D13 | Release-surface changes in `zeroclaw-gw`: `/admin/paircode*` leave it (`zeroclaw gateway get-paircode` calls the core socket); `OPTIONS /api/config/prop` requires a credential; with pairing off, a presented but invalid credential gets 401 (D2-A eligibility) | accept all three, with release notes | G3 route set |

Settled here, pending ADR acceptance, because they decide whether keys must move at config V4: webhook idempotency is core-owned (§12.6) and `gateway.webhook_secret` is verified in the core (§12.7). Consequence for config: none. The keys keep their names; §13.1 records the core as their canonical owner and only reader.

### 17.2 What fits the v0.9.0 window

The v0.9.0 window is days, not weeks. Stated plainly, from the dependency chain above:

- **Fits** (reviewable now and needing no D1–D13 choice; each item still needs its own DEC accepted before an implementation relies on it, per §17.0): this contract and ADR as a draft docs PR; the parity PRs already open (#11132, #11149, #11176, #11169, #11172, #11185, #11182) once reviewed; #11165/#11186 with the SCH fixes; the generation fence (§12.4, DEC-11, #11271) and the core-owned idempotency move (§12.6, DEC-12 and DEC-24) in process; the discovery fix and Windows pipe naming (§4.2, DEC-07); rpc-socket.md errata; version-mismatch and pipe conformance tests.
- **Does not fit without rulings and several more days**: the service identity (D1) and endpoint verification; `webhook/deliver` and `ingress/*` across processes; porting the gateway's routes off in-process state; the `zeroclaw-gw` binary; removing the gateway from the core artifact; desktop two-process supervision (#11004 depends on #11002).

ADR-007 and ADR-019 stay proposed until their acceptance gates ship; a v0.9.0 that ships only the first bullet completes G1 and part of G2, not the gateway split. That is D9.

## Appendix A. Errata this contract records against current docs

| Document | Says | Code | Fix |
|---|---|---|---|
| `docs/book/src/architecture/rpc-socket.md` | `initialize` params `{"protocolVersion":1}`, result `protocolVersion`/`serverVersion` | snake_case `protocol_version`, `server_version` (`rt/rpc/types.rs:47-126`) | use wire spellings |
| same | `session/new` "requires `agentAlias`" | required field is `agent_alias`; a request with `agentAlias` fails to parse (#11165 research, static reading) | same |
| same | `session/update` examples with `sessionId`, `toolCallId`, `rawInput` | `session_id`, `tool_call_id`, `raw_input` (`rt/rpc/dispatch.rs:11469-11560`) | same |
| same | Windows pipe ACL "defaults to the creating user and `SYSTEM`" | default security descriptor also grants read to Everyone and anonymous, full control to Administrators (`CreateNamedPipe` documentation) | state it; apply an explicit DACL (§5.5) |
| same (#11165 head) | "the prose half of the contract" | the prose page is not reviewed against the OpenRPC document | this contract replaces it as the normative prose |
| #11165 `crates.md`, `rpc_proto_boundary.rs` | proto crate "never on async I/O" | transitive tokio/reqwest/SQLite through `zeroclaw-api`/`zeroclaw-config` | narrow the claim or cut the edges |
| #11165 OpenRPC | `session/prompt` result `SessionPromptResult`; 17 error codes | `{}`; `-32004` missing | SCH-2, SCH-3 |

## Appendix B. Findings recorded while writing this contract (independent of G1)

These are static readings, not reproduced; each deserves its own issue.

- The gateway's `GmailPushChannel` is never `listen()`ed, so notifications after the first are dropped while the route answers 200; `/webhook/gmail` is unauthenticated when its secret is empty; its 1 MiB cap is unreachable behind the 64 KiB layer (`gw/lib.rs:1587-1610`, `:4572-4609`; `ch/gmail_push.rs:438-454`).
- Linq and Nextcloud Talk HMACs are computed over `from_utf8_lossy(body)`, not the exact bytes (`gw/lib.rs:4382`, `:4520`).
- A streamed `/webhook` with `X-Session-Id` registers under the same `gw_<id>` cancel key as `/ws/chat` and cancels a live WebSocket turn with that id (`gw/lib.rs:3643-3659`, `:3771-3784`; `gw/ws.rs:1714-1725`).
- `require_pairing` is fixed per `PairingGuard` instance (a daemon generation) while `webhook_secret` is re-read per request (`cfg/pairing.rs:359`, `:447-448`; `gw/lib.rs:3123-3133`).
- A plugin channel's webhook receiver can be taken only once; if `listen()` restarts within a generation the route stays published and answers 503 (`crates/zeroclaw-plugins/src/wasm_channel.rs:767-772`, `:841-843`).
- The core's `socket_path` does not trim `ZEROCLAW_SOCKET` and accepts an empty value; zerocode trims and ignores empty (`rt/rpc/local.rs:167-172`; `zc/client.rs:170-175`).
- zerocode sends `ACP_PROTOCOL_VERSION` as the RPC `protocol_version` (`zc/client.rs:1746`, `:2030`); equal to `RPC_PROTOCOL_VERSION` only by coincidence.

- #11182 at b0ad81bfd2, from typing its methods (Appendix C.4): `workspace/list` never lists symbolic links, although `BrowseEntry.kind`'s documentation says links resolve through their target; `canvas/list` returns every canvas id ever created, cleared ones included; `canvas/get` and `canvas/history` return frames of any `content_type`, including `"eval"` frames the agent-side canvas tool stores, because only `canvas/render` checks the type, so a client must treat a frame's `content_type` as untrusted input; and `IntegrationCategory`, `IntegrationStatus` and `CliCategory` serialize as PascalCase Rust variant names (for example `"AiModel"`), unlike every other enum on the wire. Under V-1 these names stay as they are in protocol 1.
- `agents/delete` (#10621, master after the baseline) is authorized only by the coarse `(Agents, Delete)` grant: its handler (`rt/rpc/dispatch.rs:9719-9860` @f151c2e00d) applies no per-agent selector or ownership check, while #11172's agent cascade through `config/map-key-delete` requires the agent plus Delete on memory, cron and sessions. Whether a permission profile can hold a scoped `agents:delete` grant, and so whether this matters, was not verified.

## Appendix C. Wire shapes the OpenRPC document does not type yet

This appendix completes the request and response contract: C.1 and C.2 for every master method whose #11165 OpenRPC side is external (`X`) or untyped (`U`), 40 of the 97; C.3 for every method this contract proposes; C.4 for the 49 methods the open PRs add and the existing payloads they extend. The shapes are read from the serde definitions at f0ae8c8bd8 (container and field attributes applied: `rename_all`, `tag`, `default`, `skip_serializing_if`), and for handlers that build JSON inline, from the handler. They are normative: the proto crate's schemas MUST match them (SCH-1), and a change to either is a protocol change under §7. Proposed payloads are in C.3.

Notation (TypeScript-like). `field: T` is always present on output and required on input unless marked `input optional` (serde `default`); `field?: T` is omitted from output when empty and optional on input; `T | null` admits JSON `null`; `any` is any JSON value; `string (RFC 3339)` is a timestamp. Enum types list their exact wire values. Each type names its source.

### C.1 Method → payload

| Method | Params | Result |
|---|---|---|
| `health` | none | `HealthSnapshot` plus `process: ProcessStats`; on a serialization failure `{status: "error", message: string}` (`rt/health/mod.rs:136-143`) |
| `doctor/run` | none | `DoctorRunResult` |
| `session/new` | `SessionNewParams` | OpenRPC `SessionNewResult` |
| `cron/list` | none | `CronListResult` |
| `cron/get` | `{id: string}` | `CronJob` |
| `cron/add` | `CronAddParams` (#11176 extends it: C.4 overlays) | `CronJob` |
| `cron/patch` | OpenRPC `CronPatchParams` (#11176 extends it: C.4 overlays) | `CronJob` |
| `cron/runs` | OpenRPC `CronRunsParams` | `CronRunsResult` |
| `cron/settings` | `{}` (a `patch` member is outside the contract, §8.5) | `SchedulerConfig` |
| `config/get` | `{prop?: string}` | with `prop`: `ConfigGetPropResult`; without: the whole `Config` document, validating against `config/schema`'s `schema`, every non-empty secret replaced by `"***MASKED***"` |
| `cost/query` | OpenRPC `CostQueryParams` | `CostSummary` |
| `cost/org` | none | the operator-supplied `<data_dir>/org_cost.json` parsed as JSON, or `null` when the file is absent (`rt/rpc/dispatch.rs:8833-8851`). The core passes it through unchanged; it has no schema by design |
| `skills/list` | OpenRPC `SkillsListParams` | `SkillsListResult` |
| `skills/read` | OpenRPC `SkillsReadParams` | `SkillsReadResult` |
| `skills/write` | `SkillsWriteParams` | OpenRPC `SkillsWriteResult` |
| `fs/list_dir` | `FsListDirRequest` | `FsListDirResponse` |
| `locales/list` | none | `LocalesListResponse` |
| `locales/fetch` | `LocalesFetchRequest` | `LocalesFetchResponse` |
| `quickstart/fields` | `QuickstartFieldsParams` | `QuickstartFieldsResult` |
| `quickstart/validate` | OpenRPC `QuickstartValidateParams` (#11176 extends it: C.4 overlays) | `QuickstartValidateResult` |
| `quickstart/apply` | OpenRPC `QuickstartApplyParams` (#11176 extends it: C.4 overlays) | `QuickstartApplyResult` |
| `quickstart/dismiss` | `QuickstartDismissParams` | OpenRPC `QuickstartDismissResult` |
| `cert/renew` | `{csr_pem: string}` | `{cert_pem: string, ca_chain_pem: string, device_id: string, not_after: integer /* Unix seconds */, relay_profile: RelayProfile}` (`rt/rpc/dispatch.rs:3421-3431`) |
| `sops/list` | none | `Sop[]` |
| `sops/get` | `SopSelectRequest` | `Sop` |
| `sops/graph` | `SopSelectRequest` | OpenRPC `SopGraph` |
| `sops/run` | `SopRunRequest` (`payload`, when present, must itself be JSON text) | `SopRunResponse` |
| `sops/runs` | `SopRunsRequest` | `{runs: SopRunSummary[]}` |
| `sops/run-detail` | `SopRunDetailRequest` | `{run: SopRunDetail}`; local transport only |
| `sops/run-overlay` | `SopRunOverlayRequest` | `RunOverlay` |
| `sops/validate` | `{sop: Sop, original_name?: string}` or `SopSelectRequest` | `{blocking: string[], warnings: string[], ok: boolean}` (`SopValidation` plus `ok`) |
| `sops/save` | `SopSaveRequest`, whose `sop` is a `Sop` | `{saved: string}` |
| `sops/create` | `SopSaveRequest` (`original_name` ignored) | `{created: string}` |
| `sops/delete` | `SopSelectRequest` | `{deleted: string}` |
| `sops/rename` | `SopRenameRequest` | `{renamed: string, from: string}`; local transport only |
| `sops/decide` | `SopDecideRequest`, whose `decision` is an `ApprovalDecision` (#11169 makes `name` optional: C.4 overlays) | `RunOverlay` |
| `sops/wire-draft` | `{sop: Sop, edit: WireEdit}` | `{sop: Sop, graph: SopGraph}` |
| `sops/graph-draft` | `{sop: Sop}` | OpenRPC `SopGraph` |
| `sops/trigger-sources` | none | `TriggerSourceRegistry` |
| `tools/param-options` | `{domain: OptionDomain, agent?: string, args?: any}` | `{options: OptionEntry[]}` |

### C.2 Types

```ts
// cfg/cost/types.rs:330
AgentCostStats = {
  agent_alias: string
  cost_usd: number
  total_tokens: integer
  input_tokens: integer  // input optional
  output_tokens: integer  // input optional
  cached_input_tokens: integer  // input optional
  request_count: integer
}

// rt/quickstart/mod.rs:78
AppliedAgent = {
  alias: string
  model_provider: string
  risk_profile: string
  runtime_profile: string
  channels: string[]
  memory_backend: string
}

// rt/sop/approval/decision.rs:11
ApprovalDecision = 
  | "approve"
  | {"deny": {reason?: string | null}}
  | {"amend": {text: string}}
  | {"revise": {guidance: string}}

// rt/sop/trigger_registry.rs:102
BoundTriggerSource = {
  source: string
  fields: TriggerField[]
  condition?: PayloadContract | null
}

// crates/zeroclaw-tools/src/canvas.rs:29
CanvasFrame = {
  frame_id: string
  content_type: string
  content: string
  timestamp: string
}

// rt/sop/trigger_registry.rs:15
ChannelAlias = {
  alias: string
  enabled: boolean
  owning_agent?: string | null
}

// rt/sop/trigger_registry.rs:26
ChannelTriggerKind = {
  channel: string
  aliases: ChannelAlias[]
  configured: boolean
  setup_path: string
  condition?: PayloadContract | null
}

// rt/rpc/types.rs:238
ChatMode = 
  | "chat"
  | "acp"

// rt/health/mod.rs:10
ComponentHealth = {
  status: string
  updated_at: string
  last_ok: string | null
  last_error: string | null
  restart_count: integer
}

// rt/sop/trigger_registry.rs:135
ConditionField = {
  path: string
  label: string
  value_type: ConditionValueType  // input optional
  options?: string[]
}

// rt/sop/condition.rs:219
ConditionOpSpec = {
  token: string
  label: string
}

// rt/sop/trigger_registry.rs:118
ConditionValueType = 
  | "string"
  | "number"
  | "bool"
  | "enum"
  | "date_time"

// rt/rpc/types.rs:676
ConfigGetPropResult = {
  prop: string
  value: string
}

// cfg/cost/types.rs:309
CostSummary = {
  session_cost_usd: number
  daily_cost_usd: number
  monthly_cost_usd: number
  total_tokens: integer
  request_count: integer
  by_model: { [key: string]: ModelStats }
  by_agent: { [key: string]: AgentCostStats }  // input optional
}

// rt/rpc/types.rs:584 (baseline; #11176's extension is in the C.4 overlays)
CronAddParams = {
  agent: string
  schedule: string
  tz?: string | null
  command?: string | null
  prompt?: string | null
  name?: string | null
  job_type?: string | null
  delivery?: DeliveryConfig | null
  session_target?: string | null
  model?: string | null
  allowed_tools?: string[] | null
  delete_after_run?: boolean | null
}

// rt/cron/types.rs:150
CronJob = {
  id: string
  expression: string
  schedule: Schedule
  command: string
  prompt: string | null
  name: string | null
  job_type: JobType
  session_target: SessionTarget
  model: string | null
  agent_alias: string  // input optional
  enabled: boolean
  delivery: DeliveryConfig
  delete_after_run: boolean
  allowed_tools?: string[] | null
  uses_memory: boolean  // input optional
  source: string  // input optional
  shell_output_format: CronShellOutputFormat  // input optional
  created_at: string (RFC 3339)
  next_run: string (RFC 3339)
  last_run: string (RFC 3339) | null
  last_status: string | null
  last_output: string | null
}

// rt/rpc/types.rs:571
CronListResult = {
  jobs: CronJob[]
}

// rt/cron/types.rs:192
CronRun = {
  id: integer
  job_id: string
  started_at: string (RFC 3339)
  finished_at: string (RFC 3339)
  status: string
  output: string | null
  duration_ms: integer | null
  execution: string | null  // input optional
  delivery: string | null  // input optional
  persistence: string | null  // input optional
  principal: InternalPrincipal | null  // input optional
  executing_agent: string | null  // input optional
  job_source: string | null  // input optional
  owner_agent: string | null  // input optional
}

// rt/rpc/types.rs:646
CronRunsResult = {
  runs: CronRun[]
}

// cfg/schema.rs:15478
CronShellOutputFormat = 
  | "wrapped"
  | "raw"

// rt/cron/types.rs:116
DeliveryConfig = {
  mode: string  // input optional
  channel: string | null  // input optional
  to: string | null  // input optional
  thread_id?: string | null
  best_effort: boolean  // input optional
}

// rt/doctor/mod.rs:39
DiagResult = {
  severity: Severity
  category: string
  message: string
}

// rt/rpc/types.rs:158
DoctorRunResult = {
  results: DiagResult[]
  summary: DoctorSummary
  log_path?: string | null
  timed_out_phase?: string | null
}

// rt/rpc/types.rs:150
DoctorSummary = {
  ok: integer
  warnings: integer
  errors: integer
}

// api/jsonrpc.rs:456
FetchedCatalog = {
  name: string
  filename: string
  content: string
}

// rt/quickstart/mod.rs:688
FieldDescriptor = {
  key: string
  label: string
  help: string
  kind: PropKind
  is_secret: boolean
  enum_variants: string[] | null
  required: boolean
  default: string | null
}

// rt/quickstart/mod.rs:680
FieldSection = 
  | "model_provider"
  | "channel"
  | "peer_group"

// rt/sop/types.rs:86
FilesystemEventKind = 
  | "created"
  | "modified"
  | "deleted"
  | "renamed"

// crates/zeroclaw-sop-graph/src/lib.rs:32
FlowRole = 
  | "sequence"
  | "dependency"
  | "failure"
  | "switch"
  | "trigger"

// api/jsonrpc.rs:579
FsEntry = {
  name: string
  full_path: string
  is_dir: boolean
  is_hidden: boolean
  size: integer
  mtime?: integer | null
}

// api/jsonrpc.rs:563
FsListDirRequest = {
  path: string
  show_hidden: boolean  // input optional
}

// api/jsonrpc.rs:572
FsListDirResponse = {
  entries: FsEntry[]
  cwd: string
}

// rt/sop/decision.rs:47
GateOnError = 
  | "run_strict"
  | "skip"

// rt/health/mod.rs:20
HealthSnapshot = {
  pid: integer
  updated_at: string
  uptime_seconds: integer
  components: { [key: string]: ComponentHealth }
}

// rt/agent/prompt.rs:17
InteractionSurface = 
  | "zerocode_code"

// api/ingress.rs:66
InternalPrincipal = 
  | {"cron": {job_id: string, job_name: string | null}}
  | {"peer_agent": {sender_alias: string}}
  | {"daemon": {task: string}}

// rt/cron/types.rs:28
JobType = 
  | "shell"
  | "agent"

// api/jsonrpc.rs:431
LocaleOption = {
  code: string
  label: string
}

// api/jsonrpc.rs:446
LocalesFetchRequest = {
  locale: string
  catalog: string[]  // input optional
}

// api/jsonrpc.rs:466
LocalesFetchResponse = {
  locale: string
  catalogs: FetchedCatalog[]
  skipped: string[]
}

// api/jsonrpc.rs:438
LocalesListResponse = {
  locales: LocaleOption[]
}

// cfg/cost/types.rs:354
ModelStats = {
  model: string
  cost_usd: number
  input_tokens: integer  // input optional
  cached_input_tokens: integer  // input optional
  output_tokens: integer  // input optional
  total_tokens: integer
  unpriced_tokens: integer  // input optional
  request_count: integer
}

// rt/sop/graph.rs:657
NodeRunOverlay = {
  step: integer
  state: NodeRunState
  tool_calls?: StepToolCall[]
}

// crates/zeroclaw-sop-graph/src/lib.rs:275
NodeRunState = 
  | "pending"
  | "active"
  | "completed"
  | "failed"
  | "skipped"

// api/tool.rs:274
OptionDomain = 
  | "channel_refs"
  | "peer_targets"
  | "peer_groups"
  | "agent_aliases"
  | "tool_names"
  | "memory_categories"

// api/tool.rs:292
OptionEntry = {
  value: string
  label?: string
  hint?: string
}

// rt/sop/trigger_registry.rs:152
PayloadContract = {
  open: boolean
  direct?: boolean
  fields?: ConditionField[]
}

// rt/sop/types.rs:306
PlannedToolCall = {
  tool: string
  args: any  // input optional
  pinned?: any | null
}

// rt/process_stats.rs:11
ProcessStats = {
  rss_bytes: integer
  system_ram_total_bytes: integer
  cpu_percent: number | null
  num_cpus: integer
}

// cfg/traits.rs:72
PropKind = 
  | "string"
  | "bool"
  | "integer"
  | "float"
  | "enum"
  | "alias_ref"
  | "string_array"
  | "object_array"
  | "object"

// rt/rpc/types.rs:1806
QuickstartApplyResult = 
  | {kind: "applied", agent: AppliedAgent, daemon_restarted: boolean}
  | {kind: "errors", errors: QuickstartError[]}

// rt/rpc/types.rs:1821
QuickstartDismissParams = {
  run_id: string
  surface: Surface
  last_step: QuickstartStep | null  // input optional
}

// rt/quickstart/mod.rs:128
QuickstartError = {
  step: QuickstartStep
  field: string
  message: string
}

// rt/rpc/types.rs:1776
QuickstartFieldsParams = {
  section: FieldSection
  type_key: string
}

// rt/rpc/types.rs:1783
QuickstartFieldsResult = {
  fields: FieldDescriptor[]
}

// rt/quickstart/mod.rs:89
QuickstartStep = 
  | "model_provider"
  | "risk_profile"
  | "runtime_profile"
  | "memory"
  | "channels"
  | "peer_groups"
  | "agent"

// rt/rpc/types.rs:1792
QuickstartValidateResult = 
  | {kind: "ok"}
  | {kind: "errors", errors: QuickstartError[]}

// rt/enroll/mod.rs:52
RelayProfile = {
  relay_url: string
  node_id: string
  relay_cert_pin: string
}

// rt/sop/graph.rs:670
RunOverlay = {
  run_id: string
  sop_name: string
  status: SopRunStatus
  current_step: integer
  total_steps: integer
  waiting: boolean
  paused: boolean
  nodes: NodeRunOverlay[]
}

// rt/cron/types.rs:101
Schedule = 
  | {kind: "cron", expr: string, tz: string | null /* input optional */}
  | {kind: "at", at: string (RFC 3339)}
  | {kind: "every", every_ms: integer}

// cfg/schema.rs:15080
SchedulerConfig = {
  enabled: boolean  // input optional
  max_tasks: integer  // input optional
  max_concurrent: integer  // input optional
  catch_up_on_startup: boolean  // input optional
  max_run_history: integer  // input optional
}

// rt/rpc/types.rs:207
SessionNewParams = {
  agent_alias: string
  cwd?: string | null
  session_id?: string | null
  tui_id?: string | null
  exclude_memory?: boolean | null
  chat_mode?: ChatMode | null
  interaction_surface?: InteractionSurface | null
  keep_siblings?: boolean | null
}

// rt/cron/types.rs:60
SessionTarget = 
  | "isolated"
  | "main"

// rt/doctor/mod.rs:31
Severity = 
  | "ok"
  | "warn"
  | "error"

// rt/skills/frontmatter.rs:12
SkillFrontmatter = {
  name: string
  description: string
  license?: string | null
  author?: string | null
  version?: string | null
  category?: string | null
  tags?: string[]
  always?: boolean
  slash_options?: SkillSlashOption[]
}

// rt/rpc/types.rs:947
SkillListEntry = {
  bundle: string
  name: string
  directory: string
  frontmatter: SkillFrontmatter
}

// rt/skills/mod.rs:276
SkillSlashChoice = {
  name: string
  value: string
}

// rt/skills/mod.rs:244
SkillSlashOption = {
  name: string
  description: string
  description_localizations: { [key: string]: string }  // input optional
  type: string
  required: boolean  // input optional
  choices: SkillSlashChoice[]  // input optional
  min: number | null  // input optional
  max: number | null  // input optional
  min_length: integer | null  // input optional
  max_length: integer | null  // input optional
}

// rt/rpc/types.rs:956
SkillsListResult = {
  skills: SkillListEntry[]
}

// rt/rpc/types.rs:1030
SkillsReadResult = {
  bundle: string
  name: string
  frontmatter: SkillFrontmatter
  body: string
}

// rt/rpc/types.rs:1039
SkillsWriteParams = {
  bundle: string
  name: string
  frontmatter: SkillFrontmatter
  body: string  // input optional
}

// rt/sop/types.rs:489
Sop = {
  name: string
  description: string
  version: string
  priority: SopPriority
  execution_mode: SopExecutionMode
  triggers: SopTrigger[]
  steps: SopStep[]
  cooldown_secs: integer  // input optional
  max_concurrent: integer  // input optional
  deterministic: boolean  // input optional
  admission_policy: SopAdmissionPolicy  // input optional
  max_pending_approvals: integer  // input optional
  agent?: string | null
  decision?: SopDecisionSpec | null
}

// rt/sop/types.rs:567
SopAdmissionPolicy = 
  | "parallel"
  | "hold"
  | "coalesce"
  | "drop"

// api/jsonrpc.rs:496 (baseline; #11169 makes name optional, C.4 overlays)
SopDecideRequest = {
  name: string
  run_id: string
  decision: any
}

// rt/sop/decision.rs:58
SopDecisionSpec = {
  model: string
  gate?: string | null
  gate_threshold: number  // input optional
  gate_on_error: GateOnError  // input optional
  modes?: SopExecutionMode[]
  mode_instructions?: string | null
  min_confidence: number  // input optional
  part_threshold: number  // input optional
}

// rt/sop/types.rs:40
SopExecutionMode = 
  | "auto"
  | "supervised"
  | "step_by_step"
  | "priority_based"
  | "deterministic"

// rt/sop/types.rs:15
SopPriority = 
  | "low"
  | "normal"
  | "high"
  | "critical"

// api/jsonrpc.rs:554
SopRenameRequest = {
  from: string
  to: string
}

// rt/sop/types.rs:947
SopRunDetail = {
  run_id: string
  sop_name: string
  status: SopRunStatus
  current_step: integer
  total_steps: integer
  started_at: string
  completed_at: string | null
  waiting_since: string | null
  trigger_source: string
  trigger_topic?: string | null
  active: boolean
  failure_reason?: string | null
  steps: SopStepDetail[]
}

// api/jsonrpc.rs:535
SopRunDetailRequest = {
  run_id: string
}

// api/jsonrpc.rs:485
SopRunOverlayRequest = {
  name: string
  run_id: string
}

// api/jsonrpc.rs:507
SopRunRequest = {
  name: string
  payload?: string | null
  dedup_key?: string | null
}

// api/jsonrpc.rs:520
SopRunResponse = {
  run_id: string
}

// api/jsonrpc.rs:528
SopRunsRequest = {
  sop?: string | null
}

// rt/sop/types.rs:732
SopRunStatus = 
  | "pending"
  | "running"
  | "cancel_requested"
  | "waiting_approval"
  | "paused_checkpoint"
  | "completed"
  | "failed"
  | "cancelled"

// rt/sop/types.rs:903
SopRunSummary = {
  run_id: string
  sop_name: string
  status: SopRunStatus
  current_step: integer
  total_steps: integer
  started_at: string
  completed_at: string | null
  trigger_source: string
  active: boolean
}

// api/jsonrpc.rs:543
SopSaveRequest = {
  sop: any
  original_name?: string | null
}

// api/jsonrpc.rs:478
SopSelectRequest = {
  name: string
}

// rt/sop/types.rs:328
SopStep = {
  number: integer  // input optional
  title: string  // input optional
  body: string  // input optional
  suggested_tools: string[]  // input optional
  requires_confirmation: boolean  // input optional
  kind: SopStepKind  // input optional
  schema?: StepSchema | null
  scope?: StepToolScope | null
  routing?: StepRouting
  on_failure?: StepFailure
  mode?: SopExecutionMode | null
  calls?: PlannedToolCall[]
  pos?: StepPos | null
  agent?: string | null
  capability?: string | null
  with?: any | null
  policy?: string | null
  gate_prompt?: string | null
  edit?: string | null
  decide?: string | null
  unless_decided?: integer | null
}

// rt/sop/types.rs:974
SopStepDetail = {
  step_number: integer
  status: SopStepStatus
  output: string
  started_at: string
  completed_at: string | null
  effective_agent?: string | null
  tool_calls?: SopToolCallDetail[]
}

// rt/sop/types.rs:260
SopStepKind = 
  | "execute"
  | "checkpoint"
  | "capability"

// rt/sop/types.rs:765
SopStepStatus = 
  | "completed"
  | "failed"
  | "skipped"

// rt/sop/types.rs:989
SopToolCallDetail = {
  index: integer
  tool: string
  args: any
  success: boolean
  output: string
  error?: string | null
  duration_ms: integer
}

// rt/sop/types.rs:142
SopTrigger = 
  | {type: "mqtt", topic: string, condition: string | null /* input optional */}
  | {type: "webhook", path: string}
  | {type: "cron", expression: string}
  | {type: "peripheral", board: string, signal: string, condition: string | null /* input optional */}
  | {type: "filesystem", path: string, events: FilesystemEventKind[] /* input optional */, condition: string | null /* input optional */}
  | {type: "calendar", calendar_source: string, calendar_ids: string[] /* input optional */, condition: string | null /* input optional */}
  | {type: "channel", channel: string, alias: string | null /* input optional */, condition: string | null /* input optional */}
  | {type: "manual"}
  | {type: "amqp", routing_key: string, condition: string | null /* input optional */}

// rt/sop/mod.rs:1993
SopValidation = {
  blocking: string[]
  warnings: string[]
}

// rt/sop/step_contract.rs:54
StepFailure = 
  | "fail"
  | {"retry": {max: integer}}
  | {"goto": {step: integer}}

// rt/sop/types.rs:320
StepPos = {
  x: number
  y: number
}

// rt/sop/step_contract.rs:23
StepRouting = {
  when?: string | null
  next?: integer | null
  terminal?: boolean
  depends_on?: integer[]
  switch?: SwitchRule[]
}

// rt/sop/types.rs:287
StepSchema = {
  input?: any | null
  output?: any | null
}

// rt/sop/types.rs:787
StepToolCall = {
  index: integer
  tool: string
  args: any
  success: boolean
  output: string
  output_data?: any | null
  error?: string | null
  duration_ms: integer  // input optional
}

// rt/sop/scope/mod.rs:11
StepToolScope = {
  allow?: string[] | null
  deny?: string[]
}

// rt/quickstart/mod.rs:18
Surface = 
  | "web"
  | "tui"
  | "cli"
  | "test"

// rt/sop/step_contract.rs:9
SwitchRule = {
  name: string
  when?: string | null
  goto?: integer | null
}

// rt/sop/trigger_registry.rs:55
TriggerField = {
  name: string
  options?: string[]
  multi?: boolean
  kind: TriggerFieldKind  // input optional
}

// rt/sop/trigger_registry.rs:42
TriggerFieldKind = 
  | "text"
  | "list"
  | "expression"

// rt/sop/trigger_registry.rs:187
TriggerSourceRegistry = {
  sources: string[]
  bound: BoundTriggerSource[]
  channels: ChannelTriggerKind[]
  operators: ConditionOpSpec[]  // input optional
}

// gw/version.rs:192 (moved into zeroclaw-rpc-proto unchanged by #11377, C.6)
VersionCheckResponse = {
  current_version: string
  latest_version: string | null
  is_newer: boolean
  release_url?: string | null
  release_notes?: string | null
  published_at?: string | null
  error?: string | null
}

// rt/sop/wire.rs:27
WireEdit = {
  op: WireOp
  from: integer
  to: integer
  role: FlowRole
  port?: integer | null
}

// rt/sop/wire.rs:16
WireOp = 
  | "connect"
  | "disconnect"
```

### C.3 Proposed payloads

The proposed methods of §8 and §12, in the same notation. Where a section of the contract gives the same shape inline, the two are one definition.

```ts
// §8.13
PairingPosture = { require_pairing: boolean, paired: boolean, pairing_generation: integer, require_pairing_on_reload?: boolean }
GatewayRegisterParams = { instance_id: string, product_version: string, listen: { scheme: "http" | "https", addr: string, path_prefix?: string }, required_methods: string[] }
GatewayRegisterResult = { effective_settings: GatewayEdgeSettings, pairing: PairingPosture, core_version: string, protocol_version: integer, missing_methods: string[] }
// the edge keys of §13.1, typed as in GatewayConfig (cfg/schema.rs:7772); TLS paths are read by the gateway itself
GatewayEdgeSettings = { host: string, port: integer, path_prefix: string | null, allow_public_bind: boolean, web_dist_dir: string | null,
  trust_forwarded_headers: boolean, rate_limit_max_keys: integer, pair_rate_limit_per_minute: integer, webhook_rate_limit_per_minute: integer,
  request_timeout_secs: integer, long_running_request_timeout_secs: integer, websocket_ping_interval_secs: integer,
  allow_remote_admin: boolean, check_updates: boolean,
  tls: { enabled: boolean, cert_path: string, key_path: string, client_auth: { enabled: boolean, ca_cert_path: string, require_client_cert: boolean, pinned_certs: string[], crl_path: string } | null } | null }
GatewaySettingsNotification = { effective_settings: GatewayEdgeSettings, pairing: PairingPosture, config_generation: integer }
GatewayStatusResult = { gateways: { instance_id: string, product_version: string, registered_at: string /* RFC 3339 */, listen: GatewayRegisterParams["listen"] }[],
  ingress: "reachable" | "gateway_absent", public_base_url?: string }
WebhookRoutesResult = { generation: integer, routes: { path: string, plugin: string, channel_alias: string }[] }   // sorted by path; revises the earlier proposal's `paths: string[]` (§12.3)

// §12.3, §12.5, ING-4. `headers` revises the earlier proposal's `[name, value]` pairs; the plugin webhook series still answers a flat outcome instead of `reject`/`reason` (§12.3)
WebhookDeliverParams = { path: string, method: "GET" | "POST", query: string, headers: { name: string, value: string }[], body_b64: string, deadline_ms: integer, delivery_id: string }
WebhookDeliverResult =
  | { outcome: "ack" }
  | { outcome: "reply", body: string /* ≤ 4096 UTF-8 bytes */ }
  | { outcome: "reject", reason: "not_found" | "queue_full" | "unavailable" | "unauthorized" | "bad_request" | "invalid_response" | "timeout" | "cancelled" }
IngressEvidence = { bearer?: string, webhook_secret?: string }
IngressWebhookParams = { evidence: IngressEvidence, client_key: string, idempotency_key?: string, session_id?: string, agent?: string,
  body: { message: string }, stream: boolean, delivery_id: string }
IngressSopParams = { evidence: IngressEvidence, client_key: string, idempotency_key?: string, path: string /* "/sop/<rest>" */, body_b64: string, delivery_id: string }
IngressResult =
  | { kind: "chat", session_id: string, content: string, model: string }
  | { kind: "sop", status: "accepted" | "blocked", source: "webhook", path: string,
      results: SopDispatchEventEntry[] }   // C.4: the same entries the in-process route builds (gw/api_sop_webhook.rs:119-152); sop is null on a blocked_unsafe entry that selected no SOP
  | { kind: "duplicate" }
  | { kind: "sop_no_match", path: string }                          // ingress/sop only
IngressCancelParams = { delivery_id: string }
IngressCancelResult = { cancelled: boolean }

// §8.8
PairingCodeReadResult = { pairing_required: boolean, pairing_code: string | null, message: string }   // proposed pairing/code
PairingDeviceCapabilitiesParams = { capabilities: string[] }
PairingDeviceCapabilitiesResult = { capabilities: string[] }
PairingRedeemParams = { code: string, client_key: string, device_label?: string }
PairingRedeemResult = { token: string, device_id: string }

// §8.1, §8.5, §8.7, §8.9, §8.12
SystemVersionCheckParams = { force?: boolean, version?: string }   // result: VersionCheckResponse (C.2); implemented by #11377 (C.6)
ConfigSchemaParams = { path?: string }
ConfigSchemaResult = { schema: any /* the config JSON Schema document */, etag: string,
  prop?: { path: string, kind: PropKind, type_hint: string, is_secret: boolean, enum_variants: string[], category: string } }
SopsSubscribeRunsParams = { sop?: string }   // #11377 uses SopRunsRequest, the same shape (C.6)
SopsSubscribeRunsResult = { subscription_id: string, runs: SopRunSummary[] }
SopRunChangedNotification = { subscription_id: string, seq: integer, run: SopRunSummary }   // #11377 names it SopRunChanged (C.6)
CanvasSubscribeParams = { canvas_id: string }
CanvasSubscribeResult = { subscription_id: string, canvas_id: string, frame: CanvasFrame | null }
CanvasFrameNotification = { subscription_id: string, seq: integer, canvas_id: string, frame: CanvasFrame }

// §9.1, §9.3 (DEC-22)
TurnStartedEvent = { type: "turn_started", session_id: string, turn_id: string, client_turn_generation?: integer }
SessionCancelTurnParams = { session_id: string, turn_id: string }      // session/cancel-turn and session/abort-turn
SessionCancelTurnResult = { session_id: string, turn_id: string, cancelled: boolean }
InitializeResultAddition = { daemon_instance_id?: string }             // random UUID per daemon process

// §5.4 D2-A (only if D2-A is chosen): additions to initialize
InitializeParamsD2 = { anonymous_http?: boolean }                    // accepted only with auth_provider "service"
InitializeResultD2 = { binding?: "service" | "anonymous", pairing_generation?: integer }
```

**Scope of this appendix.** It covers every method on master (C.1, C.2), every method this contract proposes (C.3), every method the original parity PRs add, with the existing payloads they extend (C.4), and what the gateway route ports add and change (C.6). That is the whole operation set of §8. SCH-6 keeps C.4 in step with the PRs until they merge (DEC-28).

### C.4 Methods added by open PRs

The 49 methods the open PRs add, and the eleven existing payloads they extend (four in the table below, seven in the overlays that follow it), typed at the heads of §15.1: #11132 @fad6b15824, #11185 @6713ff419b, #11176 @c0076cfe5d, #11169 @908a035d3f, #11172 @cee32cf254, #11182 @b0ad81bfd2. #11185 carries byte-identical copies of #11132's types but older handlers; the contract follows #11132 for the five shared methods (DEC-20). Same notation as C.2; output-only fields that are omitted rather than set to null are written `field?: T`. The error codes are in E.3. Under SCH-6 a PR whose shapes move before it merges updates this section in the same review cycle.

| Method | PR @head | Params | Result | Handler |
|---|---|---|---|---|
| `session/steer` | #11132 @fad6b15824 | `SessionSteerParams` (`content` must be non-blank) | `SessionSteerResult` (`accepted` is always `true`; every refusal is an error) | rt/rpc/dispatch.rs:6909 @fad6b15824 (arm :2949) |
| `session/abort` | #11132 @fad6b15824 | `SessionIdParams` | `SessionCancelResult` (`cancelled` is always `true`) | rt/rpc/dispatch.rs:6964 @fad6b15824 (arm :2950) |
| `session/append` | #11132 @fad6b15824 | `SessionAppendParams` (`content` non-blank; written as an assistant message) | `SessionAppendResult` (`message_count` = persisted transcript length after the append) | rt/rpc/dispatch.rs:7174 @fad6b15824 (spawned from arm :2951) |
| `session/rename` | #11132 @fad6b15824 | `SessionRenameParams` (`name` trimmed; 1..=200 chars) | `SessionRenameResult` (`name` echoes the trimmed name) | rt/rpc/dispatch.rs:7244 @fad6b15824 (arm :2975) |
| `session/run-once` | #11132 @fad6b15824 | `SessionRunOnceParams` (`prompt` non-blank; `session_id`, if given, must name no existing session; default id `run-once-<uuid v4>`) | `SessionRunOnceResult` (`stop_reason`: `"end_turn"` or `"cancelled"`; `usage` absent when no usage event arrived and no route served the turn). The turn streams `session/update` like `session/prompt`, ending in `turn_complete`; the transient session is closed afterwards (close failure only logged) | rt/rpc/dispatch.rs:7279 @fad6b15824 (spawned from arm :2976) |
| `session/update` `turn_complete` (notification) | #11132 @fad6b15824 | n/a (server to client) | `SessionUpdateEvent::TurnComplete` gains `safeguard_fallback?: SafeguardFallbackWire` (completed turns that a safeguard fallback served) and `usage?: TurnUsageTotals` (completed, cancelled and failed turns once a usage event arrived, and completed turns a route served; absent on refusals, missing sessions, and turns that never reached a model call); other fields unchanged | emitted by rt/rpc/dispatch.rs:6598 @fad6b15824; variant rt/rpc/types.rs:1681 @fad6b15824 |
| `session/attach` | #11185 @6713ff419b | `SessionAttachParams` (`since_seq` + `epoch` resume; omit `since_seq` for live frames only; a `since_seq` from another epoch, or with no `epoch`, replays what the ring still holds after a `subscription/lagged` with `epoch_changed: true`) | `SessionAttachResult`. Then `session/update` frames for every session-lifetime turn on the session, from any connection (connection-lifetime turns never reach the ring; see Notes), each with `subscription_id` and `seq` added; gaps reported as `subscription/lagged` (`SubscriptionLagged`); detach with `subscription/cancel` or by closing the connection | rt/rpc/dispatch.rs:7086 @6713ff419b (arm :3075; subscription start :10068) |
| `initialize` (addition) | #11185 @6713ff419b | `InitializeParams.clientCapabilities.turn_lifetime` (inside the free-form `clientCapabilities` object; key is snake_case): the string `"session"` selects session-owned turns; any other value, a non-string, or absence keeps `"connection"`. Never rejected | `InitializeResult.turn_lifetime?: TurnLifetime`, the lifetime in force for this connection's prompts. Always present from a #11185 daemon (`Some`); absent from older daemons | rt/rpc/dispatch.rs:3248 @6713ff419b (capability read :3267-3268; echo :3389) |
| `session/update` ring delivery (addition) | #11185 @6713ff419b | n/a (server to client) | When delivered through a session ring, `session/update` params are the `SessionUpdateEvent` object plus two keys: `subscription_id: string` and `seq: integer` (u64, per-ring sequence). This applies to frames read by a `session/attach` viewer and to every frame of a prompt from a connection initialized with `turn_lifetime: "session"`, including the terminal `turn_complete`; such a prompt rides the connection's existing viewer on that ring, or else an implicit own-turns subscription whose id is first seen on the frames. Connection-lifetime prompts still send plain `session/update` without these keys. Frames the viewer may not see are skipped without a `subscription/lagged`, so `seq` can have gaps | rt/rpc/dispatch.rs:11598 @6713ff419b (keys inserted :11725-11729; ring sink :12113; prompt routing :5810) |
| `skills/create` | #11176 @c0076cfe5d | `SkillsCreateParams` (`name` must be one plain directory name; `body` "" writes a default heading; `no_scaffold: true` skips `scripts/`, `references/`, `assets/`) | `SkillsCreateResult` (`bundle`/`name` from the resolved ref; `directory` = created skill directory path) | rt/rpc/dispatch.rs:9250 @c0076cfe5d (arm :3236) |
| `skills/effective` | #11176 @c0076cfe5d | `SkillsEffectiveParams` | `AgentSkillsResult` (same shape as gateway `GET /api/agents/{alias}/skills`; `dropped` omitted when empty) | rt/rpc/dispatch.rs:9275 @c0076cfe5d (arm :3237) |
| `skills/slash-option-kinds` | #11176 @c0076cfe5d | none (params ignored) | `SkillsSlashOptionKindsResult` (`kinds` in fixed order: string, integer, number, boolean, user, channel, role, mentionable) | rt/rpc/dispatch.rs:9295 @c0076cfe5d (arm :3238) |
| `personality/put` (addition) | #11176 @c0076cfe5d | `PersonalityPutParams` gains `expected_mtime_ms?: integer` (i64 ms since epoch; the mtime the editor last saw). When set, the write is refused unless the file's current mtime equals it; a missing file (or unreadable mtime) never matches, so omit it to create a file | `PersonalityPutResult` (unchanged). On drift: error -32005 with `data: PersonalityDiskDriftData` | rt/rpc/dispatch.rs:9391 @c0076cfe5d (drift check :9419-9448; arm :3243) |
| `sops/cancel` | #11169 @908a035d3f | `SopCancelRequest` (`name`, when non-empty, must be the run's SOP; `reason` is recorded with the cancellation) | `SopCancelResult` (inline `json!`; idempotent: an already-terminal run is reported with `already_terminal: true`, not as an error) | rt/rpc/dispatch.rs:10746 @908a035d3f (arm :3125; result `json!` :10805) |
| `sops/dispatch-event` | #11169 @908a035d3f | `SopDispatchEventRequest` (`path` trimmed, must be non-empty; a `null` `payload` counts as absent; any other value is handed to each started run as its JSON text, so a JSON string arrives with its quotes) | `SopDispatchEventResult` (inline `json!`; `status`: `"accepted"`, `"blocked"` when every match was refused as unsafe (the HTTP route answers 422), or `"no_match"` with `results: []`) | rt/rpc/dispatch.rs:10821 @908a035d3f (arm :3126; result `json!` :10903; entries built at rt/sop/surface.rs:145-180) |
| `sops/decision-models` | #11169 @908a035d3f | none (params ignored) | `{ models: DecisionModelOption[] }` (inline `json!`; sorted by `alias`; entries with no known endpoint, i.e. `custom` without `base_url`, are omitted; never carries `api_key`) | rt/rpc/dispatch.rs:10913 @908a035d3f (arm :3129; `json!` :10915; element built at rt/sop/surface.rs:29) |
| `sops/graph-legend` | #11169 @908a035d3f | none (params ignored) | `GraphLegend` (fixed content: `GraphLegend::canonical()`, the same body as gateway `GET /api/sops/graph-legend`) | inline in the arm, rt/rpc/dispatch.rs:3130 @908a035d3f; body crates/zeroclaw-sop-graph/src/lib.rs:330 @908a035d3f |
| `config/reload-status` | #11172 @cee32cf254 | none: the handler reads no params; absent `params` or any object or array is accepted and ignored (a non-container `params` is refused by the frame parser with `-32600`) | `ConfigReloadStatusResult` | `rt/rpc/dispatch.rs:8580` @cee32cf254 |
| `config/drift` | #11172 @cee32cf254 | none (ignored) | `ConfigDriftResult`; entries sorted by `path`; gateway-managed `gateway.paired_tokens` and env-overridden props are skipped; a missing, unreadable or malformed canonical file yields `{drifted: []}` (`compute_drift` swallows `try_compute_drift`'s error, `rt/config_ops/drift.rs:41-43`) | `rt/rpc/dispatch.rs:8591` @cee32cf254 |
| `config/agent-options` | #11172 @cee32cf254 | none (ignored) | `AgentOptionsResponse` | `rt/rpc/dispatch.rs:8781` @cee32cf254 |
| `config/section-picker` | #11172 @cee32cf254 | `ConfigSectionPickerParams` (params must be an object or array: absent params are `null` and fail `parse_params`) | `PickerResponse` | `rt/rpc/dispatch.rs:8726` @cee32cf254 |
| `config/section-select` | #11172 @cee32cf254 | `SectionSelectParams` (object or array; absent params fail `parse_params`) | `SelectItemResponse` | `rt/rpc/dispatch.rs:8738` @cee32cf254 |
| `config/delete-plan` | #11172 @cee32cf254 | `ConfigMapKeyDeleteParams` (the `config/map-key-delete` params type; absent params fail `parse_params`) | `DeletePlanResponse`; read-only, and the key's existence is never checked (`plan_delete` only walks references, `cfg/alias_refs.rs:163-176`); a `path` that is not an alias map (only `agents`, `providers.{models,tts,transcription}.<family>` and `channels.<type>` are, `cfg/alias_refs.rs:28-60`) gets `{path, key, allowed: true, blockers: [], scrubs: [], cascades_owned_state: false}` | `rt/rpc/dispatch.rs:8718` @cee32cf254 |
| `config/init` | #11172 @cee32cf254 | `ConfigInitParams`; send `{}` for a whole-config init: absent params are `null` and fail `parse_params` with `-32602` ("invalid type: null, expected struct ConfigInitParams") | `InitResponse`; `initialized` empty means nothing was saved | `rt/rpc/dispatch.rs:8602` @cee32cf254 |
| `config/migrate` | #11172 @cee32cf254 | none (ignored) | `MigrateResponse`; `migrated: false` (no write) when the file is already at the current schema, the whole-config authority is still required; on `migrated: true` the canonical file is rewritten with a `.toml.bak` backup, the live config is not replaced and `pending_reload` is set (`rt/config_ops/document.rs:92-100`) | `rt/rpc/dispatch.rs:8634` @cee32cf254 |
| `providers/refresh-context-window` | #11172 @cee32cf254 | `ProvidersRefreshContextWindowParams` (absent params fail `parse_params`) | `RefreshContextWindowResponse`; the fetched value is saved to `<path>.context_window` (provider fetch runs outside the config write lock) | `rt/rpc/dispatch.rs:8688` @cee32cf254 |
| `workspace/list` | #11182 @b0ad81bfd2 | `WorkspaceListRequest` (params must be an object; absent/`null` params fail with -32602) | `BrowseListing` | `rt/rpc/dispatch.rs:1881` (arm :1888) → `rt/rpc/workspace.rs:55` |
| `fs/mkdir` | #11182 @b0ad81bfd2 | `FsMkdirRequest` | `{created: string}` (echoes `params.path` verbatim) | `rt/rpc/dispatch.rs:1881` (arm :1895) → `rt/rpc/workspace.rs:65` |
| `fs/rmdir` | #11182 @b0ad81bfd2 | `FsRmdirRequest` | `{removed: string}` (echoes `params.path` verbatim) | `rt/rpc/dispatch.rs:1881` (arm :1902) → `rt/rpc/workspace.rs:74` |
| `fs/read` | #11182 @b0ad81bfd2 | `FsReadRequest` | `FileReadBody` | `rt/rpc/dispatch.rs:1881` (arm :1909) → `rt/rpc/workspace.rs:79` |
| `fs/delete` | #11182 @b0ad81bfd2 | `FsDeleteRequest` | `{removed: string}` (echoes `params.path` verbatim) | `rt/rpc/dispatch.rs:1881` (arm :1916) → `rt/rpc/workspace.rs:84` |
| `fs/move` | #11182 @b0ad81bfd2 | `FsMoveRequest` | `{from: string, to: string}` (echo `params.from`/`params.to` verbatim) | `rt/rpc/dispatch.rs:1881` (arm :1923) → `rt/rpc/workspace.rs:89` |
| `tools/cli-discover` | #11182 @b0ad81bfd2 | none read (any params ignored) | `{cli_tools: DiscoveredCli[]}` | `rt/rpc/dispatch.rs:1959` (arm :1965) → `rt/rpc/catalog.rs:70` |
| `tools/list` | #11182 @b0ad81bfd2 | `ToolsListRequest` (params must be an object; absent/`null` params fail with -32602) | `{tools: ToolListEntry[]}`; `{tools: []}` when no `agent` is given and no agent is enabled | `rt/rpc/dispatch.rs:2016` → `rt/rpc/catalog.rs:60` |
| `integrations/list` | #11182 @b0ad81bfd2 | none read (any params ignored) | `{integrations: IntegrationListEntry[]}` | `rt/rpc/dispatch.rs:1959` (arm :1961) → `rt/rpc/catalog.rs:32` |
| `plugins/list` | #11182 @b0ad81bfd2 | none read (any params ignored) | `PluginsResponse` | `rt/rpc/dispatch.rs:1959` (arm :1966) → `rt/rpc/catalog.rs:187` |
| `a2a/identity` | #11182 @b0ad81bfd2 | `A2aIdentityRequest` (params must be an object; absent/`null` params fail with -32602) | `AgentCard` (A2A v1.0 protobuf-JSON, camelCase): the per-alias card with `agent`, the discovery catalog card without | `rt/rpc/dispatch.rs:1959` (arm :1987) → `rt/rpc/catalog.rs:395` → `rt/a2a_card.rs:131` (`build_agent_card`) / `rt/a2a_card.rs:69` (`build_catalog_card`) |
| `canvas/list` | #11182 @b0ad81bfd2 | none read (any params ignored) | `{canvases: string[]}`: every key in the daemon's one un-namespaced store (agent-drawn ids appear as `<agent>/<id>`), HashMap order (unspecified) | `rt/rpc/dispatch.rs:2511` (arm :2520) → `rt/rpc/canvas.rs:75` |
| `canvas/get` | #11182 @b0ad81bfd2 | `CanvasIdRequest` | `{canvas_id: string, frame: CanvasFrame}` (`canvas_id` echoes the request) | `rt/rpc/dispatch.rs:2511` (arm :2521) → `rt/rpc/canvas.rs:80` |
| `canvas/history` | #11182 @b0ad81bfd2 | `CanvasIdRequest` | `{canvas_id: string, frames: CanvasFrame[]}` (oldest first, at most 50; `[]` for an unknown or cleared id) | `rt/rpc/dispatch.rs:2511` (arm :2525) → `rt/rpc/canvas.rs:89` |
| `canvas/render` | #11182 @b0ad81bfd2 | `CanvasRenderRequest` | `{canvas_id: string, frame: CanvasFrame}` (the frame just stored) | `rt/rpc/dispatch.rs:2511` (arm :2529) → `rt/rpc/canvas.rs:96` |
| `canvas/clear` | #11182 @b0ad81bfd2 | `CanvasIdRequest` | `{canvas_id: string, status: "cleared"}` (also for an id that never existed) | `rt/rpc/dispatch.rs:2511` (arm :2538) → `rt/rpc/canvas.rs:117` |
| `metrics/scrape` | #11182 @b0ad81bfd2 | none read (any params ignored) | `{content_type: string, text: string}`: `content_type` is always `"text/plain; version=0.0.4; charset=utf-8"`; `text` is the Prometheus text exposition, or the disabled hint (see Notes) | `rt/rpc/dispatch.rs:4017` (inline arm) → `rt/observability/mod.rs:355` |
| `pairing/list` | #11182 @b0ad81bfd2 | none: params are not parsed (any object, array or absent `params` is accepted) | `PairingListResult` | `rt/rpc/dispatch.rs:2092` (arm `:2118`); body `rt/devices.rs:420` |
| `pairing/revoke` | #11182 @b0ad81bfd2 | `PairingRevokeRequest` | `PairingRevokeResult` | `rt/rpc/dispatch.rs:2092` (arm `:2131`); body `rt/devices.rs:443` |
| `pairing/revoke-all` | #11182 @b0ad81bfd2 | none: params are not parsed (any object, array or absent `params` is accepted) | `PairingCodeMintResult` (`message` starts `"Revoked all <n> paired token(s) and cleared the device registry."`) | `rt/rpc/dispatch.rs:2092` (arm `:2143`); body `rt/devices.rs:486` with `rotate = "all"` |
| `pairing/new-code` | #11182 @b0ad81bfd2 | `PairingNewCodeRequest` | `PairingCodeMintResult` | `rt/rpc/dispatch.rs:2092` (arm `:2153`); body `rt/devices.rs:486` |
| `channels/list` | #11182 @b0ad81bfd2 | none: params are not parsed (any object, array or absent `params` is accepted) | `ChannelsListResult` | `rt/rpc/dispatch.rs:2181` (arm `:2192`); body `ch/control.rs:242` via `ch/control.rs:548` |
| `channels/relink` | #11182 @b0ad81bfd2 | `ChannelsRelinkRequest` | `ChannelsRelinkResult` | `rt/rpc/dispatch.rs:2181` (arm `:2196`); body `ch/control.rs:289`, code map `ch/control.rs:552-570` |
| `channels/bind` | #11182 @b0ad81bfd2 | `ChannelsBindRequest` | `ChannelsBindResult` | `rt/rpc/dispatch.rs:2181` (arm `:2204`); body `ch/control.rs:414` + `:507` via `ch/control.rs:572` |
| `system/upgrade` | #11182 @b0ad81bfd2 | `SystemUpgradeRequest` | `UpgradeAcceptedResponse` | `rt/rpc/dispatch.rs:2274` (arm `:2283`, refusal map `:2275-2281`); body `rt/self_upgrade.rs:673` |
| `system/upgrade-status` | #11182 @b0ad81bfd2 | `SystemUpgradeStatusRequest` | `UpgradeStatusResponse` | `rt/rpc/dispatch.rs:2274` (arm `:2300`); body `rt/self_upgrade.rs:738` |
| `system/restart` | #11182 @b0ad81bfd2 | `SystemRestartRequest` | `SystemRestartResult` | `rt/rpc/dispatch.rs:2274` (arm `:2306`, body `:2324`) |

#### C.4 overlays: existing payloads the open PRs extend

Each definition below is the whole payload at the pinned head and replaces the baseline definition (C.2 or the OpenRPC document) for a client of that PR; added members are marked. With the four rows above (`initialize`, `turn_complete`, ring-delivered `session/update`, `personality/put`), these cover all ten shape changes listed in §15.1 and Appendix D; C.5 covers the two optional fields master gained after the baseline.

```ts
// rt/rpc/types.rs:406 @fad6b15824 (#11132; baseline: OpenRPC SessionListParams)
SessionListParams = {
  query?: string | null
  limit?: integer | null
  running?: boolean | null                            // added: true lists only sessions with a turn in flight; absent or false lists every session
}

// rt/rpc/types.rs:584 @c0076cfe5d (#11176; baseline: C.2 CronAddParams)
CronAddParams = {
  agent: string
  schedule: string
  tz?: string | null
  command?: string | null
  prompt?: string | null
  name?: string | null
  job_type?: string | null
  delivery?: DeliveryConfig | null
  session_target?: string | null
  model?: string | null
  allowed_tools?: string[] | null
  delete_after_run?: boolean | null
  uses_memory?: boolean | null                        // added: agent jobs only; false disables memory recall (default true)
  shell_output_format?: CronShellOutputFormat | null  // added: shell jobs only; "wrapped" (default) or "raw"
}

// rt/rpc/types.rs:618 @c0076cfe5d (#11176; baseline: OpenRPC CronPatchParams)
CronPatchParams = {
  id: string
  agent: string
  name?: string | null
  schedule?: string | null
  tz?: string | null
  clear_tz?: boolean | null
  command?: string | null
  prompt?: string | null
  enabled?: boolean | null                            // added: false pauses the job, true resumes it, without deleting it
  uses_memory?: boolean | null                        // added: agent jobs only
  shell_output_format?: CronShellOutputFormat | null  // added: shell jobs only
}

// rt/rpc/types.rs:1886 @c0076cfe5d (#11176; baseline: OpenRPC QuickstartValidateParams)
QuickstartValidateParams = {
  submission: BuilderSubmission
  surface?: Surface | null                            // added: absent means "tui"; "test" is refused with -32602
}

// rt/rpc/types.rs:1917 @c0076cfe5d (#11176; baseline: OpenRPC QuickstartApplyParams)
QuickstartApplyParams = {
  submission: BuilderSubmission
  surface?: Surface | null                            // added: as for quickstart/validate
}

// api/jsonrpc.rs:497 @908a035d3f (#11169; baseline: C.2 SopDecideRequest, where name is required)
SopDecideRequest = {
  name?: string | null                                // now optional: when given, it must be the SOP the run belongs to
  run_id: string
  decision: ApprovalDecision                          // declared serde_json::Value; the handler parses it into ApprovalDecision
}

// rt/rpc/types.rs:808 @cee32cf254 (#11172; baseline: OpenRPC ConfigMapKeyDeleteResult)
ConfigMapKeyDeleteResult = {
  path: string
  key: string
  deleted: boolean
  warnings?: string[]                                 // added, output only: agent deletes whose post-commit steps (workspace archive, owned-state removal) did not complete; omitted when they all completed
}
```

#### C.4 types: Sessions (#11132, #11185), skills (#11176), SOP (#11169)

```ts
// ── #11132 @fad6b15824 ──

// rt/rpc/types.rs:201 @fad6b15824  (pre-existing on upstream/master; not in C.2, listed for completeness)
SessionIdParams = {
  session_id: string
}

// rt/rpc/types.rs:312 @fad6b15824  (pre-existing on upstream/master; not in C.2)
SessionCancelResult = {
  session_id: string
  cancelled: boolean
}

// rt/rpc/types.rs:321 @fad6b15824
SessionSteerParams = {
  session_id: string
  content: string
}

// rt/rpc/types.rs:328 @fad6b15824
SessionSteerResult = {
  session_id: string
  accepted: boolean
}

// rt/rpc/types.rs:337 @fad6b15824
SessionAppendParams = {
  session_id: string
  content: string
}

// rt/rpc/types.rs:344 @fad6b15824
SessionAppendResult = {
  session_id: string
  message_count: integer   // usize
}

// rt/rpc/types.rs:352 @fad6b15824
SessionRenameParams = {
  session_id: string
  name: string
}

// rt/rpc/types.rs:359 @fad6b15824
SessionRenameResult = {
  session_id: string
  name: string
}

// rt/rpc/types.rs:370 @fad6b15824
SessionRunOnceParams = {
  agent_alias: string
  prompt: string
  cwd?: string | null
  session_id?: string | null
  exclude_memory?: boolean | null
}

// rt/rpc/types.rs:385 @fad6b15824
SessionRunOnceResult = {
  session_id: string
  stop_reason: string   // "end_turn" | "cancelled"
  content: string
  usage?: TurnUsageTotals | null
}

// rt/rpc/types.rs:1757 @fad6b15824
SafeguardFallbackKindWire =
  | "server"
  | "client"
  | "client_server"

// rt/rpc/types.rs:1766 @fad6b15824
SafeguardFallbackWire = {
  fallback_kind: SafeguardFallbackKindWire
  requested_model: string
  served_model: string
}

// rt/rpc/types.rs:1792 @fad6b15824
ProviderUsageTotals = {
  provider_ref: string
  model: string
  input_tokens: integer          // u64
  output_tokens: integer         // u64
  cached_input_tokens: integer   // u64
  cost_usd: number               // f64
}

// rt/rpc/types.rs:1808 @fad6b15824
TurnUsageTotals = {
  input_tokens?: integer | null
  output_tokens?: integer | null
  tokens_used?: integer | null          // input + output when either is present
  cost_usd?: number | null              // sum of usage_by_provider[*].cost_usd; absent unless > 0
  provider_ref?: string | null          // serving (accepted) call
  model?: string | null                 // serving (accepted) call
  last_input_tokens?: integer | null
  max_context_tokens?: integer | null   // proactive-trim budget of the serving route
  model_context_window?: integer | null
  usage_by_provider?: ProviderUsageTotals[]   // omitted when empty; sorted by (provider_ref, model)
}

// rt/rpc/types.rs:1681 @fad6b15824  (session/update params; tag "type"; only the #11132 additions are new)
SessionUpdateEvent.TurnComplete = {
  type: "turn_complete"
  session_id: string
  outcome: "completed" | "cancelled" | "failed"
  content: string
  client_turn_generation?: integer | null
  message_count?: integer | null
  safeguard_fallback?: SafeguardFallbackWire | null   // new in #11132
  usage?: TurnUsageTotals | null                      // new in #11132
}

// ── #11185 @6713ff419b ──

// rt/rpc/types.rs:357 @6713ff419b
SessionAttachParams = {
  session_id: string
  since_seq?: integer | null   // u64
  epoch?: string | null
}

// rt/rpc/types.rs:376 @6713ff419b
SessionAttachResult = {
  session_id: string
  subscription_id: string
  seq: integer        // u64; newest seq on the session ring at attach time
  epoch: string       // hub epoch; pass back with since_seq to resume
  running: boolean    // a turn is in flight on the session now
}

// rt/rpc/types.rs:137 @6713ff419b
TurnLifetime =
  | "connection"   // default
  | "session"

// rt/rpc/types.rs:48 @6713ff419b  (only the #11185 addition shown; other InitializeParams fields unchanged)
InitializeParams.clientCapabilities = {   // wire key "clientCapabilities"; free-form object (serde_json::Value)
  turn_lifetime?: "session" | any         // read by TurnLifetime::from_client_capabilities (rt/rpc/types.rs:154); only the exact string "session" selects Session
  // elicitation?: ... (pre-existing)
}

// rt/rpc/types.rs:97 @6713ff419b  (only the #11185 addition shown)
InitializeResult += {
  turn_lifetime?: TurnLifetime | null     // omitted when None; #11185 always sets it
}

// ring-delivered session/update params, built at rt/rpc/dispatch.rs:11725 @6713ff419b
SessionUpdateRingFrame = SessionUpdateEvent & {
  subscription_id: string
  seq: integer   // u64
}

// rt/rpc/types.rs:1626 @6713ff419b  (pre-existing on upstream/master; not in C.2; params of `subscription/lagged`)
SubscriptionLagged = {
  subscription_id: string
  from_seq: integer     // u64; first frame lost
  resume_seq: integer   // u64; delivery continues here
  epoch_changed: boolean   // input optional
}

// ── #11176 @c0076cfe5d ──

// rt/rpc/types.rs:1122 @c0076cfe5d
SkillsCreateParams = {
  bundle: string
  name: string
  frontmatter: SkillFrontmatter
  body: string          // input optional (default "")
  no_scaffold: boolean  // input optional (default false)
}

// rt/rpc/types.rs:1137 @c0076cfe5d
SkillsCreateResult = {
  bundle: string
  name: string
  directory: string
}

// rt/rpc/types.rs:1146 @c0076cfe5d
SkillsEffectiveParams = {
  agent: string
}

// rt/rpc/types.rs:1083 @c0076cfe5d  (pre-existing on upstream/master, unchanged; not in C.2)
AgentSkillsResult = {
  agent: string
  skills: AgentSkillEntry[]
  dropped?: DroppedSkillEntry[]   // omitted when empty
}

// rt/rpc/types.rs:977 @c0076cfe5d  (pre-existing, unchanged; not in C.2)
AgentSkillEntry = {
  name: string
  description: string
  origin: string                 // "workspace" | "open-skills" | "plugin" | "bundle"
  plugin?: string | null         // set when origin = "plugin"
  bundle?: string | null         // set when origin = "bundle"
  directory?: string | null
  editable: boolean              // true only for origin = "bundle"
  shadowed?: ShadowedSkillEntry[]   // omitted when empty
}

// rt/rpc/types.rs:998 @c0076cfe5d  (pre-existing, unchanged; not in C.2)
ShadowedSkillEntry = {
  name: string
  origin: string
}

// rt/rpc/types.rs:1008 @c0076cfe5d  (pre-existing, unchanged; not in C.2)
DroppedSkillEntry = {
  name: string
  origin: string
  reason_kind: string            // "audit_findings" | "audit_error" | "manifest_parse_error"
  reason: string
  scripts_blocked?: boolean      // omitted when false
  directory?: string | null
}

// rt/rpc/types.rs:1154 @c0076cfe5d  (plain Serialize, output only)
SkillsSlashOptionKindsResult = {
  kinds: SlashOptionKindDescriptor[]
}

// rt/skills/mod.rs:224 @c0076cfe5d  (pre-existing, unchanged; not in C.2; output only)
SlashOptionKindDescriptor = {
  manifest_name: string   // "string" | "integer" | "number" | "boolean" | "user" | "channel" | "role" | "mentionable"
  supports_choices: boolean
  supports_numeric_bounds: boolean
  supports_length_bounds: boolean
}

// rt/rpc/types.rs:1234 @c0076cfe5d  (pre-existing type; `expected_mtime_ms` is the #11176 addition)
PersonalityPutParams = {
  agent: string
  filename: string
  content: string
  expected_mtime_ms?: integer | null   // i64; new in #11176
}

// rt/rpc/types.rs:1249 @c0076cfe5d  (pre-existing, unchanged; not in C.2)
PersonalityPutResult = {
  bytes_written: integer      // u64; UTF-8 byte length of content
  mtime_ms?: integer | null   // i64
}

// error.data for PRECONDITION_FAILED (-32005) from personality/put; inline json! at rt/rpc/dispatch.rs:9431 @c0076cfe5d
PersonalityDiskDriftData = {
  error: "personality_disk_drift"
  filename: string
  current_content?: string                // only if the caller's stamped grants permit personality:read; "" when the file is missing
  current_mtime_ms?: integer | null       // same condition; null when the file is missing or its mtime is unreadable
}

// ── #11169 @908a035d3f ──

// api/jsonrpc.rs:509 @908a035d3f
SopCancelRequest = {
  run_id: string
  name?: string | null
  reason?: string | null
}

// inline json! at rt/rpc/dispatch.rs:10805 @908a035d3f
SopCancelResult = {
  run_id: string                 // echoes the request
  sop_name: string
  outcome: "requested" | "already_requested" | "cancelled" | "already_terminal"
  status: SopRunStatus           // == run.status
  already_terminal: boolean      // true only when outcome = "already_terminal"
  run: SopRunSummary             // `active` = run is still in the engine's active set
}

// api/jsonrpc.rs:523 @908a035d3f
SopDispatchEventRequest = {
  path: string
  payload?: any | null
}

// inline json! at rt/rpc/dispatch.rs:10903 @908a035d3f
SopDispatchEventResult = {
  status: "accepted" | "blocked" | "no_match"
  source: "webhook"
  path: string                   // trimmed request path
  results: SopDispatchEventEntry[]
}

// inline json! entries at rt/sop/surface.rs:145 @908a035d3f
SopDispatchEventEntry =
  | { status: "started", sop: string, run_id: string }
  | { status: "skipped", sop: string, reason: string }     // admission refusal: reason starts with "not authorized: "
  | { status: "deferred", sop: string, reason: string }
  | { status: "coalesced", sop: string, run_id: string }   // run_id = the existing run it collapsed into
  | { status: "blocked_unsafe", sop: string | null, reason: string }

// sops/decision-models result, inline json! at rt/rpc/dispatch.rs:10915 @908a035d3f
SopDecisionModelsResult = {
  models: DecisionModelOption[]
}

// rt/sop/surface.rs:19 @908a035d3f  (plain Serialize, output only)
DecisionModelOption = {
  alias: string
  provider: SopDecisionProvider
  model: string       // configured model, else provider default ("jev-latest" / "laya"); "jev-latest" for custom without a model
  base_url: string    // configured base_url, else provider default
}

// cfg/schema.rs:27371 @908a035d3f  (pre-existing config enum; not in C.2)
SopDecisionProvider =
  | "jev"
  | "laya"
  | "custom"

// crates/zeroclaw-sop-graph/src/lib.rs:323 @908a035d3f  (pre-existing on upstream/master, unchanged; not in C.2)
GraphLegend = {
  flow_roles: LegendEntry[]    // keys: "sequence", "dependency", "failure", "switch", "trigger"
  pin_classes: LegendEntry[]   // keys: "flow", "data"
  run_states: LegendEntry[]    // keys: "pending", "active", "completed", "failed", "skipped"
}

// crates/zeroclaw-sop-graph/src/lib.rs:312 @908a035d3f  (pre-existing, unchanged; not in C.2)
LegendEntry = {
  key: string          // snake_case wire value
  label: string        // e.g. "next step", "waits for", "running", "done"
  description: string
}
```

#### C.4 types: Config (#11172)

```ts
// rt/rpc/types.rs:877 @cee32cf254
ConfigReloadStatusResult = {
  pending_reload: boolean  // one flag per daemon generation (rt/daemon/mod.rs:619), shared with the gateway's GET /api/config/reload-status. Set by every accepted RPC config write (install_saved_config, rt/rpc/dispatch.rs:3036), by a migrate that rewrote the file (:8679) and by the gateway's config writes; cleared by the gateway's POST /admin/reload and Quickstart reload signal (gw/lib.rs:4859, gw/api_quickstart.rs:167); a reload starts a new generation with it false. RPC config/reload does not clear it itself.
}

// rt/rpc/types.rs:869 @cee32cf254
ConfigDriftResult = {
  drifted: DriftEntry[]
}

// rt/config_ops/drift.rs:15 @cee32cf254
DriftEntry = {
  path: string
  secret?: boolean  // skip_serializing_if is_false: present only as true (secret or derived-from-secret prop)
  drifted: boolean  // always true
  in_memory_value?: string  // declared serde_json::Value (any); compute_drift writes only the display string; absent when secret
  on_disk_value?: string  // same as in_memory_value
}

// rt/config_ops/agent_options.rs:13 @cee32cf254
AgentOptionsResponse = {
  channels: string[]  // dotted "<type>.<alias>"
  channel_types: string[]  // distinct types with >= 1 alias, sorted
  model_providers: string[]  // dotted "<type>.<alias>"
  risk_profiles: string[]
  runtime_profiles: string[]
  skill_bundles: string[]
  knowledge_bundles: string[]
  mcp_bundles: string[]
  agents: string[]
}

// rt/rpc/types.rs:1312 @cee32cf254
ConfigSectionPickerParams = {
  section: string  // a Section key (cfg/sections.rs:262-473): "providers.models" | "model_routes" | "embedding_routes" | "risk_profiles" | "runtime_profiles" | "storage" | "memory" | "skills" | "skill_bundles" | "mcp" | "mcp.servers" | "mcp_bundles" | "knowledge_bundles" | "providers.tts" | "providers.transcription" | "channels" | "hardware" | "agents" | "peer_groups" | "decision_models" | "cron" | "tunnel" | "onboard_state"; from_key also accepts the key with '_' and '-' swapped; "hardware", "mcp", "skills", "onboard_state" are direct-form and refused
}

// rt/rpc/types.rs:1288 @cee32cf254
// on master since f0ae8c8bd8, first used by this PR
PickerResponse = {
  section: string  // echoes params.section as sent (not normalized)
  items: PickerItem[]
  help: string  // section_help(<canonical key>)
}

// rt/rpc/types.rs:1276 @cee32cf254
// on master, first used by this PR
PickerItem = {
  key: string
  label: string
  description?: string  // Option<String>, skip_serializing_if is_none; set for local model providers, known storage kinds and tunnel entries
  badge?: "active" | "configured" | "needs setup" | "created"  // Option<String>; values produced by rt/config_ops/sections.rs:377-691
}

// rt/rpc/types.rs:1318 @cee32cf254
// on master, first used by this PR
SectionSelectParams = {
  section: string  // a Section key (see ConfigSectionPickerParams)
  key: string  // provider/channel/storage type, alias for one-tier sections (agents, peer_groups, ...), memory backend, or tunnel provider ("none" allowed)
  alias?: string | null  // two-tier sections only; trimmed, "default" when absent or blank; ignored elsewhere
}

// rt/rpc/types.rs:1328 @cee32cf254
// on master, first used by this PR
SelectItemResponse = {
  fields_prefix: string  // "<family>.<key>.<alias>" (providers.models|tts|transcription, channels, storage), "<section>.<key>" (one-tier sections), "memory", "tunnel" (key "none") or "tunnel.<key>"
  created: boolean  // alias newly created; for memory and tunnel: the selection changed something (backend, completion marker, defaults)
}

// rt/rpc/types.rs:801 @cee32cf254
// on master: config/map-key-delete's params
ConfigMapKeyDeleteParams = {
  path: string  // map path: "agents", "providers.models.<family>", "channels.<type>", ...
  key: string
}

// rt/config_ops/delete.rs:39 @cee32cf254
DeletePlanResponse = {
  path: string
  key: string
  allowed: boolean  // false when a hard reference exists or, for agents, live_acp_sessions is not 0 (or unreadable)
  blockers: RefSiteDto[]  // HARD references
  scrubs: RefSiteDto[]  // SOFT references the delete would scrub
  live_acp_sessions?: integer  // Option<usize>: agents only, omitted when the session store cannot be read (allowed is then false)
  cascades_owned_state: boolean  // true iff path == "agents"
}

// rt/config_ops/delete.rs:26 @cee32cf254
RefSiteDto = {
  path: string  // dotted referrer path, e.g. "heartbeat.agent"
  raw_value: string  // stored reference text
}

// rt/rpc/types.rs:1296 @cee32cf254
ConfigInitParams = {
  section?: string | null  // string prefix scoping the init pass (matched both ways with starts_with); absent or null: every uninitialized nested section except those marked init_requires_explicit_config (crates/zeroclaw-macros/src/lib.rs:1193-1213); an unknown prefix initializes nothing
}

// rt/config_ops/document.rs:14 @cee32cf254
InitResponse = {
  initialized: string[]  // sections instantiated with defaults, dotted (e.g. "tunnel.cloudflare", "transcription.openai")
}

// rt/config_ops/document.rs:20 @cee32cf254
MigrateResponse = {
  migrated: boolean
  backup_path?: string  // Option<String>: present iff migrated; "<config>.toml.bak" (config_path.with_extension("toml.bak"))
  schema_version: integer  // u32, zeroclaw_config::migration::CURRENT_SCHEMA_VERSION
}

// rt/rpc/types.rs:1305 @cee32cf254
ProvidersRefreshContextWindowParams = {
  provider_type: string  // model provider family, e.g. "openai"
  alias: string
}

// rt/config_ops/context_window.rs:17 @cee32cf254
RefreshContextWindowResponse = {
  path: string  // "providers.models.<provider_type>.<alias>"
  context_window: integer  // usize, the value written to <path>.context_window
}

// cfg/api_error.rs:93 @cee32cf254
// on master; carried as JSON-RPC error.data by this PR
ConfigApiError = {
  code: ConfigApiCode
  message: string  // equal to error.message
  path?: string  // Option<String>, skip_serializing_if is_none
  op_index?: integer  // Option<usize>; never set on an RPC path (only the gateway's PATCH /api/config sets it)
}

// cfg/api_error.rs:10 @cee32cf254
// on master
ConfigApiCode =
  | "path_not_found"
  | "validation_failed"
  | "config_changed_externally"
  | "reload_failed"  // -> -32603
  | "op_not_supported"
  | "secret_test_forbidden"  // not produced by these nine methods
  | "value_type_mismatch"
  | "required_field_empty"
  | "invalid_numeric_range"
  | "invalid_format"
  | "invalid_enum_variant"  // not produced by these nine methods (no validation_bail! uses it)
  | "dangling_reference"
  | "internal_error"  // -> -32603
```

#### C.4 types: Workspace, catalog, canvas and metrics (#11182)

```ts
// api/jsonrpc.rs:658 @b0ad81bfd2
WorkspaceListRequest = {
  agent: string | null  // input optional; absent = the shared area <install>/shared/
  path: string | null  // input optional; absent or "" = the root
}

// api/jsonrpc.rs:668 @b0ad81bfd2
FsMkdirRequest = {
  agent: string | null  // input optional; absent = the shared area
  path: string
}

// api/jsonrpc.rs:677 @b0ad81bfd2
FsRmdirRequest = {
  path: string  // shared area only; recursive
}

// api/jsonrpc.rs:683 @b0ad81bfd2
FsReadRequest = {
  agent: string
  path: string
}

// api/jsonrpc.rs:691 @b0ad81bfd2
FsDeleteRequest = {
  agent: string
  path: string  // file or directory (recursive)
}

// api/jsonrpc.rs:698 @b0ad81bfd2
FsMoveRequest = {
  agent: string
  from: string
  to: string  // must not exist; missing parent directories are created
}

// rt/browse.rs:477 @b0ad81bfd2
BrowseListing = {
  path: string  // params.path with leading/trailing '/' trimmed ("" for the root)
  entries: BrowseEntry[]  // one level, dotfiles included; dirs before files, each by name in byte order (case-sensitive)
}

// rt/browse.rs:13 @b0ad81bfd2
BrowseEntry = {
  name: string
  kind: "dir" | "file"  // &'static str; judged from the entry itself without following links: symlinks and every other non-dir, non-file entry are left out of the listing (N3)
  size?: integer  // bytes; absent for dirs (and for a file whose metadata read failed)
  protected?: boolean  // present only as true (skip_serializing_if Not::not); set on top-level entries only
}

// rt/browse.rs:494 @b0ad81bfd2
FileReadBody = {
  path: string  // params.path with leading/trailing '/' trimmed
  size: integer  // bytes read (cap 4 MiB = 4194304)
  is_text: boolean  // bytes are valid UTF-8
  content: string  // UTF-8 text when is_text, else standard base64 (with padding)
  encoding: "utf8" | "base64"  // &'static str
}

// crates/zeroclaw-tools/src/cli_discovery.rs:36 @b0ad81bfd2
DiscoveredCli = {
  name: string  // one of the built-in probe names, in this order: git, python, python3, node, npm, pip, pip3, docker, cargo, make, kubectl, rustc, claude, gemini, kilo, gws (only those found)
  path: string  // PathBuf; first line of `which`/`where` output (lossy UTF-8, so always a valid string)
  version: string | null  // first line of the trimmed stdout of `<name> <version_args>` (stderr when stdout is blank); null when that is empty or the probe cannot spawn; exit status is not checked
  category: CliCategory
}

// crates/zeroclaw-tools/src/cli_discovery.rs:8 @b0ad81bfd2
CliCategory =   // no rename_all: Rust variant names verbatim
  | "VersionControl"
  | "Language"
  | "PackageManager"
  | "Container"
  | "Build"
  | "Cloud"
  | "AiAgent"
  | "Productivity"

// api/jsonrpc.rs:564 @b0ad81bfd2
ToolsListRequest = {
  agent: string | null  // input optional; absent = the default agent (smallest enabled alias, rt/tools/listing.rs:28)
}

// rt/rpc/catalog.rs:43 @b0ad81bfd2  (inline json! body of tool_spec_json; name ours)
ToolListEntry = {
  name: string
  description: string
  parameters: any  // the tool's JSON Schema for its arguments (ToolSpec.parameters, api/tool.rs:240)
  output?: any  // present only when ToolSpec.output is Some (declared structured-output schema)
  param_domains?: { [param: string]: OptionDomain }  // present only when non-empty (BTreeMap, keys sorted)
}

// rt/rpc/catalog.rs:16 @b0ad81bfd2  (inline json! body of integration_entry_json; name ours)
IntegrationListEntry = {
  name: string
  description: string
  category: IntegrationCategory
  category_label: "Chat Providers" | "AI Models" | "Tools & Automation" | "Platforms"  // IntegrationCategory::label(), rt/integrations/mod.rs:26
  status: IntegrationStatus
  key: string | null  // canonical config map key (model-provider family key or ChannelsConfig map key); null when the entry has no config section
}

// rt/integrations/mod.rs:18 @b0ad81bfd2
IntegrationCategory =   // no rename_all: Rust variant names verbatim
  | "Chat"
  | "AiModel"
  | "ToolsAutomation"
  | "Platform"

// rt/integrations/mod.rs:9 @b0ad81bfd2
IntegrationStatus =   // no rename_all: Rust variant names verbatim
  | "Available"
  | "Active"

// rt/rpc/catalog.rs:154 @b0ad81bfd2
PluginsResponse = {
  plugins_enabled: boolean  // [plugins].enabled as configured
  wasm_plugins_available: boolean  // compile time: true only in a `plugins-wasm` build
  plugins_dir: string  // [plugins].plugins_dir as configured, before path expansion
  plugins: PluginCatalogEntry[]  // always [] without `plugins-wasm`
  issues: PluginCatalogIssue[]  // always [] without `plugins-wasm`
}

// rt/rpc/catalog.rs:120 @b0ad81bfd2
PluginCatalogEntry = {
  name: string
  installed: InstalledPluginPackage | null
  available: AvailablePluginPackage | null
}

// rt/rpc/catalog.rs:94 @b0ad81bfd2
InstalledPluginPackage = {
  version: string
  description: string | null
  capabilities: string[]  // PluginCapability wire names: "tool" | "channel" | "memory" | "observer" | "skill" (crates/zeroclaw-plugins/src/lib.rs:126)
  permissions: string[]  // PluginPermission wire names: "http_client" | "websocket_client" | "socket_client" | "file_read" | "file_write" | "config_read" | "memory_read" | "memory_write" | "state_read" | "state_write" (crates/zeroclaw-plugins/src/lib.rs:142)
}

// rt/rpc/catalog.rs:105 @b0ad81bfd2
AvailablePluginPackage = {
  version: string
  description: string | null
  capabilities: string[]  // copied verbatim from the cached registry index (free-form strings)
  install_source: string  // "<name>@<version>" (crates/zeroclaw-plugins/src/registry.rs:98)
}

// rt/rpc/catalog.rs:146 @b0ad81bfd2
PluginCatalogIssue = {
  source: PluginCatalogIssueSource
  code: PluginCatalogIssueCode
}

// rt/rpc/catalog.rs:129 @b0ad81bfd2
PluginCatalogIssueSource =
  | "installed"
  | "registry"

// rt/rpc/catalog.rs:137 @b0ad81bfd2
PluginCatalogIssueCode =
  | "discovery_failed"  // with source "installed"
  | "cache_read_failed"  // with source "registry"

// api/jsonrpc.rs:647 @b0ad81bfd2
A2aIdentityRequest = {
  agent: string | null  // input optional; absent = the discovery catalog card
}

// api/a2a_wire.rs:99 @b0ad81bfd2  (rename_all camelCase)
AgentCard = {
  name: string  // per-alias: the alias; catalog: "ZeroClaw agents"
  description: string  // per-alias: identity bio or name line, else "ZeroClaw agent '<alias>'."; catalog: fixed text (rt/a2a_card.rs:108-113)
  supportedInterfaces: AgentInterface[]  // per-alias: exactly one JSONRPC interface; catalog: the "catalog" interface first, then one JSONRPC interface per published alias (sorted)
  version: string  // CARGO_PKG_VERSION of zeroclaw-runtime (workspace version, "0.8.5" at this head)
  capabilities: AgentCapabilities
  defaultInputModes: string[]  // always ["text"]
  defaultOutputModes: string[]  // always ["text"]
  skills: AgentSkill[]  // per-alias: the alias's exposed_skills; catalog: every published alias's, id prefixed "<alias>/" and the alias appended to tags
}

// api/a2a_wire.rs:46 @b0ad81bfd2  (rename_all camelCase)
AgentInterface = {
  url: string  // "<base>/a2a/<alias>" or, for the catalog interface, "<base>/.well-known/agents-card.json"; <base> = [a2a.server].public_base_url without trailing '/', else "http://<a2a.server.bind or gateway.host>:<a2a.server.port or gateway.port>"
  protocolBinding: string  // "JSONRPC" | "catalog"
  tenant?: string  // never set by the core: always absent here
  protocolVersion: string  // always "1.0"
}

// api/a2a_wire.rs:59 @b0ad81bfd2  (rename_all camelCase)
AgentCapabilities = {
  streaming?: boolean  // the core always emits false
  pushNotifications?: boolean  // the core always emits false
  extendedAgentCard?: boolean  // the core always emits false
}

// api/a2a_wire.rs:72 @b0ad81bfd2  (rename_all camelCase)
AgentSkill = {
  id: string  // the skill's reference name within its bundle (SkillRef name, as listed in [agents.<alias>.a2a].exposed_skills); catalog: "<alias>/<ref name>"
  name: string  // SKILL.md frontmatter `name`
  description: string  // SKILL.md frontmatter `description`
  tags?: string[]  // omitted when empty; core-built skills always carry [bundle, category?] (+ alias in the catalog), so present
}

// api/jsonrpc.rs:629 @b0ad81bfd2
CanvasIdRequest = {
  canvas_id: string  // the full store key: an agent-drawn canvas is "<agent>/<id>"
}

// api/jsonrpc.rs:636 @b0ad81bfd2
CanvasRenderRequest = {
  canvas_id: string
  content_type: string | null  // input optional; absent/null = "html"; must be "html" | "svg" | "markdown" | "text" (ALLOWED_CONTENT_TYPES, crates/zeroclaw-tools/src/canvas.rs:29), else -32602
  content: string  // at most 262144 bytes (MAX_CONTENT_SIZE, crates/zeroclaw-tools/src/canvas.rs:13), else -32602
}
```

#### C.4 types: Pairing, channels and system (#11182)

```ts
// api/jsonrpc.rs:614 @b0ad81bfd2
PairingRevokeRequest = {
  device_id: string  // matched exactly, not trimmed
}

// api/jsonrpc.rs:622 @b0ad81bfd2
PairingNewCodeRequest = {
  rotate: string | null  // input optional; trimmed, "" = absent; "all" = revoke every token, any other value = one device id
}

// rt/devices.rs:435 @b0ad81bfd2 (json! in list_devices_body)
PairingListResult = {
  devices: DeviceInfo[]  // [] when pairing is not required (no registry)
  count: integer  // devices.length
}

// rt/devices.rs:20 @b0ad81bfd2
DeviceInfo = {
  id: string
  name: string | null
  device_type: string | null
  paired_at: string (RFC 3339)
  last_seen: string (RFC 3339)
  ip_address: string | null
  capabilities?: string[]  // omitted when None, never null on output
}

// rt/devices.rs:470 @b0ad81bfd2 (json! in revoke_device)
PairingRevokeResult = {
  message: "Device revoked and bearer token invalidated"
  device_id: string  // params.device_id echoed verbatim
}

// rt/devices.rs:596 @b0ad81bfd2 (json! in new_pairing_code, the status-200 body; every non-200 body becomes a JSON-RPC error, rt/rpc/dispatch.rs:2107-2117)
PairingCodeMintResult = {
  success: true
  pairing_required: true
  pairing_code: string
  message: string  // "New pairing code generated — use this one-time code to pair" | "Revoked all <n> paired token(s) and cleared the device registry. Use this one-time code to re-pair." | "Revoked the bearer token for device '<id>'. Use this one-time code to re-pair."
}

// api/jsonrpc.rs:598 @b0ad81bfd2
ChannelsRelinkRequest = {
  channel: string  // composite "<type>.<alias>" as channels/list reports it; matched exactly, not trimmed
}

// api/jsonrpc.rs:605 @b0ad81bfd2
ChannelsBindRequest = {
  channel_type: string  // trimmed before use
  alias: string  // trimmed before use
  identity: string  // normalized per type (telegram: its own normalizer; wechat/line: trim)
}

// ch/control.rs:274 @b0ad81bfd2 (json! in channels_body)
ChannelsListResult = {
  channels: ChannelListEntry[]  // one per [channels.<type>.<alias>] block
}

// ch/control.rs:259 @b0ad81bfd2 (json! per entry in channels_body)
ChannelListEntry = {
  name: string  // "<type>.<alias>"
  type: string  // ChannelAliasInfo.channel_type, kebab-case
  alias: string
  owning_agent: string | null
  enabled: boolean
  compiled: boolean
  status: "active" | "inactive" | "error" | "unknown" | "not_compiled"
  message_count: integer  // always 0
  last_message_at: null  // always null
  health: "healthy" | "degraded" | "down" | "unavailable"  // pairs with status: active/healthy, inactive/degraded, error/down, unknown/degraded, not_compiled/unavailable (ch/control.rs:143)
  readiness: ChannelReadiness
}

// ch/control.rs:45 @b0ad81bfd2
ChannelReadiness = {
  enabled: ChannelReadinessState
  bound_to_agent: ChannelReadinessState
  authenticated: ChannelReadinessState
  listening: ChannelReadinessState
  requirements: string[]
  notes: string[]
}

// ch/control.rs:36 @b0ad81bfd2
ChannelReadinessState = 
  | "ready"
  | "missing"
  | "unknown"

// ch/control.rs:331, :339 @b0ad81bfd2 (json! in relink)
ChannelsRelinkResult = 
  | {channel: string, outcome: "cleared", removed: string[], restart_required: true, note: string}
  | {channel: string, outcome: "nothing_to_clear", removed: string[] /* always [] */, restart_required: false, note: string}

// ch/control.rs:487, :531 @b0ad81bfd2 (json! in prepare_bind AlreadyBound, commit_bind)
ChannelsBindResult = 
  | {saved: false, already_bound: true, group: string | null, channel: string, restart_required: false}
  | {saved: true, already_bound: false, group: string, channel: string, restart_required: true, note: string}
  // channel = "<trimmed type>.<trimmed alias>"; group = the peer_groups key written, or (already bound) the key that authorizes the identity, else the channel's group key, else null

// api/jsonrpc.rs:573 @b0ad81bfd2
SystemUpgradeRequest = {
  version: string | null  // input optional; target release tag, latest when absent
  auto_restart: boolean  // input optional; default false
}

// rt/self_upgrade.rs:324 @b0ad81bfd2
UpgradeAcceptedResponse = {
  handoff_id: string  // UUID v4
}

// api/jsonrpc.rs:583 @b0ad81bfd2
SystemUpgradeStatusRequest = {
  handoff_id: string | null  // input optional; ignored while no upgrade has run in this process
}

// rt/self_upgrade.rs:347 @b0ad81bfd2
UpgradeStatusResponse = {
  handoff_id?: string
  state: UpgradeStatusState
  phase?: integer  // 0 before the first "Phase N/6" marker, else 1..6
  log_tail?: string[]  // last <= 50 output lines, ANSI stripped
  previous_version?: string
  target_version?: string
  restart_mode?: "desktop_supervised" | "supervised" | "self_respawn" | "manual"  // Rust type String, from RestartMode::as_str (rt/self_upgrade.rs:55)
  restart_hint?: string
  error?: string
}
  // skip_serializing_if on every Option: "?" fields are omitted, never null.
  // state "idle" (no upgrade has run) => only {state}. Otherwise handoff_id, phase, log_tail, previous_version, restart_mode, restart_hint are always present; target_version only when the request named a version; error only after a failure.

// rt/self_upgrade.rs:265 @b0ad81bfd2
UpgradeStatusState = 
  | "idle"
  | "running"
  | "done"
  | "restarting"
  | "failed"

// api/jsonrpc.rs:591 @b0ad81bfd2
SystemRestartRequest = {
  component: string  // only "daemon" is accepted (compared untrimmed, case-sensitive)
}

// rt/rpc/dispatch.rs:2324 @b0ad81bfd2 (inline json!)
SystemRestartResult = {
  component: "daemon"
  restarting: true
}
```

#### C.4 notes

Behaviour a client must know that the shapes alone do not show, read from the same heads.

**Sessions (#11132, #11185), skills (#11176), SOP (#11169).**

**Scope and conventions.**

- All 13 methods are absent from the baseline f0ae8c8bd8; every type marked "new" was confirmed absent there with `git grep -P`. Types marked "pre-existing" are defined in the type block only because they are referenced and missing from Appendix C.2.
- "gate" codes come from the per-method authorization in `process_line` (`authorize`, `rt/rpc/dispatch.rs:1090 @fad6b15824` and the same function at each head): -32010 when the connection is unbound, the credential is expired, revalidation is due, the pairing was revoked, or the re-resolved policy denies the identity with an auth-type reason; -32012 when the grant for the method's resource:verb is missing or the policy denies with a forbidden-type reason. Every method in this file is gated. The generic -32700, -32600 and -32601 (for an unknown method, e.g. against a head without the PR) are not repeated per row.
- Helper-level -32010 codes that can only fire on an unbound dispatcher (`check_agent_selector`, `selector_session_agent`) are unreachable after the gate; the rows say so where it matters.

**`error.data` delivery (important for PRECONDITION_FAILED).**

- At #11132, #11185 and #11169, `process_line` sends handler errors with `send_error(id, code, message)`, which drops `data`. #11176 adds `send_rpc_error` (`rt/rpc/dispatch.rs:10211 @c0076cfe5d`) for inline handlers, which keeps `data`. That is what lets the personality/put -32005 `data` payload reach clients.
- Spawned handlers (session/prompt, and at #11132 session/append and session/run-once) still reply through `send_error` and would drop `data`. None of them sets `data` today.

**#11132 vs #11185 copies.**

- The types are identical. #11185 @6713ff419b carries these #11132 types byte-for-byte at shifted lines: SessionSteerParams/Result, SessionAppendParams/Result, SessionRenameParams/Result, SessionRunOnceParams/Result, TurnUsageTotals, ProviderUsageTotals, SafeguardFallbackWire, SafeguardFallbackKindWire, plus SessionIdParams, SessionCancelResult, TurnCompletionOutcome, and the whole `SessionUpdateEvent` enum. Compared source block by source block. Examples: SessionSteerParams is at types.rs:321 in #11132 and :391 in #11185; TurnUsageTotals is at :1808 and :1929.
- The handlers are not identical. #11185 has its own copies of the turn-parity commit (647afcd224; #11132 has a3b5d01588) and of 79efe45bed (as 3c31f243a6). It lacks the 8 later #11132 fix commits, 229eb35bc8 through fad6b15824 (`git cherry`, and subject match). The differences visible on the wire are listed below; error sets for #11185's copies were not derived separately, and the contract should follow #11132 @fad6b15824.
  - session/steer and session/abort use `recheck_session_control`. A credential or revalidation failure there is returned as -32010, whereas #11132's steer returns an admission refusal as -32012.
  - session/append runs inline instead of spawned. It writes any Chat durable row through `durable_chat_key`, including `gw_` and channel rows; #11132 restricts it to `rpc_<session_id>` through `rpc_chat_writer_key`.
  - session/run-once uses plain `handle_session_new` and `handle_session_close`. #11132 uses the create-only `session_new_with_mode` and binds the prompt and the close to the incarnation it created.

**Behaviour worth stating in the contract.**

- session/steer: `accepted: true` means queued for the running turn. At #11132 the steering admission is re-run when the agent consumes the message, and a refusal at that point is not reported back to the steer caller.
- session/run-once: the response arrives after the turn's `turn_complete` notification. `content` for a completed turn includes the rendered safeguard-fallback footer, as on `turn_complete`. A failed turn is -32603, with the turn's user message when it has one.
- session/rename: the 200 limit counts Unicode scalar values (`chars()`), not bytes.
- session/attach (#11185): a viewer sees only session-lifetime turns. Frames of connection-lifetime prompts go straight to the prompting connection (`forward_turn_event`, `rt/rpc/dispatch.rs:12789 @6713ff419b`) and never reach the ring. `running` reflects any in-flight turn, including a connection-lifetime one, so `running: true` can come with no frames.
- initialize (#11185): `turn_lifetime` is assigned before authentication (`rt/rpc/dispatch.rs:3267 @6713ff419b`). A re-initialize that fails authentication still changes the lifetime used by later prompts under the previous binding. This is an observation, not verified by a test.
- personality/put: `may_read` for the drift `data` uses the grants stamped on the connection at this request's gate, not a separate re-resolution. The check-then-write is not atomic.
- sops/cancel: a non-admin caller whose run's procedure is no longer loaded gets -32010 AUTH_REQUIRED, not -32012. A client that treats -32010 as "re-initialize" would misread this.
- sops/dispatch-event runs inline on the connection's reader and awaits `dispatch_webhook_event`. Per the handler's comments a decision model can deliberate before a run starts, so later requests on the same connection wait. This comes from the comments and the await structure, not from a test.

**Uncertainty.**

- The `seq` gaps without `subscription/lagged` for withheld frames come from reading `Discloser::disclose` and `session_authority`, which return `Continue` for a withheld line. Not exercised.
- session/run-once's union includes codes from the reaped-session rehydration path of `run_session_prompt` (-32012). That path is effectively unreachable for a session the call just created.
- `SessionUpdateEvent` variants other than `TurnComplete` are unchanged by these PRs and are not expanded here.

**Config (#11172).**

1. **Handler `data` on the wire.** At this head `process_line` sends every handler error through `send_rpc_error` (`rt/rpc/dispatch.rs:3483-3486`, `:10652-10662`), which serializes the whole `JsonRpcError` including `data`; `send_error` (`:10638-10648`) still sends `data: None` and is what the gate, unknown-method and frame refusals use. The only producer of a non-null handler `data` in `rt/rpc/dispatch.rs` is `config_api_err` (`:693-704`): it is used by seven of these nine methods (not reload-status, drift, agent-options) and, outside this list, by `config/map-key-delete` (`agent_delete_predicate` `:9136`, `delete_alias_with_cascade` `:9211`). Selector and authority refusals (`selector_config_write`, `recheck_config_write_authority`, `authorize_config_write_set`, the `RpcCommitGate`) are built with `rpc_err` and carry no `data`, so no `data.reason` either (Appendix D's proposed `config_path_outside_selector` does not exist at this head). (Citations @cee32cf254.)
2. **`data` is a `ConfigApiError`, keyed `code`.** `data = serde_json::to_value(&ConfigApiError)` and `error.message == data.message`. The JSON-RPC code folds the HTTP distinction away: `path_not_found` (HTTP 404), `config_changed_externally` (409) and every 400-class code all become `-32602`; only `internal_error` and `reload_failed` (500) become `-32603`. A gateway rebuilding today's HTTP status must read `data.code` (`ConfigApiCode::http_status`, `cfg/api_error.rs:71-86`), not the JSON-RPC code. Messages are the HTTP route's text and can name HTTP endpoints ("render fields via GET /api/config/list?prefix=hardware"). `op_index` never appears on these paths. Example, from the parity golden `refresh_context_window_not_found.json` (the `data` member; `code: -32602`): `{"code":"path_not_found","message":"model provider 'anthropic.missing' not found","path":"providers.models.anthropic.missing"}`. (Citations @cee32cf254.)
3. **Absent params.** The frame parser turns absent `params` into `null` (`api/jsonrpc.rs:127`), and `parse_params` is `serde_json::from_value` (`rt/rpc/dispatch.rs:11892-11894`). For a struct type `null` fails, even when every field is optional: checked locally with serde_json, `null` gives `-32602` "invalid type: null, expected struct ConfigInitParams"; `{}` and `[]` succeed; an array binds positionally (`["tunnel"]` gives `section: "tunnel"`). So `config/init` needs `params: {}` for a whole-config init. `rpc_type!` adds no `deny_unknown_fields` (`rt/rpc/types.rs:22-41`), so unknown members are ignored. reload-status, drift, agent-options and migrate never read params. (Citations @cee32cf254.)
4. **Upper bounds, not observed.** The `-32603` listed for reload-status, drift, agent-options, section-picker and delete-plan comes only from `to_result` and cannot fire for these structs. For `config/init`, `value_type_mismatch` and `path_not_found` are reachable only through `classify_validation_message` on a plain (non-`validation_bail!`) `Config::validate()` message; no such message containing "type mismatch", "invalid value" or starting "Unknown property" was found in `cfg/` ("Unknown property" is the prop get/set machinery's fall-through marker, `cfg/helpers.rs:73-83`, `:460`), but not every validator `Config::validate()` calls (`cfg/schema.rs:23601`) was walked, and a wrapped serde error could say "invalid value". The structured codes come from `validation_bail!` in `cfg/schema.rs` (invalid_numeric_range 37 sites, required_field_empty 35, invalid_format 35, dangling_reference 21, validation_failed 3). (Citations @cee32cf254.)
5. **Drift never fails; select's drift check does.** `config/drift` uses `compute_drift`, which returns `[]` on a read or parse error (`rt/config_ops/drift.rs:41-43`), while `config/section-select` calls `try_compute_drift` for a no-op memory or tunnel selection and surfaces its error as `config_changed_externally` (`rt/config_ops/sections.rs:277-307`). `DriftEntry.in_memory_value` / `on_disk_value` are declared `serde_json::Value`; the only producer writes strings, so the type block gives them as `string`. (Citations @cee32cf254.)
6. **Order of checks (what a scoped caller learns).** section-select resolves the target (`select_target_path`) before the selector, so an unknown or direct-form section is `-32602` even for a caller with no selector. refresh-context-window checks the selector on the target before the existence check and the network fetch, so a caller outside the selector gets `-32012` whether or not the provider exists. init with `section` checks the selector first; an unknown `section` that passes it is not an error (`init_defaults` matches nothing: `{initialized: []}`, nothing saved). The `-32010` that `selector_config_write` returns for an unbound dispatcher cannot occur behind the gate, which already requires a bound principal. (Citations @cee32cf254.)
7. **section-select `created` for memory and tunnel** means "the selection changed something" (backend, completion marker or tunnel defaults), not "an alias was created" (`rt/config_ops/sections.rs:229-267`). When it is true the handler still pins `fields_prefix` (`"memory"`, `"tunnel"` or `"tunnel.<key>"`) to Create in the commit check (`rt/rpc/dispatch.rs:8766-8773`). Static read: a caller whose selector names only the target (`memory.backend` or `tunnel.tunnel_provider`) passes the target check but not the pinned prefix (`may_write_config` matches `x.*` or an exact path, `api/grants.rs:221-238`), so it gets `-32012`; the memory tests (`rt/rpc/dispatch.rs:37102`, `:37126`) use `memory.*`, which covers `memory`. Not covered by a test. Because the gate requires Config:Create for every section-select, a memory or tunnel selection needs both Create (gate) and Update (target), as `:37102` asserts. (Citations @cee32cf254.)
8. **Parity goldens.** The shapes above match the RPC goldens in `crates/zeroclaw-runtime/tests/fixtures/config_parity/` (`reload_status`, `drift`, `agent_options`, `section_picker_agents`, `section_picker_direct_form_error`, `section_select_agents`, `section_select_memory`, `delete_plan_agent`, `init`, `migrate`, `refresh_context_window_not_found`), which `parity_call` (`rt/rpc/dispatch.rs:37826`) compares for results and for the error `data` of refusals. The gateway tests pin the same files, so the RPC `data` equals the HTTP error body. No golden covers a successful refresh-context-window (it needs a live provider) or a migration that rewrites the file. (Citations @cee32cf254.)
9. **Pre-existing types.** `PickerItem`, `PickerResponse`, `SectionSelectParams`, `SelectItemResponse` and `ConfigMapKeyDeleteParams` are on master (`f0ae8c8bd8`) but no master method returned or read the first four; `ConfigApiError` / `ConfigApiCode` are on master and reach the RPC wire for the first time here. The `config_ops` types (`DriftEntry`, `AgentOptionsResponse`, `DeletePlanResponse`, `RefSiteDto`, `InitResponse`, `MigrateResponse`, `RefreshContextWindowResponse`) move into `rt/config_ops/` in this PR (the directory does not exist on master); `ConfigDriftResult`, `ConfigReloadStatusResult`, `ConfigInitParams`, `ProvidersRefreshContextWindowParams`, `ConfigSectionPickerParams` are new in `rt/rpc/types.rs`. Every type in these notes' type block was checked by hand against its source (all names are unique in the workspace at this head). (Citations @cee32cf254.)

**Workspace, catalog, canvas and metrics (#11182).**

**N1. Gate grants** (`Method::authz`, `rt/rpc/dispatch.rs:502-505`, `:529-542`): `workspace/list`, `fs/read` Files:Read; `fs/mkdir` Files:Create; `fs/move` Files:Update; `fs/rmdir`, `fs/delete` Files:Delete; `tools/cli-discover`, `tools/list`, `integrations/list` Tools:Read; `plugins/list` Plugins:Read; `a2a/identity`, `metrics/scrape` System:Read; `canvas/list|get|history` Canvas:Read; `canvas/render` Canvas:Update; `canvas/clear` Canvas:Delete.

**N2. Params decoding** (`parse_params`, `rt/rpc/dispatch.rs:11944`; `JsonRpcRequest.params` defaults to `null` when absent, `api/jsonrpc.rs:200-201`), confirmed against serde_json 1.0:
- A method that parses a request struct needs `params` to be an object even when every field is optional: `null`/absent fails with -32602 `invalid type: null, expected struct WorkspaceListRequest`. Affects `workspace/list`, `tools/list`, `a2a/identity` (the tests always send `{}`).
- A JSON array is also accepted, positionally in field order (`fs/move` with `["alpha","a","b"]` parses), because serde's derived struct visitor takes sequences. Incidental and untested; the contract requires object params (§8.0).
- No request type here has `deny_unknown_fields`: unknown keys are ignored; an explicit `null` for an `Option` field equals absent.
- `tools/cli-discover`, `integrations/list`, `plugins/list`, `canvas/list`, `metrics/scrape` never read `params`: anything, including absent, is accepted.

**N3. Workspace/fs error detail** (`browse_error`, `rt/rpc/workspace.rs:27-45`; variants produced in `rt/browse.rs`). Order: parse (-32602) → selector `authorize_workspace_scope` (-32012, `rt/rpc/dispatch.rs:2567`) → blocking worker → effect-time recheck `run_workspace_operation` (`rt/rpc/dispatch.rs:1393-1418`: lease, coarse grant and agent scope re-resolved; -32010 when the credential expired, revalidation is due or the pairing was revoked meanwhile, -32012 when the grant/entitlement/configured agent was withdrawn) → the operation. A refusal is decided before any filesystem access, so it never reveals whether a path exists. Worker panic → -32603 `"<method> task failed: …"`.

| BrowseError | Code | Produced by |
|---|---|---|
| `Escape` (lexical `..` escape via `resolve_under`; or a confined call that got EACCES, `confined()` `rt/browse.rs:313-327`) | 4003 | all six |
| `PathTooLong` (> 4096 bytes or > 256 components after folding) | 4003 | all six |
| `NotADirectory` | 4003 | `workspace/list` (target not a dir), `fs/mkdir` (a component is a file), `fs/rmdir` (target not a dir), `fs/read` (target not a regular file; message says "is not a directory"), `fs/move` (destination exists; message `path 'target '<to, normalized relative path>' already exists' is not a directory`) |
| `LinkedPath` (a mutation would pass through a symlink) | 4003 | `fs/mkdir`, `fs/rmdir`, `fs/delete`, `fs/move` |
| `NotFound` | 4001 | all six; also `fs/mkdir` with `agent` and a path that resolves to the workspace root, and `fs/move` when either side resolves to the root |
| `Protected` / `ProtectedFile` | 4002 | `fs/mkdir` (with `agent`: first component is IDENTITY.md, SOUL.md, USER.md, AGENTS.md, MEMORY.md or DAILY.md), `fs/rmdir` (shared root, `skills`, `skill-bundles`, `knowledge`), `fs/delete` (workspace root, those files, `sessions/` and anything under it), `fs/move` (either side) |
| `InvalidAgent` (alias not exactly one plain path component) | -32602 | every method taking `agent`; in practice only for an operator (admin) principal, since a scoped one must name a configured agent and otherwise gets -32012 first |
| `TooLarge` (> 4 MiB) | -32602 | `fs/read` |
| `Io` (anything else, incl. `create_dir_all` of the root in `fs/mkdir` and a failed read) | -32603 | all six |

Name-matching for protected entries is case- and trailing-dot/space-insensitive (`names_reserved`, `rt/browse.rs:375`). `workspace/list` leaves symlinks out: `list_under_root` (`rt/browse.rs:75-118`) keeps only children whose own `file_type()` is dir or file, and cap-std does not follow links there (checked offline with cap-std 4.0.3 on macOS: a link to a dir and a link to a file both report `is_dir=false, is_file=false`). This contradicts the `BrowseEntry.kind` doc comment ("Symlinks resolve through their target", `rt/browse.rs:15`); a link inside the root can still be read or listed *through* by path. `workspace/list` and `fs/read` cannot produce 4002. `fs/rmdir` takes no `agent` and only ever acts on the shared area; `workspace/list` and `fs/mkdir` without `agent`, and `fs/rmdir`, need access to every agent.

**N4. `tools/list`** (`rt/rpc/dispatch.rs:2016-2083`). Order: parse (-32602) → alias = `agent` or the smallest enabled alias (`{tools: []}` if none) → selector (-32012; the default alias is checked too, so a principal not entitled to it is refused without naming an agent) → alias not configured or not enabled (-32602 `agent "<a>" does not resolve to a configured agent`) → one listing at a time daemon-wide, `try_acquire` so a concurrent call fails at once (-32603 `another tool listing is being assembled; retry`) → assembly on a blocking worker (-32603 on join failure or tool construction error; -32602 again when the agent's risk profile or security policy does not resolve). Assembly connects the agent's MCP servers but runs no tool.

**N5. `a2a/identity`** (`rt/rpc/dispatch.rs:1987-1997`, `rt/rpc/catalog.rs:395-418`). Order: parse (-32602) → only for a named agent, the selector (-32012) → A2A disabled (-32600 `the A2A server is not enabled ([a2a.server] enabled = false)`; checked after the selector, so a refused named agent gets -32012 even when A2A is off) → named agent unknown, disabled or not `a2a.published` (-32602 `agent "<a>" is not published over A2A`) → serialization (-32603; `AgentCard` holds only strings, bools and vectors, so not reachable in practice). The catalog card is not held to the selector: any System:Read holder gets it. URLs come from config only; a gateway started with a host/port override advertises its own override on its well-known routes, so the two can differ (`rt/rpc/catalog.rs:391-394`).

**N6. `plugins/list` is cfg-gated on `plugins-wasm`**, which is not a default feature (root `Cargo.toml` default = agent-runtime, default-channels, acp-bridge, gateway, observability-prometheus, schema-export; zeroclaw-runtime default = observability-prometheus, schema-export). Without it, `plugins_body` cannot fail and always returns `wasm_plugins_available: false, plugins: [], issues: []` (`rt/rpc/catalog.rs:188-194`, `:235-244`); -32603 is then reachable only through serialization, which cannot fail for these types. With it: -32603 `another plugin catalog scan is running; retry` (daemon-wide single scan, `try_acquire`) or `plugin catalog discovery failed` (worker join failure). Source failures are results, not errors: `issues` gets `{source: "installed", code: "discovery_failed"}` (plugin dir metadata or host discovery failed) and/or `{source: "registry", code: "cache_read_failed"}`. `installed` and `available` are separate per package name because their versions can differ.

**N7. `metrics/scrape`** (`rt/rpc/dispatch.rs:4017-4023`, `rt/observability/mod.rs:343-365`). `text` is `PrometheusObserver::shared().encode()` (process-wide registry, the text the gateway's `/metrics` serves) only when built with `observability-prometheus` (default) **and** the live config has `[observability] backend = "prometheus"`; otherwise the fixed `# Prometheus backend not enabled. Set [observability] backend = "prometheus" in config.\n`. An encoder failure yields `""`, not an error (`rt/observability/prometheus.rs:236-242`).

**N8. Canvas** (`rt/rpc/dispatch.rs:2511-2547`, `rt/rpc/canvas.rs`, store in `crates/zeroclaw-tools/src/canvas.rs`). Shared prologue: every canvas method first requires access to every agent (`authorize_every_agent`, `rt/rpc/dispatch.rs:2363`) → -32012 `canvas/<x> on the shared canvas store requires access to every agent`. Failures reach the wire through `From<CanvasFailure> for JsonRpcError` (`rt/rpc/canvas.rs:53-71`): NotFound / InvalidContentType / TooLarge → -32602, CapacityReached → -32600 (`Maximum canvas count reached. Clear unused canvases first.`: at most 100 canvases daemon-wide for a new id; the RPC store is un-namespaced, so the 16-per-namespace cap does not apply). `canvas/render` checks content type, then size, then capacity. `canvas/get` answers -32602 `Canvas '<id>' not found` for an unknown **or cleared** id. `canvas/list` returns every key ever created: `clear` empties content and history but keeps the key, and a subscription creates one, so the list can include ids with no content, despite the store's doc comment ("that currently have content", `crates/zeroclaw-tools/src/canvas.rs:223-236`). `canvas/clear` always answers `status: "cleared"` and pushes a non-stored `content_type: "clear"` frame to subscribers. `CanvasFrame.frame_id` is a UUID v4; `timestamp` is `chrono::Utc::now().to_rfc3339()` (RFC 3339, `+00:00`, sub-second digits).

**N9. `tools/cli-discover`** spawns `which` (`where` on Windows) and `<tool> <version_args>` for 16 fixed names on every call, on one blocking worker with a thread per probe; there is no daemon-wide limit. Results keep probe order. A failed worker is logged and answered with `cli_tools: []`.

**N10. Enum casing.** `IntegrationCategory`, `IntegrationStatus` and `CliCategory` have no `rename_all`, so they emit PascalCase Rust variant names (`"AiModel"`, `"Available"`, `"VersionControl"`), unlike the snake_case of the rest of the protocol. The contract must freeze these literals as they are, or change them as a protocol change.

**N11. Notation choices.** For output-only fields with `skip_serializing_if = "Option::is_none"` (`BrowseEntry.size`, `AgentInterface.tenant`, `AgentCapabilities.*`) are written `field?: T` without `| null`, because the daemon never emits null for them (C.2 writes the same serde pattern as `field?: T | null`, which also admits an explicit null on input). `&'static str` fields are given as their literal value sets (`BrowseEntry.kind`, `FileReadBody.encoding`). `ToolListEntry` and `IntegrationListEntry` name inline `json!` bodies; they are not Rust types. `CanvasFrame` and `OptionDomain` are the C.2 types and have the same shape at b0ad81bfd2; `CanvasFrame` moved from `crates/zeroclaw-tools/src/canvas.rs:29` (Appendix C.2's master cite) to `:33` at this head. `CanvasFrame.content_type` is an open string on the read side: `canvas/render` admits only the four allowed types, but the agent-side canvas tool stores whatever `content_type` the model passes on `render` (its schema enum is advisory; no `ALLOWED_CONTENT_TYPES` check, `crates/zeroclaw-tools/src/canvas.rs:313-343`) and stores `"eval"` frames for its `eval` action (`:393-411`), so `canvas/get` and `canvas/history` can return `"eval"` or any other string. The `"clear"` frame is broadcast only, never stored.

**N12. Uncertain / not verified by running the daemon.** Line numbers are verified against `git show b0ad81bfd2:`. Nothing in the repository was built or run; the params behaviour (N2) and the symlink behaviour (N3) were confirmed with small standalone programs against serde_json and cap-std. The row order of `zeroclaw_plugins::catalog::package_catalog` and of `all_integrations` was not checked, and neither is part of any shape. `AgentCard.version` is `env!("CARGO_PKG_VERSION")` expanded in zeroclaw-runtime, the workspace version (0.8.5 at this head), not a protocol version.

**Pairing, channels and system (#11182).**

**Scope of the error lists.** The table gives what each handler and its helpers can return (static read, upper bounds). Before any handler runs, every method passes the gate in `process_line` (`rt/rpc/dispatch.rs:3802` → `authorize`, `:1167`). The gate can return `-32010` (not initialized, credential expired, revalidation due, native pairing revoked, or a re-resolution after a policy change that denies the credential) and `-32012` (a re-resolution that finds the identity not entitled or the auth config misconfigured, or the coarse grant: `pairing/list` and `system/upgrade-status` need `system:read`; `pairing/revoke` and `pairing/revoke-all` need `system:delete`; `pairing/new-code` needs `system:create`; `channels/list` needs `channels:read`; `channels/relink` and `channels/bind` need `channels:update`; `system/upgrade` and `system/restart` need `system:execute`; map at `rt/rpc/dispatch.rs:533-539`). Framing errors `-32700`, `-32600` and `-32601` come before the gate. The unreachable fallback arms (`_ => INTERNAL_ERROR`, at `rt/rpc/dispatch.rs:2166`, `:2259` and `:2326`) are left out.

**`-32010` reachability.** `-32010` from `require_admin`, `authorize_channel_owner` or `stamped_grants()` (all "First call must be 'initialize'") needs an unbound dispatcher. The gate refuses those first, so over the wire this `-32010` always comes from the gate. The `-32010` from `recheck_config_write_authority` (`rt/rpc/dispatch.rs:1569` → `current_authority`, `:1311`) can really happen. It fires when the credential expires, is revoked or is unpaired while the call waits for the config write lock, and it applies to `pairing/revoke`, `pairing/revoke-all`, `pairing/new-code` and `channels/bind`. `AuthDenied` only ever carries `-32010` or `-32012` (`rt/rpc/auth.rs:91-138`).

**Pairing status → code** (`pairing_error_code`, `rt/rpc/dispatch.rs:641`): 404 → `-32602`, 400 or 503 → `-32600`, anything else (500) → `-32603`. It covers `DeviceFailure` (`rt/devices.rs:344`) and the non-200 statuses of `new_pairing_code`. The error message is `DeviceFailure.message`, or the failure body's `message`. The rest of a failure body (`{success: false, pairing_required: boolean, pairing_code: null, message}`, `rt/devices.rs:496`) never reaches RPC callers.

**Upgrade refusal → code** (`rt/rpc/dispatch.rs:2275-2281`): 400 or 404 → `-32602`, anything else → `-32600`. The statuses are 403 (disabled), 400 (`auto_restart` unavailable) and 409 (upgrade in progress) from `start_upgrade` (`rt/self_upgrade.rs:679`, `:687`, `:697`), and 404 (unknown `handoff_id`) from `upgrade_status` (`:757`). `start_upgrade` checks in that order: 403, then 400, then 409.

**Channels HTTP → code** (`ch/control.rs:559-563`, relink): 404 → `-32602`, 409 → `-32600`, anything else (500) → `-32603`. For bind, `BindFailureKind` maps as follows (`ch/control.rs:589-598`): ValidationFailed or PathNotFound → `-32602`, ReloadFailed → `-32603`.

**No error `data` on the wire (finding).** `ChannelsControl::relink` puts the route's failure body in `JsonRpcError.data` (`ch/control.rs:568`), and its doc comment (`ch/control.rs:541-543`) and the gateway test `channels_list_and_relink_match_the_channel_routes` (`crates/zeroclaw-gateway/src/p6_parity_tests.rs:824`) rely on that. But the dispatcher sends every handler error with `send_error(req_id, e.code, &e.message)` (`rt/rpc/dispatch.rs:4037` → `:10734`), which rebuilds the error with `data: None`. So none of the ten methods ever sends `data`. The bodies that get dropped are 404 `{error}`, 409 `{channel, outcome: "unsupported", error}` and 500 `{channel, error}`. In each case the wire `message` is that `error` string. The test checks `err.data` in-process, not on the wire.

**Params handling.**
- `parse_params` (`rt/rpc/dispatch.rs:11944`) is `serde_json::from_value` into the request struct, and it fails with `-32602`.
- An omitted `params` becomes `Value::Null` (`api/jsonrpc.rs:127`), and serde refuses null for a struct. So `pairing/new-code`, `system/upgrade` and `system/upgrade-status`, whose fields are all optional, still need `params: {}` (the PR's tests always send `{}`).
- A positional array is accepted (serde visits the struct's fields in order), and unknown fields are ignored (no `deny_unknown_fields`).
- `pairing/list`, `pairing/revoke-all` and `channels/list` never parse params.

**Order of checks** (which code wins when several apply):
- `pairing/*`: `require_admin` → local transport (`new-code`, `revoke-all`) → `registry_for` (`-32603`) → `pairing/list` answers here → wait for the config write lock (no timeout) → recheck → `require_admin_grants` → `parse_params` → the operation. A malformed `pairing/revoke` or `pairing/new-code` is reported only after the lock wait.
- `channels/*`: a missing `ChannelControl` (`-32600`) comes before param errors.
- `channels/bind`: params → owner check on the stamped grants → lock → recheck → owner check on the fresh grants → `prepare_bind` (`-32602`/`-32603`) → `authorize_write` (`-32012`; skipped when the identity is already bound) → `commit_bind`.
- `system/upgrade` and `system/restart`: `require_admin` runs before `parse_params`.

**Branch-dependent bodies.**
- `PairingCodeMintResult.message` depends on `rotate`: absent, `"all"`, or a device id.
- `channels/relink`: `cleared` or `nothing_to_clear`.
- `channels/bind`: already bound (nothing written, no write authorization, no policy publish) or written. On `saved: true` the handler publishes an accepted policy at revision+1 under the lock. If publishing fails it is only logged, and the result does not change (`rt/rpc/dispatch.rs:2241-2256`).
- `system/upgrade-status`: `{state: "idle"}` alone until an upgrade has run in this process, and a `handoff_id` sent before then is not checked.
- `system/upgrade` and `system/upgrade-status` wrap `serde_json::to_value(..).unwrap_or(Value::Null)` (`rt/rpc/dispatch.rs:2298`, `:2304`). These structs always serialize, so a null result does not happen.

**Side effects clients see.**
- `pairing/revoke-all`, `pairing/revoke` of the caller's own device, and `pairing/new-code` with `rotate: "all"` or the caller's own device id also revoke the caller's native pairing token when the connection is bound by one. The connection's next request fails the gate with `-32010` (`rpc-auth-pairing-revoked`, `rt/rpc/dispatch.rs:1207-1214`).
- `system/restart` answers first. It then schedules a daemon reload after 200 ms (`RPC_RELOAD_REPLY_FLUSH_DELAY`, `rt/rpc/dispatch.rs:9`), shutting down the gateway first when one is attached (`schedule_daemon_reload`, `:9135`). Every connection drops.
- `system/upgrade` spawns `zeroclaw update` in a detached task. With `auto_restart` the daemon exits (SIGTERM on unix) so its supervisor can relaunch it.

**Feature and config gates.**
- No `cfg` guards the ten methods or the modules behind them. They are always in `Method::ALL` (`rt/rpc/dispatch.rs:380-389`).
- `channels/*` need the `ChannelsControl` the daemon registers (`src/main.rs:7790`, unconditional on the daemon path). Other `RpcContext` builders leave `channel_control: None` (`rt/rpc/context.rs:294`…`:663`), and there all three methods return `-32600`.
- `channels/relink` works only for QR-pairing types: `wechat` (feature `channel-wechat`) and WhatsApp Web (feature `whatsapp-web` with a `web` backend) (`ch/listing.rs:282`). Every other type gets 409 → `-32600`.
- In `channels/list`, `compiled`, `status: "not_compiled"` and the readiness notes depend on which channel features were compiled in (`ch/listing.rs:243`).
- `channels/bind` works only for `telegram`, `wechat` and `line` (`ch/orchestrator/mod.rs:12168`).
- `pairing/*` depends on `gateway.require_pairing`, which is fixed for each `PairingGuard`, that is, for each daemon generation. With pairing off: `pairing/list` returns `{devices: [], count: 0}`, `pairing/revoke` returns `-32600` (503), and `pairing/new-code` and `pairing/revoke-all` return `-32600` (400).
- A single-device rotate's 503 (`rt/devices.rs:540-546`) cannot happen over RPC: `registry_for` returns `None` only when pairing is off, and then `new_pairing_code` has already returned 400.
- `pairing/new-code` and `pairing/revoke-all` need `TransportKind::Local` (Unix socket or named pipe; `rt/rpc/transport.rs:12`). WSS gets `-32012`.
- `system/upgrade` depends on `gateway.allow_self_upgrade`. Whether `auto_restart` is allowed depends on the detected restart mode (`rt/self_upgrade.rs:104`: container, systemd `INVOCATION_ID`/`JOURNAL_STREAM`, launchd `XPC_SERVICE_NAME`, desktop supervision).
- `system/restart` needs `ctx.reload_tx` (a daemon supervisor).

**Notation and uncertainty.**
- Serialize-only `Option` fields with `skip_serializing_if = "Option::is_none"` are written `field?: T`, because they are omitted and never null (`UpgradeStatusResponse`, `DeviceInfo.capabilities`); C.2 writes the same serde pattern as `field?: T | null`.
- `restart_mode` is a `String` in Rust. The literal union comes from `RestartMode::as_str`.
- `DeviceInfo` timestamps use chrono's `DateTime<Utc>` serde format: `SecondsFormat::AutoSi` with a `Z` suffix, so fractional seconds have 0, 3, 6 or 9 digits (chrono 0.4.45 `src/datetime/serde.rs:45`). A stored value that does not parse is reported as the current time (`rt/devices.rs:241`, `:244`).
- `previous_version` is `CARGO_PKG_VERSION` of `zeroclaw-runtime`.
- `message_count: 0` and `last_message_at: null` are constant placeholders in `channels/list`.
- None of the ten methods references an Appendix C (`Appendix C.2`) type.

### C.5 Methods added on master after the baseline

PR #10621 (f151c2e00d) adds two methods after the f0ae8c8bd8 baseline and two optional fields to existing payloads.

| Method | Params | Result | Handler |
|---|---|---|---|
| `agents/delete-preview` | `AgentDeleteParams` | `AgentDeletePreviewResult`; `allowed: false` with a lifecycle blocker appended when another agent lifecycle mutation holds the alias | `rt/rpc/dispatch.rs:9682` @f151c2e00d |
| `agents/delete` | `AgentDeleteParams` | `AgentDeleteResult`; a refused preflight is `{deleted: false, scrubbed: 0, warnings: [], error: "<blockers joined by '; '>"}`, not an RPC error | `rt/rpc/dispatch.rs:9719` @f151c2e00d |

```ts
// rt/rpc/types.rs:905 @f151c2e00d
AgentDeleteParams = { alias: string }
// rt/rpc/types.rs:911 @f151c2e00d
AgentDeletePreviewResult = { alias: string, allowed: boolean, blockers: string[], scrubs: string[], owned_state: string[] }
// rt/rpc/types.rs:921 @f151c2e00d
AgentDeleteResult = { alias: string, deleted: boolean, scrubbed: integer, warnings: string[], error?: string }
// additions to existing payloads (rt/rpc/types.rs:687, :826 @f151c2e00d)
ConfigSetParams += { comment?: string }              // input optional
ConfigMapKeyRenameResult += { rewritten: integer }   // input optional (default 0)
```

### C.6 Additions from the gateway route ports

Seven open PRs that move dashboard routes onto the core add two methods and one notification method (#11377) and change nineteen existing methods (#11345, #11373, #11376, #11381, #11382, #11384), typed at the heads in §15.1: #11345 @a59d3c0f42, #11373 @cec2b83bd1, #11376 @e503e5cb8e, #11377 @8b2ed4ef5c, #11381 @a3b5f3920a, #11382 @967865aeae, #11384 @00a5c1ae52.

Not every change here is additive. The V-1 additions are the new methods and notification, and the optional params and result fields below. The rest are exceptions that protocol 1 admits only as registered entries (V-5, V-6, Appendix D): conformance corrections (F), such as a truthful `memory/delete` and the `sops/run` pre-check; a widening under an accepted rule (W), the administrator's retained cron history; security corrections (S), the authenticated approver and #11381's authority recheck at the effect; and two changed refusal codes, which are not additive at all:

- `sops/decide`: an approval-group or approval-mode refusal is `-32012`, not `-32010` (#11373).
- `cron/delete`: at #11376's head, an administrator's id with neither a job nor retained history answers `-32603`, where the baseline answered `-32602`. This is an implementation gap, not accepted behaviour: the administrator path skips the job lookup that answered `-32602` and maps every removal failure to `INTERNAL_ERROR` (`rt/rpc/dispatch.rs:7354-7367` @e503e5cb8e). `[pending fix round]`: #11376 restores `-32602`.

`[pending fix round]` marks an entry whose PR's review asked for a change that can still alter it; each PR's open points are listed under the table. E.3 gives the error codes of the two new methods and of the refusals these additions introduce.

| Method | PR @head | Params | Result | Handler |
|---|---|---|---|---|
| `system/version-check` | #11377 @8b2ed4ef5c | `SystemVersionCheckParams` (`{}`: the latest release, a result cached for an hour; `force` skips the cache; `version` checks that tag and is never cached) | `VersionCheckResponse` (C.2). A check that fails is this result with `error` set and `latest_version: null`. The caller's authority is resolved again once the check finishes, so a grant withdrawn meanwhile gets the refusal, not the result | rt/rpc/dispatch.rs:3262 @8b2ed4ef5c (recheck :3276-3286; the check rt/update_check.rs:65-95, its subprocess :97-120, killed on timeout) |
| `sops/subscribe-runs` | #11377 @8b2ed4ef5c | `SopRunsRequest` (`{}`: every SOP) | `SopsSubscribeRunsResult`: `runs` is what `sops/runs` lists for the same `sop`, read under the engine lock that arms the change feed. Then `sops/run-changed` on this connection until `subscription/cancel` or the connection ends; a change can reach the connection before this result | rt/rpc/dispatch.rs:10018 @8b2ed4ef5c (delivery :10856-10935; per-delivery recheck :10992-11026) |
| `sops/run-changed` (notification) | #11377 @8b2ed4ef5c | n/a (server to client) | `SopRunChanged`. `seq` counts this subscription's changes from 1. A lag of n changes on the engine's feed sends `subscription/lagged {from_seq: next, resume_seq: next + n, epoch_changed: false}`, and the next change carries `resume_seq`; with `sop` set, n counts every SOP's lost changes. No replay | rt/rpc/dispatch.rs:10856 @8b2ed4ef5c |
| `health` (addition) | #11345 @a59d3c0f42 | unchanged | `components.gateway.bound_addr?: string` (`ip:port`): the address the daemon's own gateway listener bound in its current gateway generation; absent before it binds and after that generation ends | rt/rpc/dispatch.rs:3214 @a59d3c0f42 (inserted :3222-3242; `GatewayBinding` rt/rpc/context.rs:124-139) |
| `memory/list`, `memory/search` (addition) | #11376 @e503e5cb8e | `content_max_chars?: integer`, instead of the default 200-byte preview: content longer than that many characters keeps its first `content_max_chars − 3` characters followed by `...`, so at most `content_max_chars` characters for a budget of 3 or more. A budget of 0, 1 or 2 still yields `...`, three characters: the subtraction saturates at 0, the ellipsis is always appended, and the params accept any unsigned value. A client that needs the bound sends at least 3 (the gateway's memory routes send 4096, `gw/api.rs:24` @e503e5cb8e) | unchanged | rt/rpc/dispatch.rs:10851-10875 @e503e5cb8e (applied :7143, :7177) |
| `memory/delete` (behaviour) | #11376 @e503e5cb8e | unchanged | `deleted` reports whether an entry under the key was removed; on master it is always `true` | rt/rpc/dispatch.rs:7243-7264 @e503e5cb8e |
| `cron/delete`, `cron/runs` (behaviour) | #11376 @e503e5cb8e | unchanged | for a caller with the administrator grant, an id whose job row is gone but whose run history remains (a completed one-shot) is served as the HTTP routes serve it: `cron/delete` removes the history, `cron/runs` lists it. An id with neither is refused: a scoped principal as before, `-32602` from the job lookup; the administrator gets `-32603` at this head, because its path skips that lookup and maps every removal failure to `INTERNAL_ERROR` (`[pending fix round]`; not additive while it stands, Appendix D) | rt/rpc/dispatch.rs:7351-7372 (administrator path :7354-7359, mapping :7367), :7374-7404 @e503e5cb8e |
| `sops/decide` (addition and behaviour) | #11373 @cec2b83bd1 | unchanged | `pending_quorum?: true` when the vote counted and the gate still waits for its quorum; absent otherwise. The approver is the connection's authenticated identity, never the claimed `tui_id`. An approval-group or approval-mode refusal is `-32012`, no longer `-32010` (not additive, Appendix D) | rt/rpc/dispatch.rs:10021 @cec2b83bd1 (refusal :10119-10128; marker :10171-10176; approver :10180-10198) |
| `sops/run` (behaviour) | #11373 @cec2b83bd1 | unchanged | a procedure with an `execute` step no agent owns is refused before dispatch with `-32602` and the reason; on master such a run started and failed its first step | rt/rpc/dispatch.rs:9820-9825 @cec2b83bd1 |
| `status` (addition) | #11382 @967865aeae | `StatusParams` gains `overview?: boolean` (default false) and `agent?: string`. With `overview`, the overview is resolved install-wide or for `agent`, whose selector the caller must pass (`-32012`), checked after the method's awaits; if the core cannot describe its runtime, an `overview` request still answers, without the runtime fields | `StatusResult += { overview?: StatusOverview }`. A core that predates the params ignores them and answers without `overview` | rt/rpc/dispatch.rs:3197-3240 @967865aeae |
| `logs/query` (addition) | #11382 @967865aeae | `LogsQueryParams` gains `field_eq?: {key: value}`, exact matches on attribution fields (any other key is `-32602`; `sop_run_id` is shorthand and wins on conflict), and `report_disabled?: boolean`: with persistence off, an empty page instead of the `-32603` refusal | `LogsQueryResult += { persistence_enabled, daemon_started_at?, attribution_keys? }`. An older core ignores both params: it does not filter by `field_eq` and still refuses when persistence is off | rt/rpc/dispatch.rs:9266-9294 @967865aeae |
| `doctor/run` (addition) | #11382 @967865aeae | `DoctorRunParams { static_only?: boolean }`: only the static checks, with no provider auth check and no live model probes | unchanged. A core that predates it ignores it and runs the full suite, live probes included, and the result does not say which ran | rt/rpc/dispatch.rs:3321-3339 @967865aeae |
| `session/messages`, `session/state`, `session/delete` (addition) | #11381 @a3b5f3920a | `session_keys?: string[]`: 1 to 4 (`MAX_SESSION_KEYS`) non-empty exact chat-store keys, most preferred first, else `-32602`. The core acts on the first key that names a stored row the caller may see, adds no prefix and never falls back to `session_id`; with no visible row it addresses the last key. For a scoped principal another principal's row counts as absent. An `rpc_<id>` key covers the live session `<id>`; no other key covers a live session. `session/state` and `session/delete` take `SessionTargetParams { session_id, session_keys? }`, a superset of `SessionIdParams` on the wire | unchanged. An older core ignores `session_keys` and resolves the id with its fallback, which can address another row | rt/rpc/dispatch.rs:2073-2108 (keys), :6720, :6977, :7041 @a3b5f3920a |
| `session/messages` (addition) | #11381 @a3b5f3920a | `max_bytes?: integer`: a bound on the serialized `messages`. The page keeps the newest entries of its window that fit, and the existing `start` gives its first index for paging back with `before_index` | a single entry over the bound is `-32602` with `data: {reason: "entry_exceeds_max_bytes", index, bytes}` | rt/rpc/dispatch.rs:6949-6963 @a3b5f3920a |
| `session/messages`, `session/delete` (behaviour) | #11381 @a3b5f3920a | unchanged | a stored-transcript read and a delete re-check the caller's authority after the session-queue wait, before the read or delete (S) | rt/rpc/dispatch.rs:6720, :7041 @a3b5f3920a |
| `skills/delete` (addition) | #11384 @00a5c1ae52 | `purge?: boolean`: the skill's directory is removed instead of archived; omitted or `false` archives, as before | unchanged | rt/rpc/dispatch.rs:8695 (purge branch :8701) @00a5c1ae52 |
| `personality/list`, `personality/get`, `personality/put` (addition) | #11384 @00a5c1ae52 | `require_configured_agent?: boolean`: an agent that `[agents]` does not configure, or none, is `-32602` before anything is read or written, even for the operator | unchanged | rt/rpc/dispatch.rs:10893-10909 @00a5c1ae52 (applied :8723, :8781, :8831) |
| `personality/put` (addition) | #11384 @00a5c1ae52 | `expected_mtime_ms?: integer` and `-32005` when it is stale, exactly as #11176 (C.4, §8.16), taken over unchanged so the two merge as one | the refusal's `data` is keyed `error: "personality_disk_drift"` (DEC-17 would key it `reason`) and carries `current_content` and `current_mtime_ms` only for a caller that also holds `personality:read`. At this head method errors are sent without `data` (§15.1, "Error `data`"), so the client receives the code alone | rt/rpc/dispatch.rs:8842-8869 @00a5c1ae52 |
| `personality/templates` (addition) | #11384 @00a5c1ae52 | overrides `preset?`, `agent_name?`, `user_name?`, `timezone?`, `communication_style?`, `include_memory?`, and `defaults?: "quickstart" \| "editor"` (`PersonalityTemplateDefaults`; omitted is `quickstart`, as before). `editor` names the agent by its alias only when it is configured (otherwise `ZeroClaw`) and includes memory unless the backend is `none` | unchanged | rt/rpc/dispatch.rs:8883 @00a5c1ae52 |

```ts
// crates/zeroclaw-rpc-proto/src/types.rs:175 @8b2ed4ef5c
SystemVersionCheckParams = { force?: boolean /* default false */, version?: string }
// crates/zeroclaw-rpc-proto/src/types.rs:1762 @8b2ed4ef5c
SopsSubscribeRunsResult = { subscription_id: string, runs: SopRunSummary[] }   // untyped objects in the OpenRPC document, as for sops/runs
// crates/zeroclaw-rpc-proto/src/types.rs:1774 @8b2ed4ef5c; name at notification.rs:25
SopRunChanged = { subscription_id: string, seq: integer, run: SopRunSummary }
// crates/zeroclaw-rpc-proto/src/lib.rs:76-78 @8b2ed4ef5c
error_reasons.SOP_DISABLED = "sop_disabled"
// additions to existing payloads
MemoryListParams += { content_max_chars?: integer }     // #11376 crates/zeroclaw-rpc-proto/src/types.rs:448 @e503e5cb8e
MemorySearchParams += { content_max_chars?: integer }   // :481
RunOverlay += { pending_quorum?: true }                 // as the sops/decide result only; #11373 crates/zeroclaw-rpc-proto/src/types.rs:103 @cec2b83bd1
HealthSnapshot.components.gateway += { bound_addr?: string /* "ip:port" */ }   // #11345
// #11382 crates/zeroclaw-rpc-proto/src/types.rs:146-155, :161-196, :215-217, :227-232 @967865aeae
StatusParams = { overview?: boolean /* default false */, agent?: string }
StatusOverview = { agent_alias: string | null, model_provider: string | null /* "<type>.<alias>" */, model: string, temperature: number | null,
  memory_backend: string, uptime_seconds: integer, daemon_started_at: string /* RFC 3339 UTC */, gateway_port: integer, locale: string,
  paired: boolean, channels: { [type_alias: string]: boolean }, health: any /* the health snapshot without process */, process: any,
  check_updates: boolean, allow_self_upgrade: boolean, restart_mode: string /* desktop_supervised | supervised | self_respawn | manual */, restart_hint: string }
StatusResult += { overview?: StatusOverview }
DoctorRunParams = { static_only?: boolean /* default false */ }
// :1471-1517, :1521-1565
LogsQueryParams += { field_eq?: { [attribution_key: string]: string }, report_disabled?: boolean }
LogsQueryResult += { persistence_enabled: boolean /* omitted by an older core: read as true */, daemon_started_at?: string, attribution_keys?: string[] }
// #11381 crates/zeroclaw-rpc-proto/src/types.rs:214-233, :381-402 @a3b5f3920a
SessionTargetParams = { session_id: string, session_keys?: string[] /* 1..=4 */ }   // session/state, session/delete
SessionMessagesParams += { session_keys?: string[], max_bytes?: integer }
// error.data of session/messages' -32602 when one entry exceeds max_bytes: rt/rpc/dispatch.rs:6958-6962 @a3b5f3920a
{ reason: "entry_exceeds_max_bytes", index: integer, bytes: integer }
// #11384 crates/zeroclaw-rpc-proto/src/types.rs:967-975, :990-998, :1022-1028, :1046-1060, :1074-1112 @00a5c1ae52
SkillsDeleteParams += { purge?: boolean }
PersonalityListParams += { require_configured_agent?: boolean }
PersonalityGetParams += { require_configured_agent?: boolean }
PersonalityPutParams += { expected_mtime_ms?: integer /* as #11176 */, require_configured_agent?: boolean }
PersonalityTemplatesParams += { preset?: string, agent_name?: string, user_name?: string, timezone?: string,
  communication_style?: string, include_memory?: boolean, defaults?: "quickstart" | "editor" }
```

**Open review points that can still change these entries** (the `[pending fix round]` marks):

- #11376: restore `-32602` for an administrator's `cron/delete` of an id with neither a job nor history.
- #11382: guard `doctor/run {static_only}` against an older core, which runs live probes instead; resolve the `status` agent check on fresh grants after its waits; own the `/api/events` subscription's cleanup during the `logs/subscribe` round trip; treat an older core's refusal of a disabled log as a capability gap.
- #11381: an explicit capability for `session_keys` and `max_bytes` (an older core ignores them); a stable cursor that binds a page walk to one row; an exact `max_bytes` bound that counts the array's own bytes; ownership decided on the fresh grants after the wait; a `zeroclaw-gw` delete that settles a turn the in-process gateway owns.
- #11384: proof of support for the new write semantics before an older core is asked (it would ignore `purge`, `require_configured_agent` and `expected_mtime_ms`); the 409's current file state, which needs `data` on the wire; a bound on personality content across the RPC; the remaining failure-status differences.

`VersionCheckResponse` moves into `zeroclaw-rpc-proto` with its fields and serde attributes unchanged, so the gateway's OpenAPI schema for `GET /api/version/check` keeps its shape. Two names differ from the proposal text in §8.9 and C.3, and the contract takes the PR's: the subscribe params are `SopRunsRequest` (the `sops/runs` type, not a new `SopsSubscribeRunsParams`), and the notification payload is `SopRunChanged` (not `SopRunChangedNotification`).

## Appendix D. Compatibility register (V-1, V-2, V-5, V-6)

Every change the open PRs, and this contract itself, make to an existing method, classified. `A` additive (V-1); `C` behind an opt-in capability (V-2); `S` security correction (V-5); `F` conformance correction (V-6); `W` widening under an accepted rule (V-6); *gap* a change at a PR head that the contract does not accept, which that PR's fix round removes. The `data.reason` values are proposed (DEC-17); today these refusals carry only a code and a message. The refusal-signal column gives the code each PR head returns today; where the contract wants a different code, the row says so, marks the remap `[proposed]` and names its owner. PR heads as in §15.1.

| Method(s) | PR | Change | Class | Refusal signal | Who is affected; migration |
|---|---|---|---|---|---|
| `initialize` | #11185 | `clientCapabilities.turn_lifetime`; result `turn_lifetime` | A, C | none | none; opt-in |
| `session/list` | #11132 | optional `running` | A | none | none |
| `session/prompt` | #11132 | `turn_complete` gains `usage?`, `safeguard_fallback?` | A | none | none |
| `session/prompt` | #11132, #11234 | authority re-decided after every wait; a refused admitted prompt answers the error plus a failed `turn_complete` | S | `-32012` or `-32003`, `stale_authority` | a principal demoted while its prompt waits; clients end their working state on the failed `turn_complete` (zerocode already does, `zc/chat.rs:2629-2631`) |
| `session/prompt` | #11185 | frames on a per-session ring with `subscription_id`, `seq`; disconnect no longer cancels | C | none | opt-in only |
| `session/prompt`, `session/new` | #11149, #11176 | tools marked `requires_unrestricted_principal` (now also `cron_add`, `cron_update`, `cron_run`, `schedule`, `send_message_to_peer`) are withheld from every principal except the shared operator | S | the tool is absent from the turn (no RPC error) | named principals whose agents scheduled work through those tools; schedule as the shared operator, or through `cron/*` under the principal's own grants |
| `session/new`, `session/close`, `session/kill`, `session/delete`, `session/configure`, `session/messages` | #11234 | ownership judged by current authority after every wait; one uniform refusal | S | `-32012`, `session_not_owned` | a demoted administrator's queued operations lose the admin bypass |
| `session/new` | #11182 | an RPC-built agent's `canvas` tool draws into the daemon's shared store under `<agent>/<id>` instead of a private one | F | none | dashboards now see RPC agents' canvases; a scoped session still cannot reach the shared store (#11182 tests) |
| `session/cancel` | #11132 | the refusal branch no longer stalls the connection while a turn runs | F | none | none |
| `session/cancel` | #11185 | the owning principal may cancel from any client | W (TL-3) | none | none; relies on TL-3's acceptance |
| `session/close`, `session/kill`, `session/delete`, `subscription/cancel` | #11185 | end the session's viewers; leave the viewer set | C | none | session-lifetime viewers only |
| `logs/subscribe`, `events/subscribe` | #11185 | each line is sent only after writer room is reserved and authority re-decided; the stream ends if the policy generation moves three times | S | the stream ends; the next call gets `-32010`/`-32012` | long-lived dashboard streams re-subscribe after policy churn |
| `cron/add`, `cron/patch` | #11149 | `approved: false`: a command that needs approval is refused; rechecked under the lock | S | today `cron/add` answers `-32603` (`dispatch.rs:7640` @42b73abfc1) and `cron/patch` `-32602` (`:7695`, asserted at `:13566`); `[proposed]` remap both to `-32012` with `data.reason: "command_requires_approval"`, owned by a #11149 follow-up (adding a reason alone changes no code) | clients that created or edited medium-risk shell jobs over RPC; approve through the approval flow or change the command policy |
| `cron/add`, `cron/patch`, `cron/trigger` | #11149, #11176 | agent jobs are restricted to the shared operator; `allowed_tools` checked against the agent's policy | S | agent-job refusal `-32012` today (`dispatch.rs:1904-1917` @42b73abfc1), `data.reason: "agent_job_requires_shared_operator"` proposed; `allowed_tools` refusal `-32602` today (`dispatch.rs:959-990` @c0076cfe5d), `data.reason: "tool_outside_agent_policy"` proposed | named administrators and scoped principals who manage agent jobs; the shared operator (or `zeroclaw cron` locally) does it |
| `cron/add` | #11176 | optional `uses_memory`, `shell_output_format` | A | none | none |
| `cron/add` | #11176 | `prompt`, `job_type`, `session_target`, `model`, `allowed_tools`, `delete_after_run` take effect | F | none | a client that sent them expecting them to be ignored (none known) |
| `cron/patch` | #11176 | optional `enabled`, `uses_memory`, `shell_output_format`; the HTTP route's timezone rules; `command`/`prompt` map to the field the job runs | A, F | none | none |
| `personality/put` | #11176 | optional `expected_mtime_ms`; `-32005` only when it is sent and stale | A | `-32005`, `personality_disk_drift` | none unless the client opts in |
| `quickstart/validate`, `quickstart/apply` | #11176 | optional `surface` (absent → `tui`; `test` refused) | A | `-32602` for `test` | none |
| `quickstart/apply` | #11172 | commits through the commit gate; publishes a committed config even if a later step fails | S, F | `-32012` on revocation | none |
| `sops/decide` | #11169 | `name` optional; the approver is the connection's authenticated identity, not the claimed `tui_id`; rechecked under the engine lock | A, S | `-32012` | approval groups: each paired credential or principal counts once; a client that voted under another TUI id is refused. Operators re-check approval-group sizes: votes that used to count separately per TUI id now count once per credential |
| `sops/run`, `sops/decide` | #11220 | also require `tools:execute` | S | `-32012`, `tools_execute_required` | principals with `sops:execute` and no `tools:execute`: add `tools:execute` to their permission profile |
| `config/set`, `config/set-many`, `config/delete`, `config/map-key-create` | #11172 | each written path needs the Config verb of its own effect and the selector, checked before I/O and again at the file replacement; every accepted write sets `pending_reload` | S, A | the authority decision's code: `-32012`, or `-32010` when the credential was revoked meanwhile (`dispatch.rs:663-689` @cee32cf254); `data.reason: "config_path_outside_selector"` proposed | scoped config writers whose write reached paths outside their selector through side effects |
| `config/map-key-delete` | #11172 | alias deletion cascades (soft references scrubbed; hard references and live sessions refuse it); a scoped agent delete needs Delete on memory, cron and sessions and ownership of every session; result gains `warnings?` | F, S, A | `-32012`, or `-32602` with `referenced_alias` / `live_sessions` | callers that deleted aliases still in use (now refused, or references scrubbed) |
| `config/map-key-rename` | #11172 | Delete on `from` and Create on `to`; every referrer the cascade rewrites is authorized | S | `-32012` | scoped renamers |
| `agents/delete-preview`, `agents/delete` | #10621 (master after the baseline) | new methods | A | none | none |
| `config/set`, `config/map-key-rename` | #10621 (master after the baseline) | optional `comment` param; `rewritten` result field | A | none | none |
| `session/new` (resume) | #10246 (master after the baseline) | a remote (WSS) resume of a session holding local channel capabilities is refused; remote reconnects wait behind an active turn or get `-32002` | S | `-32003` ("Remote caller cannot use this session's local capabilities"); `-32002` when a remote reconnect cannot enter the session queue | remote clients that resumed such sessions; DEC-18 applies, because the check decides "remote" by transport |
| `health` | #11345 | result gains `components.gateway.bound_addr?` | A | none | none; the desktop app reads it before trusting the bound address |
| `memory/list`, `memory/search` | #11376 | optional `content_max_chars` | A | none | none |
| `memory/delete` | #11376 | `deleted` reports whether an entry was removed (always `true` on master) | F | none | a client that treated `deleted` as constant (none known) |
| `cron/delete`, `cron/runs` | #11376 | for the administrator grant, a job whose row is gone but whose run history remains is served as the HTTP routes serve it, instead of refused | W, F | none | none; scoped principals are unchanged |
| `cron/delete` | #11376 | an administrator's id with neither a job nor retained history: `-32602` becomes `-32603` | gap, **not additive**: the refusal code changes; `[pending fix round]` #11376 restores `-32602` | `-32603` ("Cron delete failed…", `rt/rpc/dispatch.rs:7354-7367` @e503e5cb8e); the baseline answered `-32602` from `authorize_cron_job` (`dispatch.rs:7607`, `:1531` @f0ae8c8bd8) | an administrator's client that read `-32602` as "no such job" sees an internal error; scoped principals are unchanged |
| `sops/decide` | #11373 | optional `pending_quorum: true`; the approver is the authenticated identity, as in #11169's row | A, S | none | as #11169's row |
| `sops/decide` | #11373 | an approval-group or approval-mode refusal is `-32012`, not `-32010` | F, **not additive**: the refusal code changes | `-32012` (`rt/rpc/dispatch.rs:10119-10128` @cec2b83bd1) | a client that read `-32010` from this method as a credential problem (none known: zerocode shows the message); #11169 keeps `-32010` and must take this change when the two merge |
| `sops/run` | #11373 | a procedure with an `execute` step no agent owns is refused before dispatch | F | `-32602` with the reason | none; such a run failed its first step before |
| `sops/subscribe-runs`, `sops/run-changed`, `system/version-check` | #11377 | new methods and notification | A | `-32603`, `data.reason: "sop_disabled"` | none |
| every method | #11377 | a handler error's `data` reaches the wire | A (DEC-17) | the error's own code | none; older clients ignore `data` |
| `status` | #11382 | optional `overview`, `agent`; result `overview?` | A | `-32012` when the caller cannot pass the agent's selector (only with `agent`) | none; an older core answers without `overview` `[pending fix round]` |
| `logs/query` | #11382 | optional `field_eq`, `report_disabled`; result `persistence_enabled`, `daemon_started_at?`, `attribution_keys?` | A | `-32602` for a `field_eq` key that is not an attribution field | none; an older core ignores both params `[pending fix round]` |
| `doctor/run` | #11382 | optional `static_only` | A | none | none; an older core runs the full suite, live probes included `[pending fix round]` |
| `session/messages`, `session/state`, `session/delete` | #11381 | optional `session_keys`; `SessionTargetParams` for state and delete | A | `-32602` for a malformed list | none; an older core ignores the keys and resolves the id with its fallback `[pending fix round]` |
| `session/messages` | #11381 | optional `max_bytes` | A | `-32602`, `data.reason: "entry_exceeds_max_bytes"` | none `[pending fix round]` |
| `session/messages`, `session/delete` | #11381 | the caller's authority re-checked after the session-queue wait, before the read or delete | S | the authority decision's code (`-32010` or `-32012`) | a caller whose credential is revoked while its request waits `[pending fix round]` (ownership on the fresh grants) |
| every method | #11381 | a handler error's `data` reaches the wire | A (DEC-17) | the error's own code | none; as #11377's row |
| `skills/delete` | #11384 | optional `purge` | A | none | none |
| `personality/list`, `personality/get`, `personality/put` | #11384 | optional `require_configured_agent` | A | `-32602` for an agent `[agents]` does not configure | none |
| `personality/put` | #11384 | optional `expected_mtime_ms`; `-32005` when it is sent and stale, as #11176's row | A | `-32005`, `data.error: "personality_disk_drift"`; at this head the error reaches the client without `data` | none unless the client opts in `[pending fix round]` |
| `personality/templates` | #11384 | optional overrides and `defaults` | A | none | none |
| `subscription/cancel` | this contract (DEC-31) | classified *own connection* instead of `Logs:Read` | W (DEC-31) | none | callers that hold a subscription but not `Logs:Read` can now end it; nobody gains access to another connection's subscriptions |
| `session/state` | this contract (DEC-22) | `turn_id` becomes the turn's random UUID instead of the turn-generation counter string | F | none | clients that parsed the counter (none known; it is documented as opaque) |

## Appendix E. Error codes by method

This completes SCH-5. **Every method** can return the gate's and the envelope's errors: `-32010` (not initialized, credential expired or revoked, re-validation due), `-32012` (the `(Resource, Verb)` grant or the service allowlist refuses), `-32600` / `-32700` (malformed request or frame), `-32601` (unknown method), and at the transport `-32004` before a connection is admitted (§8.16). `-32011` is `initialize`'s alone. The table adds what each handler itself can return at f0ae8c8bd8, found by a static read of the handler and the helpers it calls, three levels deep, in `rt/rpc/`. The lists are upper bounds; CT-SCH-03 requires every code a conformance run observes to be in its method's list, and a listed code that no path can produce to be removed. Meanings and HTTP mappings are in §8.16; `data.reason` values in §8.16 and Appendix D.

### E.1 Master methods

| Handler codes (beyond the gate) | Methods |
|---|---|
| none beyond the gate | `health` |
| `-32603` | `agents/list`, `agents/status`, `config/catalog`, `config/reload`, `config/sections`, `config/status`, `config/templates`, `config/validate`, `cost/org`, `cron/list`, `cron/settings`, `doctor/run`, `locales/list`, `quickstart/state`, `session/list-acp`, `skills/bundles`, `sops/list`, `sops/trigger-sources`, `status`, `tui/list` |
| `-32602`, `-32603` | `cert/renew`, `config/catalog-models`, `config/get`, `config/list`, `config/map-keys`, `config/resolve-alias-source`, `locales/fetch`, `logs/get`, `logs/query`, `quickstart/dismiss`, `quickstart/fields`, `quickstart/validate`, `session/list`, `skills/delete`, `skills/list`, `skills/read`, `skills/write`, `sops/get`, `sops/graph`, `sops/graph-draft`, `sops/run-overlay`, `sops/runs`, `sops/validate`, `sops/wire-draft`, `subscription/cancel`, `tools/param-options` |
| `-32010`, `-32602`, `-32603` | `cron/delete`, `cron/get`, `cron/runs`, `cron/trigger`, `sops/run-detail` |
| `-32010`, `-32012`, `-32603` | `events/history` |
| `-32012`, `-32602`, `-32603` | `events/subscribe`, `file/upload/chunk`, `logs/subscribe`, `session/approve` |
| `-32010`, `-32012`, `-32602`, `-32603` | `config/delete`, `config/map-key-create`, `config/map-key-delete`, `config/map-key-rename`, `config/set`, `config/set-many`, `cost/query`, `cron/add`, `cron/patch`, `memory/delete`, `memory/get`, `memory/list`, `memory/search`, `memory/store`, `personality/get`, `personality/list`, `personality/put`, `personality/templates`, `quickstart/apply`, `sops/decide`, `sops/run` |
| `-32000`, `-32012`, `-32602`, `-32603` | `file/upload/commit`, `session/git_branch`, `session/state` |
| `-32000`, `-32010`, `-32012`, `-32602`, `-32603` | `file/attach`, `file/upload/begin` |
| `-32010`, `-32011`, `-32012`, `-32602`, `-32603` | `initialize` |
| `-32000`, `-32003`, `-32012`, `-32602`, `-32603` | `session/cancel` |
| `-32000`, `-32002`, `-32012`, `-32602`, `-32603` | `session/close`, `session/configure`, `session/delete`, `session/kill`, `session/messages` |
| `-32010`, `-32012`, `-32020`, `-32602`, `-32603` | `sops/create` |
| `-32010`, `-32012`, `-32021`, `-32602`, `-32603` | `sops/delete`, `sops/save` |
| `-32010`, `-32020`, `-32021`, `-32602`, `-32603` | `sops/rename` |
| `-32010`, `-32012`, `-32602`, `-32603`, `4001`, `4003` | `fs/list_dir` |
| `-32000`, `-32002`, `-32010`, `-32012`, `-32602`, `-32603` | `session/prompt` |
| `-32000`, `-32001`, `-32002`, `-32010`, `-32012`, `-32602`, `-32603` | `session/new` |

### E.2 Proposed methods

| Method | Codes beyond the gate (`data.reason` in parentheses) |
|---|---|
| `gateway/register` | `-32602` (malformed `listen`), `-32600` (`already_registered`: a second registration on one service connection) |
| `gateway/status`, `webhook/routes` | `-32603` |
| `webhook/deliver` | none as errors: every delivery outcome is a result (§12.3); `-32602` (`duplicate_delivery_id`, malformed params) |
| `ingress/webhook`, `ingress/sop` | `-32010` (`auth_required`, `rate_limited`), `-32012` (`sop_credentials_required` when SOP dispatch is not allowed, `tools_execute_required`), `-32003` (`session_not_owned` on continuation), `-32602` (`duplicate_delivery_id`, unknown `agent`, `invalid_json`), `-32600` (`ingress_capacity`), `-32603` (`sop_dispatch_unavailable`, `needs_quickstart`) |
| `ingress/cancel` | none: unknown or finished ids answer `{cancelled: false}` |
| `pairing/redeem` | `-32010` (`rate_limited`, `pairing_code_invalid`), `-32600` (`pairing_disabled`) |
| `pairing/code` | `-32012` (admin or local transport required) |
| `pairing/device-capabilities` | `-32012` (`native_device_token_required`), `-32602` (`device_not_found`), `-32603` (`device_registry_disabled`) |
| `config/schema` | `-32602` (`path_not_found`) |
| `canvas/subscribe` | `-32600` (`canvas_capacity`), `-32012` (the every-agent rule) |
| `session/cancel-turn` | `-32000` (no such session), `-32003` (not the owner), `-32602`; a `turn_id` that is not running is `{cancelled: false}`, not an error |
| `session/abort-turn` | `-32000`, `-32012` (not the owner and no admin bypass), `-32602`; a `turn_id` that is not running is `{cancelled: false}` |

The `ingress/*` refusals keep today's HTTP bodies: the gateway maps each `data.reason` to the status and body the in-process route returns now (401, 403, 404, 429 with `retry_after`, 503 `needs_quickstart`), which CT-ING-06 checks byte for byte.

### E.3 Methods added by open PRs

Codes each handler and the helpers it calls can return, beyond the gate, at the heads in C.4 and C.6 (a static read of the handlers, as in E.1). Where a cell names the gate it is for completeness; every method also gets the gate's `-32010`/`-32012`. SCH-6 keeps these lists current until the PRs merge.

| Method | PR @head | Error codes |
|---|---|---|
| `session/steer` | #11132 @fad6b15824 | gate (sessions:execute): -32010 AUTH_REQUIRED, -32012 FORBIDDEN. handler: -32602 INVALID_PARAMS (parse; blank `content`; id matches both ACP and Chat durable history); -32012 FORBIDDEN (not owned / unknown id for a scoped principal; steering admission refused, which re-codes a credential or revalidation denial from the admission check as -32012); -32002 SESSION_BUSY ("Steering queue is full for the running turn"; the per-turn queue holds 32 messages, `rt/rpc/dispatch.rs:5646 @fad6b15824`); -32000 SESSION_NOT_FOUND ("No active turn for this session", including a session with no live incarnation and a turn of a different incarnation); -32603 INTERNAL_ERROR (owner lookup failed; owner records disagree for an unscoped caller; result serialization) |
| `session/abort` | #11132 @fad6b15824 | gate (sessions:delete): -32010, -32012. handler: -32602 (parse; ambiguous ACP+Chat id); -32012 (not owned; post-lookup recheck lost `sessions:delete`; ownership re-held under fresh grants); -32010 (post-lookup recheck: credential expired, revalidation due, pairing revoked, generation race); -32000 ("No active turn for this session" for the authorized incarnation); -32603 (owner lookup; ambiguous owners; serialization) |
| `session/append` | #11132 @fad6b15824 | gate (sessions:update): -32010, -32012. handler: -32602 (parse; blank `content`; ACP session; id names a gateway/channel session or an `rpc_`-prefixed alias rather than an RPC chat session addressed by its own id; ambiguous ACP+Chat id); -32012 (not owned; post-queue recheck/ownership); -32010 (post-queue recheck); -32002 ("Session busy: ..." when the session queue cannot be acquired); -32000 (no durable chat row; unscoped caller whose incarnation was replaced while queued); -32603 ("Session persistence is disabled"; append failed; owner lookup; serialization) |
| `session/rename` | #11132 @fad6b15824 | gate (sessions:update): -32010, -32012. handler: -32602 (parse; blank `name`; >200 chars; ACP session "session/rename is not supported for ACP sessions"; ambiguous ACP+Chat id); -32012 (not owned); -32000 (no durable chat row); -32603 ("Session persistence is disabled"; rename write failed; owner lookup; serialization) |
| `session/run-once` | #11132 @fad6b15824 | gate (sessions:execute): -32010, -32012; plus a required `sessions:create` grant (-32012; its recheck can give -32010). handler and helpers (`session_new_with_mode` create-only, `run_session_prompt`): -32602 (parse; blank `prompt`; `session_id` already exists; create-only id raced into existence); -32012 (agent not entitled; workspace not permitted; not owned; prompt admission refused); -32010 (post-wait rechecks); -32002 (session queue busy; "Session resume already in progress"; connection closed before prompt admission/execution; provider-generation wait timeout); -32001 SESSION_LIMIT_REACHED; -32000 (incarnation replaced before the prompt; session vanished); -32603 (agent construction failed; turn failed, message = the turn's user message or error text; persistence failures; serialization) |
| `session/update` `turn_complete` (notification) | #11132 @fad6b15824 | none |
| `session/attach` | #11185 @6713ff419b | gate (sessions:read): -32010, -32012. handler: -32602 (parse; `since_seq` ahead of the ring head when `epoch` matches; id matches both ACP and Chat durable history); -32012 (scoped caller: unknown or not-owned session); -32000 ("Session not found" for an unscoped caller when no live or durable record exists; "Session changed while attaching" when the ring was retired between lookup and join); -32603 (owner lookup failed; owner records disagree; serialization). Losing authority later ends the stream silently (no error frame) |
| `initialize` (addition) | #11185 @6713ff419b | no new codes (initialize keeps -32602, -32011 VERSION_MISMATCH, and the handshake's -32010/-32012) |
| `session/update` ring delivery (addition) | #11185 @6713ff419b | none (a refused disclosure ends or narrows the stream silently) |
| `skills/create` | #11176 @c0076cfe5d | gate (skills:create): -32010, -32012. handler: -32602 (parse; name not a single directory name; `resolve_ref` refusal such as unknown bundle: "Invalid skill ref: ..."); -32603 ("Skill create failed: ..." for any scaffold error, including an existing skill; serialization). No agent selector |
| `skills/effective` | #11176 @c0076cfe5d | gate (skills:read): -32010, -32012. handler: -32602 (parse); -32012 (agent selector: not entitled to `agent`, or `agent` not configured for a non-admin); -32010 (selector on an unbound dispatcher only; unreachable after the gate); -32603 ("Skill resolution failed: ..." when a bundle directory cannot be resolved; serialization) |
| `skills/slash-option-kinds` | #11176 @c0076cfe5d | gate (skills:read): -32010, -32012. handler: -32603 (serialization only) |
| `personality/put` (addition) | #11176 @c0076cfe5d | gate (personality:update): -32010, -32012. handler: -32602 (parse; filename not in the editable allowlist SOUL.md, IDENTITY.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md, MEMORY.md; content > 20000 chars); -32012 (agent selector); -32005 PRECONDITION_FAILED (new; message "<filename> changed on disk since it was read"); -32603 ("Read failed: ..." during the drift check; "Write failed: ..."; serialization). `data` reaches the client because #11176 routes inline handler errors through `send_rpc_error` (dispatch.rs:10211), which keeps `data` |
| `sops/cancel` | #11169 @908a035d3f | gate (sops:execute): -32010, -32012. handler: -32602 (parse; "run '<id>' not found" before or under the engine lock; run belongs to another SOP); -32012 (agent selector for an agent of the run's procedure, before and after the post-lock recheck); -32010 (run's procedure not loaded and caller not admin: "...the run's procedure is not loaded, so its agents cannot be authorized"; post-lock recheck credential failures); -32603 ("SOP subsystem not enabled"; engine lock poisoned; "cancellation could not be durably persisted; the run remains active - retry"; other engine errors; "run disappeared after cancellation"; serialization) |
| `sops/dispatch-event` | #11169 @908a035d3f | gate (sops:execute): -32010, -32012. handler: -32602 (parse; empty `path`); -32012 (preflight with stamped grants: constrained tool selector, or not entitled to an agent a matched SOP runs as; and when every result is an admission refusal, message = first result's `reason`, which starts with "not authorized: "); -32603 ("SOP subsystem not enabled" when engine or audit is missing or dispatch is unavailable; engine lock poisoned; serialization). A recheck failure at admission (including an expired credential) is not an error code: it becomes a `skipped` entry, or -32012 if every entry is one |
| `sops/decision-models` | #11169 @908a035d3f | gate (sops:read): -32010, -32012. handler: -32603 (serialization only) |
| `sops/graph-legend` | #11169 @908a035d3f | gate (sops:read): -32010, -32012. handler: -32603 (serialization only) |
| `config/reload-status` | #11172 @cee32cf254 | **`-32603`.** `to_result` only; unreachable for this struct |
| `config/drift` | #11172 @cee32cf254 | **`-32603`.** `to_result` only; unreachable |
| `config/agent-options` | #11172 @cee32cf254 | **`-32603`.** `to_result` only; unreachable |
| `config/section-picker` | #11172 @cee32cf254 | **`-32602`, `-32603`.** `-32602`: `parse_params`; `data.code` `path_not_found` (`path` = the section as sent) for a section `Section::from_key` does not know, and (`path` = canonical key) for a direct-form section (`hardware`, `mcp`, `skills`, `onboard_state`), `rt/config_ops/sections.rs:17-49`. `-32603`: `to_result` (unreachable) |
| `config/section-select` | #11172 @cee32cf254 | **`-32010`, `-32012`, `-32602`, `-32603`.** `-32602`: `parse_params`; `data.code` `path_not_found` (unknown or direct-form section, from `select_target_path` before any authority check; `create_map_key` refusal; invalid alias key), `validation_failed` (reserved alias; agent workspace scaffold failure; `memory.backend` / `tunnel.tunnel_provider` set failure), `config_changed_externally` (a no-op memory or tunnel selection whose paths drifted on disk, or whose canonical file cannot be read or parsed for that check), `rt/config_ops/sections.rs:64-324`; `validate_config_auth` in the save (no `data`). `-32010`, `-32012`: `selector_config_write(target)` before the lock; `authorize_config_write_set([(target, Update for memory.backend / tunnel.tunnel_provider, else Create)])` after the lock; `authorize_config_effects` (every dirty path by effect, plus `fields_prefix` pinned to Create when `created`); `save_and_swap_config`'s pre-I/O check and the `RpcCommitGate` at the file replacement. `-32603`: save failure (`Config save failed: …`, no `data`); `to_result` |
| `config/delete-plan` | #11172 @cee32cf254 | **`-32602`, `-32603`.** `-32602`: `parse_params`; `data.code` `op_not_supported` (`path` = `<path>.<key>`) when `path` is `providers.tts.<family>` or `providers.transcription.<family>`, `rt/config_ops/delete.rs:99-102`. `-32603`: `to_result` (unreachable) |
| `config/init` | #11172 @cee32cf254 | **`-32010`, `-32012`, `-32602`, `-32603`.** `-32602`: `parse_params`; `data.code` from `scoped_validate` (`rt/config_ops/document.rs:33-68`) over `Config::validate()`: structured `invalid_numeric_range`, `required_field_empty`, `invalid_format`, `dangling_reference`, `validation_failed` (`validation_bail!`, `cfg/schema.rs`), or classified from a plain message: `value_type_mismatch`, `path_not_found`, `validation_failed` (`cfg/api_error.rs:138-147`); refused only when the failing path touches an initialized section or has no path; `validate_config_auth` in the save (no `data`). `-32010`, `-32012`: `selector_config_write(section)` when `section` is sent (before the lock); `recheck_config_write_authority(section)` after the lock (without `section`: liveness and coarse grant only); `authorize_config_effects` (dirty paths by effect, each initialized section pinned to Create); `save_and_swap_config`'s pre-I/O check and the `RpcCommitGate`. `-32603`: save failure (no `data`); `to_result` |
| `config/migrate` | #11172 @cee32cf254 | **`-32010`, `-32012`, `-32602`, `-32603`.** `-32010`, `-32012`: `selector_config_write("*")` before the lock (whole-config authority: a path-scoped selector cannot pass); `recheck_config_write_authority(Some("*"))` after the lock; the `RpcCommitGate` (`writes: [("*", Update)]`) at the rename, whose refusal is returned as-is (`gate.take_refusal()`, `rt/rpc/dispatch.rs:8673-8675`) in place of the migration's own `internal_error`. `-32602`: `data.code` `validation_failed` (`migration failed: …`, or the authorization-policy rejection from `validate_config_auth`, re-wrapped by the `accept` closure, `rt/rpc/dispatch.rs:8662-8669`). `-32603`: `data.code` `internal_error` (read, temp write or fsync, backup, parent or file name, failed rename; `rt/config_ops/document.rs:116-250`), `reload_failed` (re-parse after migration); `to_result` |
| `providers/refresh-context-window` | #11172 @cee32cf254 | **`-32010`, `-32012`, `-32602`, `-32603`.** `-32602`: `parse_params`; `data.code` `path_not_found` (`path` = `providers.models.<type>.<alias>`: no `.model` prop at the pre-fetch snapshot, or the profile vanished during the unlocked fetch), `invalid_format` (provider cannot auto-detect, or the fetch failed; same `path`), `rt/config_ops/context_window.rs:33-109`; `validate_config_auth` in the save (no `data`). `-32010`, `-32012`: `selector_config_write(target)` with target `providers.models.<type>.<alias>.context_window`, checked before the existence check and the network fetch; `recheck_config_write_authority(target)` after the lock; `authorize_config_effects`; `save_and_swap_config`'s check and the `RpcCommitGate`. `-32603`: `data.code` `internal_error` (`set_prop_persistent` failure; `path` = profile path); save failure (no `data`); `to_result` |
| `workspace/list` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4003 |
| `fs/mkdir` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4002, 4003 |
| `fs/rmdir` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4002, 4003 |
| `fs/read` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4003 |
| `fs/delete` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4002, 4003 |
| `fs/move` | #11182 @b0ad81bfd2 | -32010, -32012, -32602, -32603, 4001, 4002, 4003 |
| `tools/cli-discover` | #11182 @b0ad81bfd2 | none (a failed discovery worker yields `cli_tools: []`, logged) |
| `tools/list` | #11182 @b0ad81bfd2 | -32012, -32602, -32603 |
| `integrations/list` | #11182 @b0ad81bfd2 | none |
| `plugins/list` | #11182 @b0ad81bfd2 | -32603 (reachable only in a `plugins-wasm` build; N6) |
| `a2a/identity` | #11182 @b0ad81bfd2 | -32012, -32600, -32602, -32603 (serialization only, not reachable in practice; N5) |
| `canvas/list` | #11182 @b0ad81bfd2 | -32012 |
| `canvas/get` | #11182 @b0ad81bfd2 | -32012, -32602 |
| `canvas/history` | #11182 @b0ad81bfd2 | -32012, -32602 |
| `canvas/render` | #11182 @b0ad81bfd2 | -32012, -32600, -32602 |
| `canvas/clear` | #11182 @b0ad81bfd2 | -32012, -32602 |
| `metrics/scrape` | #11182 @b0ad81bfd2 | none |
| `pairing/list` | #11182 @b0ad81bfd2 | `-32010` (unbound, `require_admin`); `-32012` (not admin); `-32603` (HTTP 500: device registry cannot be opened or listed) |
| `pairing/revoke` | #11182 @b0ad81bfd2 | `-32010` (unbound, or the recheck under the config write lock finds the credential expired, revoked or unpaired); `-32012` (not admin, before or after the lock; the recheck finds `system:delete` or entitlement lost); `-32600` (HTTP 503 `"Device registry is disabled"`: pairing not required, so no registry); `-32602` (params; HTTP 404 `"Device not found"`); `-32603` (HTTP 500: registry open/revoke error, or `"Token revoked in memory but config persist failed: …"`) |
| `pairing/revoke-all` | #11182 @b0ad81bfd2 | `-32010` (unbound, or the recheck under the lock); `-32012` (not admin; connection not `TransportKind::Local`; recheck); `-32600` (HTTP 400 `"Pairing is disabled for this gateway"`); `-32603` (HTTP 500: registry open/clear, config persist, or code generation failed) |
| `pairing/new-code` | #11182 @b0ad81bfd2 | `-32010` (unbound, or the recheck under the lock); `-32012` (not admin; connection not `TransportKind::Local`; recheck); `-32600` (HTTP 400 pairing disabled; HTTP 503 registry disabled on a single-device rotate, which the RPC path cannot reach, see Notes); `-32602` (params; HTTP 404 `"Device '<id>' not found; nothing revoked."` for `rotate: <device id>`); `-32603` (HTTP 500) |
| `channels/list` | #11182 @b0ad81bfd2 | `-32600` (no `ChannelControl` registered: `"channels/list is not available: this process runs no channels"`). The body builder is infallible |
| `channels/relink` | #11182 @b0ad81bfd2 | `-32600` (no `ChannelControl`; HTTP 409: the type has no QR relink or its feature is not compiled); `-32602` (params; HTTP 404: no channel has that composite name); `-32010` (unbound, `authorize_channel_owner`); `-32012` (caller may not use the owning agent; a channel with no owner, or an unknown name, needs access to every agent); `-32603` (HTTP 500: `"failed to clear persisted login: …"`) |
| `channels/bind` | #11182 @b0ad81bfd2 | `-32600` (no `ChannelControl`); `-32602` (params; channel type other than `telegram`/`wechat`/`line`; alias not configured; empty identity or an `ignore` entry denies it); `-32010` (unbound; the recheck under the config write lock finds the credential expired, revoked or unpaired); `-32012` (owning-agent check before and after the lock; recheck loses `channels:update`; the write needs `config:create` (new peer group) or `config:update` and write access to `peer_groups.<group>.external_peers`); `-32603` (the persisted peer policy cannot be read; `"save failed: …"`) |
| `system/upgrade` | #11182 @b0ad81bfd2 | `-32010` (unbound, `require_admin`); `-32012` (not admin); `-32602` (params; HTTP 400: `auto_restart: true` where the detected restart mode is `manual`); `-32600` (HTTP 403: `gateway.allow_self_upgrade` is false; HTTP 409 `"an upgrade is already in progress"`) |
| `system/upgrade-status` | #11182 @b0ad81bfd2 | `-32602` (params; HTTP 404 `"unknown handoff_id"`: `handoff_id` differs from the current or most recent upgrade's). Not admin-gated |
| `system/restart` | #11182 @b0ad81bfd2 | `-32010` (unbound, `require_admin`); `-32012` (not admin); `-32602` (params; `component` other than `"daemon"`); `-32600` (`"no daemon supervisor is attached; restart the process instead"`: no `reload_tx` in the context) |
| `system/version-check` | #11377 @8b2ed4ef5c | gate (system:read): -32010, -32012. handler: -32602 (params); after the check the caller's authority is resolved again: -32010 (credential expired, revoked or due for revalidation), -32012 (grant withdrawn). A failed check is a result with `error`, not an error |
| `sops/subscribe-runs` | #11377 @8b2ed4ef5c | gate (sops:read): -32010, -32012. handler: -32602 (params); -32603 (`sop_disabled` in `data.reason`; "engine lock poisoned" without `data`; result serialization) |
| `status` (addition) | #11382 @967865aeae | handler, with `overview` and `agent`: -32012 when the caller cannot pass the agent's selector. Without `overview` the method answers as on master |
| `logs/query` (addition) | #11382 @967865aeae | handler: -32602 (a `field_eq` key that is not an attribution field); -32603 when persistence is off, unless `report_disabled` |
| `session/messages`, `session/state`, `session/delete` (addition) | #11381 @a3b5f3920a | handler: -32602 (`session_keys` empty, over 4 or holding an empty key); `session/messages`: -32602 with `data.reason: "entry_exceeds_max_bytes"`; a stored-transcript read or a delete re-checks authority after its queue wait: -32010, -32012 |
| `personality/list`, `personality/get`, `personality/put` (addition) | #11384 @00a5c1ae52 | handler: -32602 (`require_configured_agent` with an agent `[agents]` does not configure, or none); `personality/put`: -32005 as #11176's row |

### E.4 Methods added on master after the baseline

| Method | Codes beyond the gate |
|---|---|
| `agents/delete-preview` | `-32602` (params), `-32603` (the preview task failed) |
| `agents/delete` | `-32602` (params; another agent lifecycle mutation holds the alias; applying the scrubs failed), `-32603` (the preflight or cleanup task failed); a blocked delete is a result |
