# Design note: browser Authorization Code + PKCE enrollment (#8289 stage 5 remainder)

Status: DECIDED 2026-08-24 (decisions recorded below). Tracker: #8289. RFC: #7141.
Prior art: #10270 (device grant + client_credentials enrollment), #10255 (token
verification), #10248 (canonical principals), #10274 (gateway route-layer auth).

Enrollment stays client-side machinery: it obtains an access token to present as
`auth_token`; the daemon only verifies. Nothing here changes verification.

## The load-bearing fact about reachability

In the Authorization Code flow the IdP never contacts the redirect URI from its
servers. The user's browser follows a 302 to it. So the redirect target only
needs to be reachable from the browser doing the login, not from the internet.
A self-hosted gateway that is not publicly reachable is therefore NOT blocked;
what matters is which machine the browser runs on. That fact drives Decision 1.

## Decision 1: where the redirect lands

**Option A (recommended): CLI loopback listener, gateway untouched.**
`zeroclaw oidc login <alias> --browser` binds an ephemeral one-shot listener on
`http://127.0.0.1:<random port>/callback` (the RFC 8252 native-app pattern; the
IdP client registers the loopback redirect, any port), opens the system browser,
receives the code, exchanges it with the PKCE verifier, prints the token on
stdout exactly like #10270 (progress on stderr, token composable via
`ZEROCLAW_AUTH_TOKEN="$(...)"`).

- Works for every deployment where the person enrolling has a browser on the
  machine running the CLI, including fully private gateways. Remote/headless
  hosts are exactly the case the device grant in #10270 already covers, so the
  pair closes the matrix.
- No new unauthenticated gateway route, no `public_url` config, no token
  hand-off to a browser context. Smallest attack surface, ships as a pure
  extension of the existing `Enrollment` client.
- Tradeoff: does not give the web dashboard an in-browser sign-in. A dashboard
  user still pastes a token (or pairs) today.

**Option B: gateway-hosted flow (`GET /oidc/login/{alias}` starts,
`GET /oidc/callback` finishes).**
The gateway becomes the OAuth client: it stores state server-side, exchanges the
code, and must then deliver the resulting token to the browser session. Works
whenever the dashboard itself is reachable, so also fine for LAN-only gateways.

- Tradeoff: two new unauthenticated routes (the callback must be), a required
  `redirect base URL` config field per deployment, a pending-flow store, rate
  limiting on an unauthenticated starter, and a token-delivery design in the
  browser (one-time display page vs httpOnly cookie session) that is really the
  "dashboard sign-in" feature, not enrollment. Larger blast radius for the same
  token.

**Option C: both now.** Full coverage, roughly double the surface and review
load, and B's hardest part (browser token delivery) still needs its own design.

Recommendation: **A now; B later as its own "dashboard sign-in" design once the
dashboard consumes scoped principals for real work.** A is a strict subset of
the code B needs anyway (same PKCE core), so nothing is thrown away.

## Decision 2: CLI shape

**Option A (recommended): `--browser` flag on the existing `zeroclaw oidc login
<alias>`.** One verb for "sign in interactively", flag picks the mechanism;
device grant stays the default (works everywhere, including over SSH). No
silent auto-fallback in either direction: picking the mechanism stays explicit,
matching the provider-selection ethos, though each flow's failure message names
the other as the alternative.
- Tradeoff: flag semantics to document; `login` help text grows.

**Option B: a new `zeroclaw oidc browser-login <alias>` subcommand.** More
discoverable in `--help`, but two verbs for one action, and scripts must know
which one their IdP supports.

## PKCE method (confirm, not really a fork)

**S256 only.** `plain` exists for clients that cannot compute SHA-256; we are
not one. If the issuer's discovery document does not advertise S256 in
`code_challenge_methods_supported`, enrollment fails with a clear error. No
downgrade path, no `plain` fallback, `response_type=code` only (no implicit).

## State, nonce, verifier: storage and checks

Option A keeps the whole flow in the memory of one short-lived CLI process:

- `state`: 128-bit random, echoed by the IdP, compared exactly once. The
  listener answers only the request whose `state` matches; anything else gets a
  fixed static failure page (never echo attacker-supplied query content).
- PKCE `code_verifier`: generated per attempt, held in process memory, sent only
  in the code exchange, never logged or persisted.
- `nonce`: sent on the authorize request; if the token response carries an
  `id_token`, its nonce is checked client-side and the id_token is then
  discarded. Only the ACCESS token is ever presented to the daemon, preserving
  the token-purpose separation #10255 enforces (the daemon still rejects
  nonce-marked ID tokens as credentials).

Nothing is written to disk. If Option B is ever built, its server-side
pending-flow store follows the `api_pairing::PairingStore` precedent: in-memory,
keyed by `state`, single-use consume-on-arrival, short TTL, capped size, rate
limited via the existing gateway auth limiter.

## Joining the canonical principal model

No new join point. Enrollment output is an opaque access token; identity enters
through the one existing door: the client presents it as `auth_token` with
`auth_provider = "oidc.<alias>"` in the RPC handshake (or the #10274 gateway
header), the selected `OidcAuthProvider` verifies it, and the resolver keys the
principal by issuer+subject exactly as #10248 defined. PKCE-enrolled and
device-grant-enrolled tokens for the same person at the same IdP resolve to the
SAME principal, because the principal is derived from verified claims, never
from how the token was obtained. Enrollment mints no principals and grants
nothing.

## Coexistence with #10270

One `Enrollment` client, three flows (device grant, client_credentials,
authorization code + PKCE), one output contract (stderr progress, stdout token,
store nothing). Refresh tokens remain out of scope for all three until the
keychain design lands; re-enroll on expiry.

## Failure modes that must fail closed

| Condition | Behavior |
|---|---|
| `state` missing or mismatched on the callback | Abort; discard the code; static failure page; nonzero exit |
| `id_token` present with wrong/missing `nonce` | Abort (token substitution); never present the access token |
| `iss` callback parameter (RFC 9207) present and wrong | Abort (mix-up attack) |
| IdP redirects with `error=` | Surface the IdP's error verbatim; no automatic retry |
| Discovery lacks S256 support | Hard error naming the requirement; no `plain` downgrade |
| Code exchange non-2xx / unparseable | Abort with the OAuth error body; verifier is single-use, never resent |
| Listener receives a second request | Ignored; the listener answers one matching request then shuts down |
| Browser cannot be opened | Print the authorize URL for manual opening; the loopback wait continues (bounded by the flow timeout) |
| Flow timeout (default: authorize request `expires_in`-equivalent, minutes not hours) | Listener shuts down; nonzero exit |

The listener binds `127.0.0.1` only, one random port, single request lifetime.

## Decisions (ratified by Jordan, 2026-08-24)

1. **Redirect surface: Option C, both.** The CLI loopback listener AND the
   gateway-hosted flow ship.
2. **CLI shape: the `--browser` flag**, with a requirement that reshapes the
   architecture: the enrollment APIs must work across CLI, webGUI, and
   TUI/zerocode, not just the CLI. See "Cross-surface enrollment API" below.
3. **S256 only, confirmed** (it is the RFC 7636 standard method; `plain` is the
   legacy fallback we refuse).
4. **Browser token delivery: delegated.** Resolution chosen here: the gateway
   callback renders a one-time, self-contained page that hands the token to
   `window.opener` via `postMessage` with the gateway's own origin as the
   target, plus a manual copy fallback on the same page. No cookies and no new
   session model: the dashboard keeps authenticating per request with the
   `Authorization` header and `X-ZeroClaw-Auth-Provider`, exactly the
   route-layer contract already reviewed. This is the most reversible choice
   (a later cookie-session design deprecates one page, not an auth model).

## Cross-surface enrollment API (added for decision 2's requirement)

zerocode deliberately cannot depend on the zeroclaw crates (the
`zerocode_no_zeroclaw_dep_gate` enforces it), and neither zerocode nor the
webGUI holds IdP client credentials. So the shared surface is the gateway: a
small, unauthenticated-by-necessity, rate-limited enrollment API that proxies
the IdP flows, with the gateway holding the `[oidc.<alias>]` client
credentials. It performs no authentication itself and grants nothing: it only
relays what the IdP grants after the user approves.

| Route | Flow | Consumer |
|---|---|---|
| `GET /api/oidc/providers` | lists configured aliases (already advertised by the RPC handshake) | all GUIs |
| `POST /api/oidc/{alias}/device/start` | proxies the RFC 8628 device-authorization request | TUI/zerocode, webGUI fallback |
| `POST /api/oidc/{alias}/device/poll` | proxies one token poll (pending / slow_down / token) | TUI/zerocode, webGUI fallback |
| `GET /oidc/login/{alias}` | starts Authorization Code + PKCE, 302 to the IdP | webGUI |
| `GET /oidc/callback` | validates state, exchanges the code, delivers per decision 4 | webGUI |

Client-to-surface mapping:

- **CLI** keeps talking to the IdP directly (it has the config): device grant
  and `client_credentials` shipped already; `--browser` adds the loopback PKCE
  flow. The gateway API also works for it, but is not its default.
- **webGUI** uses `GET /oidc/login/{alias}` (full-page or popup) and receives
  the token per decision 4, then sends it on every API call with the
  route-layer headers.
- **TUI/zerocode** calls the device endpoints over plain HTTPS before its RPC
  handshake: start, render the user code, poll, then reconnect presenting the
  token as `auth_token`. Device grant is the right UX for a terminal; no
  loopback listener or browser is assumed on the zerocode host.

Security posture of the API: every route is rate limited through the existing
gateway auth limiter; the PKCE flow store follows the `PairingStore` precedent
(in-memory, keyed by `state`, single-use consume-on-arrival, short TTL, capped
size); the device proxy is stateless (the IdP's `device_code` is the flow
handle); the `redirect_uri` used at exchange is the one stored at flow start,
derived from the configured redirect base when set and otherwise from the
request origin, and a wrong one simply fails at the IdP, which only accepts
registered redirect URIs.
