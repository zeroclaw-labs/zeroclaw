---
id: ADR-019
title: The gateway reaches the core only through the authenticated local RPC contract
date: 2026-09-30
status: proposed
relates-to:
  - ADR-007
  - ADR-017
  - ADR-018
  - ADR-012
  - https://github.com/zeroclaw-labs/zeroclaw/issues/11000
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7432
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8691
  - https://github.com/zeroclaw-labs/zeroclaw/issues/11001
  - https://github.com/zeroclaw-labs/zeroclaw/issues/11002
  - https://github.com/zeroclaw-labs/zeroclaw/issues/11003
  - https://github.com/zeroclaw-labs/zeroclaw/issues/11004
  - docs/book/src/architecture/core-gateway-ipc-contract.md
  - docs/book/src/architecture/rpc-socket.md
  - crates/zeroclaw-api/src/jsonrpc.rs
  - crates/zeroclaw-api/src/webhook.rs
  - crates/zeroclaw-runtime/src/rpc
  - crates/zeroclaw-gateway
---

# ADR-019: The Gateway Reaches the Core Only Through the Authenticated Local RPC Contract

## Context

[ADR-007](./ADR-007-gateway-extraction.md) decides that the gateway becomes a separate, optional `zeroclaw-gw` process that reaches the agent runtime only through an authenticated, versioned, documented local IPC contract. It does not define that contract. Issue #11000 asks for it before the dependent work in #11001 (IPC parity), #11002 (standalone gateway), #11003 (plugin webhooks across IPC) and #11004 (desktop supervision).

The starting point at `upstream/master` f0ae8c8bd8:

- The core serves JSON-RPC 2.0 framed as newline-delimited JSON over a Unix domain socket (`<data_dir>/daemon.sock`) or a data-directory-derived Windows named pipe (`crates/zeroclaw-runtime/src/rpc/local.rs`); `ZEROCLAW_SOCKET` overrides both. It has 97 methods, a compile-forced authorization classification for each (`Method::authz()`), principal-bound connections ([ADR-017](./ADR-017-inbound-authentication-and-principals.md)), transport bounds and chunked uploads, and a replayable subscription hub. The generated OpenRPC document (#11165) fully types 57 of the 97 methods. Master has since gained two more, `agents/delete-preview` and `agents/delete` (#10621); the contract records them separately from its pinned baseline.
- No gateway route calls that interface. Of the gateway's 174 HTTP, WebSocket and SSE routes, 40 have an exact RPC equivalent, 45 a partial one and 89 none. The gateway runs agent turns itself for chat, `/webhook`, A2A and vendor webhooks. It receives the daemon's pairing guard, inbound-auth state, decrypted configuration, event bus and plugin webhook registry as shared in-process objects; it owns the webhook idempotency store; it mints and persists pairing tokens; and 97 of its routes authorize with a native-bearer check that is skipped when pairing is off.
- An RPC disconnect cancels the turns that connection started, a daemon reload retires every RPC connection, and cancellation targets "whatever turn is current" in a session.
- RFC #5574 asks for an "OpenAPI 3.1" contract, while #7432 requires a core artifact with no HTTP server.
- Endpoint discovery exists in three copies that disagree (the daemon, zerocode and #11186's client), the Windows pipe name comes from a hash the Rust standard library does not keep stable across releases, and the listener keeps its socket owner-only, so another OS account cannot reach it.

Open work already extends the interface: `zeroclaw-rpc-proto` with the OpenRPC document (#11165), `zeroclaw-rpc-client` with an in-process seam (#11186), and open PRs that together add 49 methods and change 27, several of them security corrections that narrow what protocol 1 used to admit.

The contract itself, with the operation catalog, route-to-method table, wire shapes, error lists, compatibility register, conformance test index and decision register, is the [core-to-gateway IPC contract](../core-gateway-ipc-contract.md). This record keeps the decisions, the options that were rejected, and the consequences.

### Options considered

- **Describe the core transport in OpenAPI 3.1.** Rejected: it needs an HTTP server in the core, or a document that does not describe the wire.
- **A new HTTP, gRPC or WebSocket IPC.** Rejected: it puts a server framework back in the core or adds TLS to a same-host hop, and duplicates authentication, authorization classification, bidirectional requests and the Windows transport.
- **A service principal acting on behalf of users.** Rejected: it is the parallel authority ADR-017 forbids.
- **A custom `server_proof = HMAC(key, nonce)` handshake.** Rejected: in a credential-first handshake a fake listener receives the key it is asked to prove, and one proof does not cover later pooled connections.
- **Credential-less gateway connections as a convenience, including for public routes.** Rejected: the core resolves them to the shared operator. Public routes use the service connection and one allowlisted method each.
- **A gateway-held idempotency cache.** Rejected: the plugin worker reserves and commits mid-delivery inside the core, and a per-gateway-run cache loses deduplication at every gateway restart.
- **Keeping `record_if_new` for generic webhooks while promising at-least-once delivery.** Rejected: it consumes a key before dispatch, so a refused request suppresses its own retry.
- **Correlating ingress streams by session id or JSON-RPC id.** Rejected: a new session's id is unknown until the call finishes, and JSON-RPC ids correlate responses, not notifications.
- **One delivery per connection instead of per-delivery cancellation.** Rejected: it multiplies connections under load and still needs a cancellation rule on shared connections.
- **Verifying `gateway.webhook_secret` at the gateway edge.** Rejected: it puts a decrypted core secret in the internet-facing process and splits the pairing-and-secret conjunction across two processes.
- **A header allowlist on forwarded plugin webhooks.** Rejected: vendor signatures use arbitrary headers, and Gmail's credential is in `Authorization`.
- **Moving the pairing and webhook keys into new config sections at V4.** Rejected for v0.9.0: the rename machinery has open migration hazards (rename after edit, environment aliases, downgrade), and ownership can be stated without moving keys.
- **Redefining protocol 1 so that every disconnect keeps turns running.** Rejected: it silently changes behaviour for existing clients.
- **A new protocol version for every security correction.** Rejected: old clients would keep the hole the correction closes.
- **A generic multi-protocol connection upgrade for ACP and future media.** Deferred: ACP needs principal-aware execution first.

## Decision

Every decision below is **proposed**. Each names its entry in the contract's decision register, which gives the contract section and the code, PR or test that will enforce it.

### Reuse the existing local transport (DEC-01)

We will connect the gateway to the core over the existing local listener. We will not add a second local server, an HTTP listener in the core, or a second authority path.

### Keep three documents with separate jobs, and specify every payload (DEC-02, DEC-15, DEC-28)

The contract is normative for framing, connection states, limits, handshake, credential routing, discovery, version policy, errors, subscriptions, turn lifetime, reconnect, process lifecycle and ingress. The OpenRPC document generated from `zeroclaw-rpc-proto` is the machine-readable schema. OpenAPI 3.1 describes only the gateway's external HTTP surface and is served only by the gateway; it does not describe the core transport, which has no HTTP methods, paths or status codes, carries requests in both directions on one stream, and interleaves notifications with responses.

The contract gives the wire shape of every payload the OpenRPC document leaves untyped, for the master methods, the proposed methods and the 49 methods the open PRs add (pinned to their heads), and every method's error codes; the proto crate's schemas must match them. A PR that adds or changes a method types it, with its error list, in the same PR, and keeps the contract in agreement.

### Keep authority in the core (DEC-03, DEC-14)

The core will decide every allow or deny on domain state, from the principal bound to the connection, `Method::authz()`, the method's selectors and ownership predicates and, for ingress, evidence the core verifies itself. The gateway applies only transport policy that narrows (bind, body caps, rate limits, timeouts). After the split the gateway contains no inbound-auth layer, pairing authority, grant evaluation, secret comparison or vendor verification; each piece of state has one owner, and the gateway holds transport state only. The dependency ratchet that proves this runs over the gateway binary's transitive dependency graph.

### Bind each connection to one caller's credential (DEC-04, DEC-05, DEC-25)

The gateway will run each end user's operation on a connection initialized with that user's own credential, one credential per connection, re-presented on every connect. No method will carry an on-behalf-of field. Ingress that carries its own credential (`/webhook`, `/sop/*`, plugin and vendor webhooks) crosses as evidence on ingress methods, and the core's ingress handler stamps the turn's provenance ([ADR-018](./ADR-018-runtime-security-and-provenance.md)).

**No absent, blank, invalid or unsupported-provider credential will ever become a tokenless core connection**, including in health probes and reconnect paths: the core would admit such a connection as the shared operator through the trusted daemon uid or local compatibility. Public routes (health, metrics, A2A cards, the config schema, pairing redemption and posture) never take a caller credential: the gateway serves each through its service connection with the one method that route needs, and the core handler enforces the route's own publication rule.

### Verify the endpoint's OS identity before sending any credential (DEC-06, DEC-27)

Before a client writes any frame that contains a reusable credential, on every dial including reconnects, it will verify who serves the endpoint: the peer uid of the Unix socket and the ownership of its directory, or the user SID (and, in child mode, the pid) of the named-pipe server process. The core will create every pipe instance with an explicit DACL. The check stops another local user's squatter or a stale `ZEROCLAW_SOCKET`; it cannot stop malware running as the core's own account, and the contract says so.

Two trust boundaries are specified: same OS account and separate accounts. A separate-account gateway needs its own layout: an IPC directory outside the private data directory, owned by the core, that grants the gateway's group only traversal, the socket and its read-only bootstrap and key, and that the listener applies on every bind and fails closed on; Windows adds a roster and a DACL entry. Which boundaries v0.9.0 supports is open (D11).

### Discover the endpoint one way (DEC-07)

Every client will resolve the endpoint through one implementation in `zeroclaw-rpc-client`: an explicit endpoint from the launcher, then the platform default for the data directory resolved with the core's precedence. Launchers pass the endpoint explicitly. The Windows pipe name moves to a specified SHA-256 derivation in one lockstep release. No config key is required to find the endpoint.

### Keep protocol version 1 additive, with registered security corrections (DEC-08, DEC-17, DEC-29)

Within protocol 1 we will make only additive changes, and clients ignore unknown fields, notifications and event types. Two kinds of non-additive change also stay inside protocol 1, because keeping the old behaviour would keep a defect alive: **security corrections**, which only narrow what the documented authority model never granted, fail closed and carry a stable refusal signal; and **conformance corrections**, where a method starts doing what its parameters or its HTTP twin already promised. Each is entered in the contract's compatibility register with who is affected and how supported clients and configurations migrate, and repeated in the release notes. A change that widens authority is allowed only as the implementation of an accepted rule. Any other semantic change needs an opt-in capability (as #11185's `turn_lifetime`) or a new protocol version. A gateway degrades per route when an optional method is missing and refuses to serve when a mandatory method or a configured security property is missing. `-32011` carries structured data, and error `data`, when present, carries a stable `reason`.

### Let sessions own turns, identify every turn, and cancel explicitly (DEC-09, DEC-22, DEC-31)

For clients that opt in, a turn will belong to its session, not to the connection that submitted it. Viewers attach and replay; producers never wait for viewers; cancellation is explicit and authorized by session ownership. Every turn gets a core-assigned random-UUID `turn_id`, announced before its first event. Targeted cancellation is a separate pair of methods (`session/cancel-turn`, `session/abort-turn`) that a client uses only after the core advertises them, because an older handler would ignore an added target field and cancel whatever turn is current; a retried targeted cancel never stops a later turn, and a daemon restart, which a new `daemon_instance_id` reveals, voids every pending retry of a transient id. Methods that name a session only by its id are never replayed after an unknown outcome, because a removed id can be re-created. A caller may always end a subscription its own connection opened, whatever grant opened it, and never another connection's. Clients that do not opt in keep connection-lifetime turns. A daemon reload or shutdown still cancels in-flight turns with a recorded cause, and the drain counts session-owned turns.

### Forward webhooks as raw bounded requests with a delivery identity (DEC-10, DEC-11, DEC-23)

The core keeps the plugin route table, its generations, deadlines and body re-checks. The gateway forwards `/plugin/{path}` requests unchanged through `webhook/deliver` and maps a closed outcome enum back to today's HTTP statuses. Every ingress call carries a gateway-chosen `delivery_id`: it tags the call's stream events, so concurrent streams on one connection never mix, and it is the target of `ingress/cancel`, so one caller leaving cancels only its own delivery. Closing a connection cancels only the deliveries it carried. Registry sends are fenced to the live generation; unloading or replacing a plugin takes effect at the next channel-supervisor generation.

### Own webhook deduplication in the core, committing at admission (DEC-12, DEC-24)

The core will own every ingress idempotency store at daemon-process scope, together with plugin keys that include the owning endpoint instance, so deduplication survives gateway restarts and reloads and a reassigned path does not inherit another plugin's records. Plugin deliveries are at-least-once up to the point the message is enqueued, and deduplicated after it. Generic `/webhook` and `/sop/*` keys are reserved after verification, committed when the work is admitted and rolled back on every refusal before that, so a refused request never suppresses its own retry; after admission a key means "admitted once". Everything is TTL-bounded and lost on a core restart; nothing is exactly-once.

### Check `/webhook` and `/sop/*` in the core (DEC-13)

`ingress/webhook` and `ingress/sop` will own every authorization check the gateway makes today: the pairing-bearer and secret truth table from one policy snapshot, the auth-attempt limiter, the fail-closed SOP credential rule, SOP-first dispatch, `?agent=` validation, idempotency and session continuation. Only the per-client rate limit stays at the edge. `gateway.webhook_secret` is verified in the core; the gateway never reads it. The HTTP bodies stay byte for byte: the core returns everything they need, including the chat reply's configured-model label, which only the core can compute, and `/sop/*`'s 404 for a path no SOP matches, which never falls through to chat.

### Project the pairing posture, never the pairing authority (DEC-26)

The gateway learns whether pairing is required, whether any device is paired, and whether a change waits for the next reload from a read-only projection the core sends at registration and on change. It uses the projection only for presentation; every authorization stays in the core.

### Keep today's config names and record one owner per key (DEC-21)

The split needs no config key rename or move. Keys keep their names, and the contract records the canonical owner of each: the pairing fields, `webhook_secret`, the idempotency settings, session persistence and self-upgrade are core-owned and never read by the gateway; bind, TLS, rate-limit, timeout and keepalive settings are gateway edge policy. Config schema V4 only retires the inert `[gateway.pairing_dashboard]`. New keys the split needs (a supervision mode, and the endpoint and gateway group of the separate-account layout) are additive.

### Treat "local transport" as "same OS account", not "a local human" (DEC-18)

Checks that use the local transport as evidence of an operator at the machine will be re-expressed as "the peer runs as the daemon's own OS account", because a gateway relays remote browsers over the local socket.

### Fix the semantics where open PRs disagree (DEC-19, DEC-20)

SOP execution over RPC requires `tools:execute` (#11220's predicate survives the #11169 merge). The five session methods duplicated in #11132 and #11185 take #11132's semantics.

### Open decisions for the maintainers

These have no default. The recommendation is the author's.

| # | Decision | Options | Recommendation |
|---|---|---|---|
| D1 | The gateway's own identity | A: `service` auth provider, core-minted key file, and a method-allowlist profile checked in addition to grants (DEC-16); B: a roster-mapped OS account (Unix only); C: none, a credential-less control connection that resolves to the shared operator and so keeps DEC-05 only for HTTP-caller traffic | A, with no pairing-code delivery to that profile |
| D2 | Serving callers when pairing is disabled | A: a core-controlled anonymous connection for requests with no credential at all, specified in DEC-30; B: credential-less connections; C: require pairing for the separate gateway | C for v0.9.0 and until D1-A ships (pairing-disabled installs keep the in-process gateway); A after that |
| D3 | Owner and origin of webhook- and channel-originated sessions | shared operator with an origin tag; a per-channel service principal; the mapped sender under an explicit channel policy | mapped sender under explicit policy, shared operator only as a declared legacy mode |
| D4 | Approvals with no viewer attached | deny immediately (#10538, and #11185 today); park until the existing deadline and replay; split by cause | park and replay, deadline never reset |
| D5 | Supervision mode and its config key | `child` / `external` / `disabled`, name and default | `child` default; `external` for hardened and desktop layouts |
| D6 | Product-version skew | exact; same `major.minor`; protocol and capabilities only | same `major.minor` with a banner on patch skew; desktop bundles exact |
| D7 | Native WhatsApp Cloud, Linq, Nextcloud Talk and Gmail routes in v0.9.0 | in-process gateway only, `503` in `zeroclaw-gw`; move verify and parse into the core; wait for #8850 | in-process only for v0.9.0 |
| D8 | Core listeners under "no HTTP server in the core" | `[wss]` plane, `[enroll]` endpoint, channel listeners: exceptions or moves | `[wss]` and `[enroll]` as documented opt-in exceptions; channel listeners a dated exception |
| D9 | What v0.9.0 ships | contract, parity and in-process groundwork with the process split later; or the full split | contract, parity and groundwork; this record does not assume the full split fits the v0.9.0 window |
| D10 | Surfaces with no core path: ACP, nodes, WebAuthn, OIDC enrollment relays | keep them on the in-process gateway and answer `503` in `zeroclaw-gw`; or core methods (the contract specifies `oidc/*` for the relays) | in-process only for v0.9.0 |
| D11 | OS trust boundaries supported in v0.9.0 | same account only; same and separate accounts | same account only for v0.9.0; separate accounts once the layout of DEC-27 and D1-A ship |
| D12 | What a generic ingress idempotency key means after admission | admitted once (today); commit at completion and roll back on failure or cancellation; run admitted work to completion regardless of the caller | admitted once for v0.9.0 |
| D13 | Release-surface changes in `zeroclaw-gw` | `/admin/paircode*` leave it (the CLI uses the core socket); `OPTIONS /api/config/prop` requires a credential; with pairing off, a presented but invalid credential gets 401 | accept all three, with release notes |

### Acceptance gates

This record stays proposed until:

- the contract is merged with the operation catalog, route-to-method table, wire shapes, error lists and compatibility register, and the OpenRPC document types every master and gateway-used method and matches the contract's shapes and error lists, drift-checked in CI;
- every operation the supported dashboard and API need is implemented in the core with a conformance test, or recorded as an explicit release gap (#11001 AC1);
- Unix-socket and named-pipe conformance covers initialization, authenticated identity, denial, compatible and incompatible versions, endpoint discovery and endpoint verification, with the pipe leg in a required CI job (#11001 AC2), plus the two-account layout if D11 admits separate accounts;
- turn lifetime, targeted cancellation, viewer attach, reconnect and bounded buffering have tests, including killing the gateway mid-turn (#11001 AC3, #11002 AC4);
- `webhook/deliver`, the ingress methods, core-owned idempotency with its acceptance point, per-delivery cancellation, stream correlation and the generation fence pass the ingress conformance set across separate processes (#11003); and
- each open decision above is recorded as decided on #8691 or #11000.

### Review and acceptance status

A decision is **proposed** when written here, **reviewed** when both reviewers return CLEAR on the same revision, and **accepted** only when the maintainers accept it on this record's PR, #11000 or #8691. A CLEAR review does not accept anything: an implementation PR may rely only on accepted decisions (#11000 AC6). The contract's decision register tracks each decision's status.

The contract and this record were reviewed adversarially over four revisions by two independent reviewers, who worked separately and checked every claim against the pinned source. Both cleared revision 4.

| Revision | Outcome | What changed in response |
|---|---|---|
| 1 | both blocked | wire shapes for every untyped master payload and per-method error lists; the separate-account socket layout; `delivery_id` correlation and per-delivery cancellation for ingress; idempotency committed at admission instead of before dispatch; public routes through the service allowlist; the pairing posture projection; the security-correction rule for protocol 1 and the compatibility register; the complete pairing-disabled (D2-A) specification |
| 2 | both blocked | typed shapes and error lists for the 49 methods open PRs add; capability-advertised targeted cancellation with random turn ids and a daemon instance id; own-connection `subscription/cancel`; the chat reply's `model` field and `/sop/*`'s no-match 404 |
| 3 | one cleared, one blocked | typed overlays for the seven existing payloads the open PRs extend; the SOP dispatch-entry union reused for ingress results; distinct names for the pairing read and mint results |
| 4 | both cleared | none |

## Consequences

Positive:

- One authority path: the gateway can be replaced, restarted or left out without changing who decides access.
- Webhook deduplication and secret verification no longer depend on the lifetime of the internet-facing process, the decrypted webhook secret never enters it, and a refused webhook never consumes its own retry.
- Discovery, version checks and endpoint verification become testable properties shared by zerocode, the gateway and the desktop app.
- Implementers get complete request, response and error shapes up front, and security fixes have a defined way to ship inside protocol 1.
- The OpenAPI question is answered without an HTTP server in the core, and no config key moves.

Negative:

- Session-owned turns and turn identities change drain, ephemeral-mode accounting and the event stream, and need their own lifecycle tests.
- Ingress methods, the service identity (D1-A) and endpoint verification add a resolver branch, a key lifecycle and platform code that ADR-017 review must cover; the separate-account layout changes how the listener secures its socket.
- The proto crate must stop depending on `zeroclaw-config` before the gateway can pass its dependency ratchet.
- Every security correction inside protocol 1 can refuse a call an older client used to make; the register and release notes are the only warning.
- The Windows pipe-name change and the discovery fix are lockstep changes across three binaries.
- Until D7 and D10 are decided differently, the separate gateway cannot serve native vendor webhooks, ACP, nodes, WebAuthn or OIDC enrollment.
- In the default same-account deployment the gateway remains operator-equivalent by operating-system construction; only the separate-account layout makes its service profile a real boundary.

Follow-up work this record creates: the SCH fixes to #11165's OpenRPC document and the removal of its `zeroclaw-config` dependency; turn identities, the targeted-cancel methods and the daemon instance id; the own-connection classification of `subscription/cancel`; endpoint verification in `zeroclaw-rpc-client`'s dial path and an explicit pipe DACL; the discovery fix and SHA-256 pipe naming; the plugin webhook generation fence and the core-owned idempotency store with its acceptance point, both possible in process now; the ingress methods, `webhook/deliver` and the pairing posture for the process split; errata to `rpc-socket.md`; and the conformance harness over a real socket and a real named pipe (#11273 drafts the daemon side), with the pipe leg in a required CI job.

## References

- [ADR-007: Extract the gateway into a separate optional process](./ADR-007-gateway-extraction.md)
- [ADR-017: Inbound authentication and principals](./ADR-017-inbound-authentication-and-principals.md)
- [ADR-018: Runtime security and provenance](./ADR-018-runtime-security-and-provenance.md)
- [ADR-012: Generation-scoped live config apply](./ADR-012-generation-scoped-live-config-apply.md)
- [Core-to-gateway IPC contract](../core-gateway-ipc-contract.md)
- [RPC socket transport](../rpc-socket.md)
- [Gateway API](../../gateway/api.md)
- Issue [#11000](https://github.com/zeroclaw-labs/zeroclaw/issues/11000), tracker [#7432](https://github.com/zeroclaw-labs/zeroclaw/issues/7432), decision thread [#8691](https://github.com/zeroclaw-labs/zeroclaw/issues/8691)
- `crates/zeroclaw-api/src/jsonrpc.rs`
- `crates/zeroclaw-api/src/webhook.rs`
- `crates/zeroclaw-runtime/src/rpc/`
- `crates/zeroclaw-gateway/src/plugin_webhook.rs`
