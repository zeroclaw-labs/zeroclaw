# Fork Notes

This page keeps the fork-specific facts that used to live in `AGENTS.md`. The short rules are in [`AGENTS.md`](https://github.com/kckylechen1/zeroclaw/blob/master/AGENTS.md); product direction is in [ADR-013](../architecture/decisions/ADR-013-channels-as-gateway-clients.md) and [ADR-017](../architecture/decisions/ADR-017-personal-agent-body-edges-and-delegation.md).

## Where new types go

`crates/zeroclaw-config/src/schema.rs` is about 22.5k production lines of config structs on `agent-runtime`'s mandatory path, so every consumer pays for every subsystem's config at build time. The release binary is unaffected because LTO discards what is never instantiated; the cost is build time and disk.

1. **Shared wire and domain types go in `zeroclaw-api`.** A type two crates both name belongs there.
2. **Logic stays where it runs.** `zeroclaw-api` holds the shape; the owning crate holds the behavior. A consumer that only reads a result must not link the runtime that produces it.
3. **New config sections get their own module** under `zeroclaw-config/src/` (see `persona.rs`), not another block in `schema.rs`.
4. **Do not restructure `schema.rs` to fix this.** Shrink it by deleting sections whose readers are gone (#382 S7). Schema unit tests live in `zeroclaw-config/src/schema/tests.rs`.

## Upstream relationship

### Desktop backend continuity

Compared with upstream `zeroclaw-labs/zeroclaw#11523`, merged as
`ee04d0d44142674886c880e9f809838b49804aca`, this fork adapts abandoned-turn
settlement at the gateway owner. It does not import upstream's broader live
config authority. The existing conversation hub owns detached turns, while
`SessionBackend` owns history and request receipts. Only gateway-prefixed
running rows are settled, after exclusive data-directory ownership; failure
refuses startup. The empty lock file contains no second task ledger.

Upstream comparison head `87461d1ac5c9005b82216a223010471e63ab6718` has no
selected equivalent for this fork's shared-conversation approval recovery.
The owner approved retaining pending tool prompts through client detach within
the original timeout. This replaces the prior immediate offline revocation;
expiry, cancellation, deletion, restart and existing response authorization
remain fail-closed. See [runtime ownership and persistence](../architecture/runtime-state-and-persistence.md#client-detach-and-approvals).

This repository is an independent project (decision 2026-08-16). Upstream `zeroclaw-labs/zeroclaw` is a reference to port from deliberately, not a stream to track: no scheduled rebases and no wholesale merges.

- **Baseline:** history contains upstream work through upstream PR #9356; `master` `40cc158e8` (2026-08-16) is the last wholesale-aligned point.
- **Porting:** read the upstream diff, adapt it, validate it at the touched surface's risk level, and record provenance in the commit message ("ported from upstream #NNNN").
- **Dependency CVEs** stay covered by `cargo audit` and `cargo deny`.

### Completed tool failure recovery (#382)

Boundedly adapted from upstream [#11507](https://github.com/zeroclaw-labs/zeroclaw/pull/11507),
merged as `fcc23a22f6eaf199408fbb58377abe551c7d3e51`. Configured Agent turns use
completed canonical dispatch evidence: repeated identical failures prompt a
correction and allow one recovery retry before a visible, recoverable stop.
Completed results and receipts remain in the existing turn history. Successful
polling is advisory only; approvals, hooks and cancellation keep their existing
authority. The fork retains its extracted turn modules, raw outcome data path,
legacy channel detector and outer steering behavior. Only the existing live
daemon config handle is retained for pacing refresh; no upstream lifecycle,
additional configuration key, durable ledger or orchestration layer is imported.
See [runtime internals](../agents/internals.md#repeated-tool-failure-recovery).

### Deliberate behavioral divergences

| Divergence | Upstream | Status |
|---|---|---|
| `always_ask` outranks Full autonomy | returns `Approved` for Full before consulting `always_ask` (fail-open) | deliberate |
| Risk-profile `allowed_tools`: absent is not empty | maps `[]` to `None`, which is unrestricted | deliberate |
| Cron `allowed_tools = []` means deny-all | asserts the opposite in a test | deliberate |
| `ModelProvider::set_credential` with real 429 rotation | logs "cannot apply … Retrying with original key" | deliberate |
| WeChat state persistence is atomic and non-blocking | blocking, non-atomic write | deliberate |
| Portable memory kernel behind `memory-tachi` (`zeroclaw-memory/tachi`) | absent | deliberate; distinct ZeroClaw memory and companion roles |
| `vault:<NAME>` secret references resolved from Tachi Vault and preserved on disk | `op://` only at `87461d1ac5` | deliberate; Tachi Vault owns credentials |
| HyperMemory custom CRUD backend | absent | retired; never re-add |
| Builtin `calculator` / `weather` tools (`crates/zeroclaw-tools/src/{calculator,weather_tool}.rs`) | present | retired; implementations deleted, names reserved in `RETIRED_OPERATOR_TOOL_NAMES`. Weather, if ever needed again, is an HTTP skill / MCP wrapper, not a builtin |
| `knowledge` graph tool + `knowledge.db` + `[knowledge]` / `[knowledge_bundles]` config | present | retired; relationship memory belongs to the Tachi memory kernel (`memory-tachi`), not a second local graph. Leftover sections warn via `RETIRED_CONFIG_SURFACES` tombstones |

### Known gaps

- **MCP `__` auto-admit is opt-in.** The default `mcp_discovered_tool_policy` is `explicit_only`. Setting `auto_admit` on a risk profile admits any unlisted `<server>__<tool>`, so keep write tools off the MCP servers such a profile connects to.
- **Approvals.** Local grants persist in `data_dir/approvals.db` with an `approval_audit` trail. Node grants and receipts are not built yet (#62).

## Phone candidate-history pages

The #379 history leaf extends the existing operator-only
`GET /api/user-model/candidates` route. Its sqlite User Model store remains the
single history owner. Legacy requests, including `pending=true`, keep the
existing `{ "candidates": [...] }` response with all matching history.

Opt in with `?limit=50` (accepted range: 1 through 200). Paged responses add
`next_cursor`, either `null` at the end or `{ "created_at_unix": 123, "id": "..." }`.
For the next page, pass both returned values as `before_created_at` and
URL-encoded `before_id`, retaining the same `pending` filter and `limit`.
Cursors without a limit, partial cursors, invalid limits and out-of-range
sqlite timestamps are rejected after operator authentication and before the
store opens. Bridge and Node tokens do not gain owner access.

Ordering is newest first by `(created_at_unix, id)`, including ties. Each
request selects at most `limit + 1` IDs before fetching their evidence; the extra
row determines whether another page exists. This bounds evidence materialization
without an index or schema migration. Metadata selection still scans and sorts
matching history; a page is not a database snapshot across requests. Candidates
inserted newer than the cursor appear on refresh, and pending rows reviewed
between requests disappear from subsequent pending pages.

The phone view requests 50 candidates at a time, appends unique IDs after an
explicit load, and keeps its existing rows and cursor when a page fails. Refresh,
Agent/tab changes, owner authorization failure and successful review mutations
reset the loaded view. Old in-flight reads cannot repopulate the reset view.
Gateways that ignore the new query and omit `next_cursor` produce an upgrade
message. Upgrade the gateway before serving the new web bundle; a rollback to
the older bundle keeps its legacy route contract. No data migration is needed.

### Upstream comparison for this leaf

Source inventory: upstream `zeroclaw-labs/zeroclaw` at
`329e66f76394b00af9d00b767e00ad298133961e`; fork baseline
`36b362f509a4dde72e686b4aca6b640b5537b495`.

| Source | Existing local equivalent | Decision and remaining gap |
|---|---|---|
| #9353, merged `1c7ef505b29ca28d7bcec3594c8c5c69c063c159` | Alias-scoped hydration and bounded browser transcript cache | Defer port: independent conversations, picker and per-conversation draft lifecycle remain a separate leaf. |
| #9355, merged `e7548768e0d21411008f130f324cfa06b10be4a1` | Alias-keyed `openChats` workspace | Defer port: upstream per-pane keys, session selection and sibling collision guards are not equivalent to this workspace. |
| #10544, merged `e5d556a17d4633218aa70f82ed8aab58e821a37d` | Existing authenticated HTTP attachment handles for shared intake | Defer port: the web composer has no image attachment control; preserve the fork's handle contract when adapting it. |

The pinned upstream tree has no `api_user_model.rs`, `Review.tsx` or `review.ts`
equivalent. This pagination leaf extends the fork's existing owner-review
surface and does not port the unrelated upstream lifecycle or upload changes.
It adds no endpoint, configuration, store or dependency, and retires no route.
Phone pairing/chat/attachment and PWA installability acceptance remain open.

## Hyperion (dormant)

Hyperion, the owner's quantitative-trading system, is not an active consumer of this fork (#267 to #269 are closed). If that work resumes, these rules still apply:

1. Memory goes through typed hapi-edge facade actions only. Never connect to a memory backend directly, never touch `data/hapi.db`, and never re-add HyperMemory.
2. Trading tools go through the hapi-edge MCP server only, never direct broker or data-vendor calls.
3. Real position writes require human OTP confirmation.
4. Timezone `Asia/Shanghai`; A-share lot 100 shares (STAR: minimum 200, then 1-share increments).

The companion stores (#49) are a separate, feature-gated, in-process exception.
The optional shared Portable Kernel (#560) supplies persistence for the Tachi
memory backend and companion store, with distinct exact roles and separate
database files. They never share a live database with, or connect to, a network
memory service. Hypermem remains Hyperion's adapter; the shared kernel does not
own ZeroClaw's Soul, User Model, personality policy or Tachi delegation bridge.

### Portable kernel adoption (#560)

`memory-tachi` pins `portable-kernel` 1.9.0 at Tachi revision
`b8b10642372ac6910afb2e2bab5265c1d2041a9e`, without default/admin features.
This retains the SQLite 3.54.1 security backport at
`c35ace357375f0e3a9db32d7a3bb529507a72291` and the vault-kit system-entropy
backport at `a4147e40e937f2863cd0e124d64061defe0ed433` from
[Tachi #2319](https://github.com/kckylechen1/Tachi/pull/2319).
The vault nonce and salt sizes, encrypted formats and fail-closed entropy
behavior remain unchanged. It does not adopt unrelated Tachi main changes.
The support workspace also retains the dependency security fixes from
[Tachi #2323](https://github.com/kckylechen1/Tachi/pull/2323), the exact
metadata key candidate streaming backport from
[Tachi #2320](https://github.com/kckylechen1/Tachi/pull/2320)
(`find_metadata_key_candidates`), and the scoped recent-read backport in
[Tachi #2322](https://github.com/kckylechen1/Tachi/pull/2322).
The scoped recent-read contract applies host agent, session, namespace and
lexical time predicates before the SQL limit, while the adapter retains its
final host admission check. Large agent allowlists use bounded queries and merge
their results before applying the global limit; each query hydrates at most that
limit of full rows.
The SQLite backport uses the previous
`b0c754a0cd3e4e7ffd0199691e08954942b492bd` baseline, adapting the native dependency
and connection APIs from [Tachi #2284](https://github.com/kckylechen1/Tachi/pull/2284).
It retains the snapshot-reopen fix in
[Tachi #2205](https://github.com/kckylechen1/Tachi/pull/2205), on top of the shared
kernel contracts in [Tachi #2198](https://github.com/kckylechen1/Tachi/pull/2198).
Generic memory requires `PortableKernel` / `zeroclaw-memory`; companion requires
`PortableKernel` / `zeroclaw-companion`. Existing runtime and status-probe paths
never upgrade a schema or adopt an unowned store. Backend/config names and
database locations remain unchanged.

Upstream comparison:
`zeroclaw-labs/zeroclaw@7f39e313e7fe554da1c85a83a4b5f00f6ef537e4` has no
Tachi/memcore/Portable Kernel adapter in its tree or memory dependency manifest.
This implements the fork's existing owner boundary, with no upstream code port.
The standalone `memcore` runtime dependency is replaced only after preserving
the existing memory, enrichment, governance and companion consumers. No store or
configuration key is retired. See the
[operator migration procedure](../setup/portable-memory.md).

## Yonbo device host ownership

The legacy N3B1 Yonbo device crate is retired from the ZeroClaw workspace. Its
complete 11-file source snapshot is preserved in [Zeroclaw-Robotic](https://github.com/kckylechen1/Zeroclaw-Robotic/tree/main/integration/zeroclaw/source/crates/zeroclaw-yonbo-node),
which owns future device engineering. N3B1 is distinct from Gateway Node v2.
This source retirement changes no running device, credential, or Gateway Node
contract and does not establish device replacement or physical acceptance.

## Restricted device chat admission

The #495 leaf under #377 adds an optional `gateway.bridges.<name>.device`
policy to the existing shared chat route. Its configuration is the authority
owner; the existing session backend and Conversation remain the history and
turn owners. See [restricted device bridges](../gateway/api.md#restricted-device-bridges)
for bindings, per-operation grants, revocation and downgrade discipline.
Chat authority never grants owner approval or Operator learning provenance.

Upstream `zeroclaw-labs/zeroclaw` was inspected at
`5d4b18d335780af8bf15979b2f45e541f91c5eb9`, against fork baseline
`79a8320132f5866b3846ca561eaaf054241d37b5`. Upstream `src/gateway/ws.rs` and
`src/gateway/ws_approval.rs` contain chat/approval transport, but no equivalent
restricted bridge policy. This fork already owns shared Conversations, bridge
credentials and session scopes, so this leaf extends those owners rather than
porting the upstream session implementation. No existing route or consumer is
retired. Turn streaming/recovery, adapter integration and Node invocation remain
separate slices; #377 and #380 remain open.

## Non-Telegram turn control

The #497 leaf adds opt-in request/turn correlation to the existing shared
Conversation, following restricted bridge authority in #495. It does not borrow
Telegram source identifiers, replace the session ledger, or add another Agent.

Upstream comparison: `zeroclaw-labs/zeroclaw` at
`5d4b18d335780af8bf15979b2f45e541f91c5eb9`,
`crates/zeroclaw-gateway/src/ws.rs`; fork baseline `eaf007c95`.
The upstream socket lifecycle and cancel-token helpers are not equivalent to
this fork's shared Conversation and durable request receipts. Retain the local
owners and extend their transport boundary; do not port a second runtime.
The extension's bounded in-process replay cache is owned by ConversationHub.
It is not a durable input queue or an execution-retry promise. Missing recovery
state is unknown, never permission to repeat effects.

## Shared session deletion

Deleting a persisted gateway session also clears the live Agent history shared
by every socket on that session. ConversationHub owns a short-lived per-session
lifecycle mutex; the existing session execution queue still serializes turns
and their persistence. Deletion cancels active turns, waits for their writes,
then clears the backend and each live Agent under those guards. Local reset
revisions reject pre-deletion claims, continuations and unread conversation
frames for both legacy and negotiated sockets. Bytes already written to a
client cannot be retracted. Request tombstones and
the process-wide turn-control epoch are preserved; other sessions are unaffected.
With persistence disabled, DELETE retains its existing 404 response and does not
implicitly reset an in-memory conversation. This does not delete Soul, User Model
or the agent's long-term memory.

Upstream `zeroclaw-labs/zeroclaw` at
`641b766030805edfa999bb5aa6ff19a9ffd4339b`,
`crates/zeroclaw-gateway/src/ws.rs`, constructs an Agent per socket and has no
ConversationHub. The fork already owns shared conversations and replay receipts;
the gap is deletion of their retained live history. This is a bounded local fix,
with no upstream code port, new durable store, protocol or configuration key.

## Deferred MCP exact-name recovery

The #382/#376 leaf compares upstream `zeroclaw-labs/zeroclaw` at
`641b766030805edfa999bb5aa6ff19a9ffd4339b` against fork baseline
`fc9c8698b8f2558ac0d4e40320c756be27614b42`. Upstream's
[tool executor](https://github.com/zeroclaw-labs/zeroclaw/blob/641b766030805edfa999bb5aa6ff19a9ffd4339b/crates/zeroclaw-runtime/src/agent/tool_execution.rs)
resolves static and already-activated tools; its
[MCP wrapper](https://github.com/zeroclaw-labs/zeroclaw/blob/641b766030805edfa999bb5aa6ff19a9ffd4339b/crates/zeroclaw-tools/src/mcp_tool.rs)
forwards arguments to the server. This fork already owns deferred MCP stubs,
activation, tool search and access policy. The bounded gap is recovery when a
model calls a known, authorized deferred name before searching, plus compact
argument feedback. No upstream catalog or runtime is wholesale-ported.

The existing conversation `ActivatedToolSet` now owns the deferred set, policy
and activation notification shared by `tool_search` and exact-name recovery.
The MCP registry remains the connection/dispatch owner; the runtime retains
approval, hooks, current-turn exclusion and execution ownership. No new store,
configuration or default tool is added. Node/native catalogs are outside this
leaf. No consumer or dependency is retired.

OpenClaw's
[tool-search contract](https://github.com/openclaw/openclaw/blob/fe2b1a51b41b80941fe93cfb0dfbdc0def2401d1/docs/tools/tool-search.md)
is a mechanism reference for bounded discovery and recovery, not a code port.
OpenClaw's dispatcher contract differs; ZeroClaw keeps its existing tool loop and
only activates exact MCP names already admitted by its own policy. See
[MCP loading and argument recovery](../tools/mcp.md#deferred-loading) for behavior
and validation limits. Reverting this leaf restores search-first loading; no
persistent data migration is required.
