# Native pairing-token consumer audit (task 1.3)

Input for design §4's migration of the pairing authority to token records with
mint/expiry metadata. Every path below accepts a native pairing bearer today;
each row names where authoritative expiry must be enforced when personal mode
is enabled. Read-only audit of `master` at 8f03c962dc; line numbers drift.

## Canonical authority

| Fact | Owner | Evidence |
|---|---|---|
| Paired-token set | `PairingGuard` (`zeroclaw-config/src/pairing.rs:357`) | Hash-only `HashSet<String>` (SHA-256). No mint time, no expiry, no per-token identity. |
| Persistence | `Config.gateway.paired_tokens` (`zeroclaw-config/src/schema.rs:7883`) | Hashes serialized inside the config file; loaded at startup. The record migration must stay compatibly serializable. |
| One-time pairing code | `PairingGuard::pairing_code`, `PAIRING_CODE_TTL = 10 min` (`pairing.rs:305`) | Codes already expire; tokens never do. Reservation/commit keeps the code restorable on failed issuance. |
| Process admin token | `PairingGuard::admin_token`, `rotate_admin_token` (`pairing.rs:461`) | In-memory authority, owner-only file for local clients, constant-time compare, rotation fails closed. Distinct from paired bearers; used only for pairing-code admin routes. |
| Strict vs fail-open | `token_is_paired` vs `is_authenticated` (`pairing.rs:660,675`) | `is_authenticated` fails OPEN when `require_pairing=false`; auth providers must use the strict check. The record-based check must preserve this split. |
| Revocation | unpair flows through config writes | Revocation is a config-write path, published through the same persist boundary as policy. |

## Accepting transports (where expiry must bind)

| # | Surface | Check today | Expiry enforcement point for the migration |
|---|---|---|---|
| 1 | Dashboard HTTP routes (`zeroclaw-gateway/src/api.rs:43` `require_auth`, ~31 call sites) | `state.pairing.is_authenticated` per request, fail-open when pairing disabled | Replace/extend with the record-based liveness check; fail-open posture must not silently extend to personal mode (personal activation requires pairing). |
| 2 | Config/onboarding route layer (`zeroclaw-gateway/src/principal_gate.rs`, route_layer) | `GatewayInboundAuth` → `RpcInboundAuth::authenticate` → native provider; write-set re-check under the config write lock | Already flows through the principal model; record expiry reaches it via the resolved principal (see below). |
| 3 | Webhook ingress (`zeroclaw-gateway/src/lib.rs:3396`) | `is_authenticated` + auth rate limit, per request | Same as #1. |
| 4 | SSE `/api/events` (`zeroclaw-gateway/src/sse.rs:48`) | Connect-time `is_authenticated` ONLY. **No live recheck on the established stream.** | Existing revocation-latency boundary: a revoked token keeps streaming until reconnect. Personal-mode business surfaces must not inherit this; for the owner control plane, document or fix per design §4 ("WebSocket/RPC rechecks"). |
| 5 | WS sop runs (`zeroclaw-gateway/src/ws_sop_runs.rs:49`) | Connect-time only | Same gap as #4. |
| 6 | WS canvas (`zeroclaw-gateway/src/canvas.rs:184`) | Connect-time only | Same gap as #4. |
| 7 | WS plugins (`zeroclaw-gateway/src/api_plugins.rs:105`) | Connect-time only | Same gap as #4. |
| 8 | RPC admission (`zeroclaw-runtime/src/rpc/auth.rs:668` `authenticate`) | Native provider → `token_is_paired`; stamps `ConnectionAuth.native_token_hash` + `LocalCredentialEvidence::NativeTokenHash` | Admission must also stamp per-token expiry onto the identity/principal. |
| 9 | RPC established-connection recheck (`zeroclaw-runtime/src/rpc/dispatch.rs:1443`) | Per operation: `credential_unexpired` (principal `expires_at`/`revalidate_by`) + `token_hash_is_paired` liveness + generation-move revalidation | Expiry already enforced here once the principal carries it; nothing new needed beyond admission stamping. |
| 10 | Guarded effects (`RpcInboundAuth::hold_authority` → `AuthorityLease::current_grants`, `auth.rs:573-617`; `dispatch.rs:1625,11789`) | Holds paired-token set + accepted state still across the effect; checks `credential_unexpired` + liveness | Same: picks up principal expiry automatically. Lease duration must stay shorter than any meaningful expiry granularity. |

## Expiry infrastructure that already exists

`Principal.expires_at` / `revalidate_by` (`zeroclaw-api/src/principal.rs:261-265`) are
checked by `credential_unexpired` (`rpc/auth.rs:622`) on every live-grant path
(#9, #10). OIDC providers stamp them; the native shared-operator identity leaves
them `None` (`principal.rs:279`), so native tokens never expire today. The
migration can therefore flow expiry through existing enforcement by stamping the
identity at admission from per-token records — no new check sites on RPC paths.

## Direct hash-set consumers that bypass principals

Rows #1, #3-#7 consult `PairingGuard` directly (no principal, no `ConnectionAuth`).
For these, the record-based check must be exposed on the guard itself (e.g.
token-hash → record lookup), or the surfaces must migrate to the gate. Task 3.2
must test each of these transports with an expired record.

## Web credential storage (`web/src/lib/auth.ts`)

- Bearer stored in `localStorage` under `zeroclaw_token`, plaintext, readable by
  any page script; sent as `Authorization` header.
- No cookies ⇒ no CSRF surface on the current dashboard; the flip side is XSS =
  token theft. PRD §8.4's "not readable by normal page scripts" therefore
  requires a NEW owner-session mechanism for personal mode (server-side hashed
  opaque session + HttpOnly cookie), NOT reuse of `lib/auth.ts`.
- `useAuth.ts` pairs via `POST /pair` and keeps the token for the tab's
  lifetime; there is no server-side session concept to extend.

## Pairing-code admin routes

`require_gateway_admin_token` (`zeroclaw-gateway/src/lib.rs:4887`) gates pairing-code
issuance on the run-scoped process admin token (presented via
`GATEWAY_ADMIN_TOKEN_HEADER`); rotation happens at gateway startup
(`lib.rs:1940`). Local clients learn the token from the owner-only file written
by `write_gateway_admin_token`. This matches design §4: the admin token stays
local-only and is never a business credential.

## Migration obligations distilled

1. Extend `PairingGuard` records with mint/expiry metadata; keep config
   serialization compatible for disabled deployments (task 3.1).
2. Stamp expiry at RPC admission (#8) so #9/#10 enforce it unchanged.
3. Migrate or wrap direct consumers (#1, #3-#7) onto the record-based check;
   decide the SSE/WS live-recheck posture explicitly instead of inheriting the
   connect-time-only gap (task 3.2 tests every transport).
4. Personal activation invalidates records lacking trustworthy metadata and
   requires local re-pairing; disabled deployments keep expiry-free behavior.
5. Preserve the strict/fail-open split; personal mode requires pairing, so the
   fail-open path must be unreachable there.
