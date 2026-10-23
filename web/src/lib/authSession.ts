// Dashboard sign-in: the gateway's `/api/auth/*` and `/auth/*` contract, the
// startup decision between pairing, open and OIDC, and the session refresh used
// by re-authentication and CSRF recovery.
//
// Sign-in is opt-in. When the gateway has no `[gateway.dashboard_sign_in]`
// providers, `/api/auth/methods` answers `oidc: []` and everything here falls
// through to today's pairing flow.

import {
  AUTH_EVENTS,
  CSRF_HEADER,
  getOidcSession,
  setOidcSession,
  type OidcSession,
} from './auth';
import { apiOrigin, basePath } from './basePath';

// ---------------------------------------------------------------------------
// Contract types
// ---------------------------------------------------------------------------

/** One provider offered for dashboard sign-in. The alias is its label. */
export interface OidcMethod {
  alias: string;
  /** `oidc.<alias>` */
  provider: string;
}

/** `GET /api/auth/methods` */
export interface AuthMethods {
  pairing_required: boolean;
  oidc: OidcMethod[];
}

/** The `reason` of a 401/403 body from a cookie-authenticated request. */
export type AuthFailureReason =
  | 'session_expired'
  | 'session_invalid'
  | 'operator_required'
  | 'not_permitted'
  | 'agent_not_allowed'
  | 'csrf'
  | 'origin';

const FAILURE_REASONS: readonly AuthFailureReason[] = [
  'session_expired',
  'session_invalid',
  'operator_required',
  'not_permitted',
  'agent_not_allowed',
  'csrf',
  'origin',
];

/** 403 reasons that mean "this account may not do that", shown as not available. */
export const NOT_AVAILABLE_REASONS: ReadonlySet<AuthFailureReason> = new Set([
  'operator_required',
  'not_permitted',
  'agent_not_allowed',
]);

/** Name of the BroadcastChannel sign-in, re-auth and sign-out talk on. */
export const AUTH_CHANNEL = 'zeroclaw-auth';

export type AuthChannelMessage = { type: 'signed-in' } | { type: 'signed-out' };

/** Show the re-auth banner this long before the session expires. */
export const REAUTH_WARNING_MS = 120_000;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/** The structured `reason` of an error body, or null. */
export function failureReason(body: string): AuthFailureReason | null {
  if (!body) return null;
  try {
    const parsed: unknown = JSON.parse(body);
    if (parsed && typeof parsed === 'object' && 'reason' in parsed) {
      const reason = (parsed as { reason: unknown }).reason;
      return FAILURE_REASONS.find((r) => r === reason) ?? null;
    }
  } catch {
    // Not JSON (e.g. the pairing 401 text); no reason.
  }
  return null;
}

export function parseAuthMethods(value: unknown): AuthMethods | null {
  if (!value || typeof value !== 'object') return null;
  const raw = value as { pairing_required?: unknown; oidc?: unknown };
  const oidc = Array.isArray(raw.oidc)
    ? raw.oidc.flatMap((entry: unknown): OidcMethod[] => {
        if (!entry || typeof entry !== 'object') return [];
        const { alias, provider } = entry as { alias?: unknown; provider?: unknown };
        return typeof alias === 'string' && alias && typeof provider === 'string'
          ? [{ alias, provider }]
          : [];
      })
    : [];
  return {
    pairing_required: raw.pairing_required !== false,
    oidc,
  };
}

export function parseOidcSession(value: unknown): OidcSession | null {
  if (!value || typeof value !== 'object') return null;
  const {
    kind,
    provider,
    principal_id: principalId,
    display_name: displayName,
    admin,
    expires_at: expiresAt,
    csrf_token: csrfToken,
    features,
  } = value as Record<string, unknown>;
  if (
    kind !== 'oidc' ||
    typeof provider !== 'string' ||
    typeof principalId !== 'string' ||
    !principalId ||
    typeof expiresAt !== 'number' ||
    !Number.isFinite(expiresAt) ||
    typeof csrfToken !== 'string' ||
    !csrfToken
  ) {
    return null;
  }
  const flags = (features && typeof features === 'object' ? features : {}) as Record<
    string,
    unknown
  >;
  return {
    kind: 'oidc',
    provider,
    principal_id: principalId,
    display_name: typeof displayName === 'string' ? displayName : '',
    admin: admin === true,
    expires_at: expiresAt,
    csrf_token: csrfToken,
    // A missing flag is a denial: the page then shows less, never more.
    features: {
      chat: flags.chat === true,
      operator_pages: flags.operator_pages === true,
      core_sessions: flags.core_sessions === true,
    },
  };
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

const gatewayUrl = (path: string) => `${apiOrigin}${basePath}${path}`;

/** Tries per request while a gateway reload finishes. */
export const RESTART_ATTEMPTS = 4;
const MAX_RESTART_WAIT_MS = 5_000;

/** The wait a `gateway_restarting` answer asks for: its `Retry-After`, bounded. */
export function restartRetryDelayMs(response: Pick<Response, 'headers'>): number {
  const seconds = Number(response.headers.get('Retry-After') ?? Number.NaN);
  if (!Number.isFinite(seconds) || seconds < 0) return 1_000;
  return Math.min(seconds * 1_000, MAX_RESTART_WAIT_MS);
}

/**
 * `fetch` that waits out a gateway reload. A request that reaches a gateway
 * generation already told to stop is refused before it is routed (503
 * `gateway_restarting`), so sending it again, to the next generation, is safe
 * for every method. Every other answer is returned as it came.
 */
export async function fetchThroughRestart(
  input: string,
  init?: RequestInit,
  fetchImpl: FetchLike = fetch,
): Promise<Response> {
  for (let attempt = 1; ; attempt += 1) {
    const response = await fetchImpl(input, init);
    if (response.status !== 503 || attempt >= RESTART_ATTEMPTS) return response;
    let reason: unknown = null;
    try {
      reason = (JSON.parse(await response.clone().text()) as { reason?: unknown } | null)?.reason;
    } catch {
      // Not the gateway's JSON body: not a reload.
    }
    if (reason !== 'gateway_restarting') return response;
    await new Promise((resolve) => setTimeout(resolve, restartRetryDelayMs(response)));
  }
}

/**
 * `GET /api/auth/methods`. Null when the gateway doesn't answer it, which the
 * caller treats as "no sign-in providers".
 */
export async function fetchAuthMethods(
  fetchImpl: FetchLike = fetch,
): Promise<AuthMethods | null> {
  const response = await fetchThroughRestart(
    gatewayUrl('/api/auth/methods'),
    { cache: 'no-store' },
    fetchImpl,
  );
  if (!response.ok) return null;
  return parseAuthMethods(await response.json().catch(() => null));
}

export type SessionProbe =
  | { kind: 'oidc'; session: OidcSession; clockOffsetMs: number }
  | { kind: 'pairing' }
  | { kind: 'none'; reason: AuthFailureReason | null }
  | { kind: 'error' };

/**
 * `GET /api/auth/session` with whatever cookie the browser holds. It never
 * sends a bearer: this asks about the cookie session only.
 */
export async function fetchAuthSession(
  fetchImpl: FetchLike = fetch,
  now: () => number = Date.now,
): Promise<SessionProbe> {
  let response: Response;
  try {
    response = await fetchThroughRestart(
      gatewayUrl('/api/auth/session'),
      { cache: 'no-store', credentials: 'same-origin' },
      fetchImpl,
    );
  } catch {
    return { kind: 'error' };
  }
  if (response.status === 401) {
    return { kind: 'none', reason: failureReason(await response.text().catch(() => '')) };
  }
  if (!response.ok) return { kind: 'error' };
  const body: unknown = await response.json().catch(() => null);
  if (body && typeof body === 'object' && (body as { kind?: unknown }).kind === 'pairing') {
    return { kind: 'pairing' };
  }
  const session = parseOidcSession(body);
  if (!session) return { kind: 'error' };
  return { kind: 'oidc', session, clockOffsetMs: serverClockOffset(response, now()) };
}

/** Server clock minus browser clock from the response `Date` header, or 0. */
export function serverClockOffset(response: Pick<Response, 'headers'>, nowMs: number): number {
  const date = response.headers.get('Date');
  const serverMs = date ? Date.parse(date) : Number.NaN;
  return Number.isFinite(serverMs) ? serverMs - nowMs : 0;
}

/** The app path (router path, without the gateway prefix) to come back to. */
export function currentReturnPath(
  location: Pick<Location, 'pathname' | 'search'> = window.location,
): string {
  let path = location.pathname;
  if (basePath && (path === basePath || path.startsWith(`${basePath}/`))) {
    path = path.slice(basePath.length);
  }
  if (!path.startsWith('/')) path = `/${path}`;
  return `${path}${location.search}`;
}

/** `/auth/sign-in/{alias}`: a full-page start, or the re-auth popup. */
export function signInUrl(
  alias: string,
  options: { returnPath?: string; popup?: boolean } = {},
): string {
  const params = new URLSearchParams();
  if (options.popup) {
    params.set('popup', '1');
  } else if (options.returnPath) {
    params.set('return', options.returnPath);
  }
  const query = params.toString();
  return gatewayUrl(`/auth/sign-in/${encodeURIComponent(alias)}${query ? `?${query}` : ''}`);
}

/**
 * `POST /auth/sign-out` for the held session. Resolves once the gateway has
 * dropped it (or it was already gone); throws when that can't be confirmed.
 */
export async function postSignOut(
  csrfToken: string,
  fetchImpl: FetchLike = fetch,
): Promise<void> {
  const response = await fetchThroughRestart(
    gatewayUrl('/auth/sign-out'),
    {
      method: 'POST',
      credentials: 'same-origin',
      headers: { [CSRF_HEADER]: csrfToken },
    },
    fetchImpl,
  );
  if (response.ok || response.status === 401) return;
  throw new Error(`Sign-out failed (${response.status})`);
}

// ---------------------------------------------------------------------------
// Startup
// ---------------------------------------------------------------------------

export type InitialAuth =
  | { kind: 'paired' }
  | { kind: 'open' }
  | { kind: 'signed-in'; session: OidcSession; clockOffsetMs: number; providers: OidcMethod[] }
  | { kind: 'signed-out'; providers: OidcMethod[] };

export interface InitialAuthDeps {
  /** A pairing token is stored. */
  hasToken: boolean;
  /** Running in the desktop app, which doesn't offer OIDC sign-in. */
  desktop: boolean;
  getHealth: () => Promise<{ require_pairing: boolean }>;
  getMethods: () => Promise<AuthMethods | null>;
  getSession: () => Promise<SessionProbe>;
}

/**
 * Decide how the dashboard starts.
 *
 * - A stored pairing token wins without a request, exactly as before.
 * - A gateway that doesn't require pairing is open, exactly as before.
 * - Only when the gateway lists sign-in providers is the session cookie
 *   checked; otherwise the result is today's pairing dialog.
 */
export async function resolveInitialAuth(deps: InitialAuthDeps): Promise<InitialAuth> {
  if (deps.hasToken) return { kind: 'paired' };
  const [health, methods] = await Promise.all([
    deps.getHealth().catch(() => null),
    deps.desktop ? Promise.resolve(null) : deps.getMethods().catch(() => null),
  ]);
  if (health && !health.require_pairing) return { kind: 'open' };
  const providers = methods?.oidc ?? [];
  if (providers.length > 0) {
    const probe = await deps.getSession().catch((): SessionProbe => ({ kind: 'error' }));
    if (probe.kind === 'oidc') {
      return {
        kind: 'signed-in',
        session: probe.session,
        clockOffsetMs: probe.clockOffsetMs,
        providers,
      };
    }
  }
  return { kind: 'signed-out', providers };
}

// ---------------------------------------------------------------------------
// Expiry and re-authentication
// ---------------------------------------------------------------------------

export type ExpiryPhase = 'active' | 'warning' | 'expired';

/** Milliseconds left on the session, on the server's clock. */
export function msUntilExpiry(expiresAtSec: number, nowMs: number, offsetMs: number): number {
  return expiresAtSec * 1000 - (nowMs + offsetMs);
}

export function expiryPhase(expiresAtSec: number, nowMs: number, offsetMs: number): ExpiryPhase {
  const left = msUntilExpiry(expiresAtSec, nowMs, offsetMs);
  if (left <= 0) return 'expired';
  if (left <= REAUTH_WARNING_MS) return 'warning';
  return 'active';
}

export type SignedInDecision = 'adopt' | 'resume' | 'reload';

/**
 * What a tab does with a session it just read after a sign-in somewhere in the
 * browser: adopt it (this tab had none), resume (same principal: keep the
 * page), or reload (another account: the page's data belongs to the old one).
 */
export function decideSignedIn(
  current: OidcSession | null,
  next: OidcSession,
): SignedInDecision {
  if (!current) return 'adopt';
  return current.principal_id === next.principal_id ? 'resume' : 'reload';
}

export type RefreshOutcome = 'resumed' | 'reloading' | 'expired' | 'signed-out' | 'error';

/**
 * Re-read the cookie session and apply it: adopt or resume (and tell stopped
 * sockets to reconnect), or reload for a different account. Reports a missing
 * session without acting on it, so the caller decides between the re-auth
 * prompt and the sign-in screen.
 */
export async function refreshOidcSession(
  deps: {
    probe?: () => Promise<SessionProbe>;
    reload?: () => void;
  } = {},
): Promise<RefreshOutcome> {
  const probe = await (deps.probe ?? (() => fetchAuthSession()))();
  switch (probe.kind) {
    case 'oidc': {
      if (decideSignedIn(getOidcSession(), probe.session) === 'reload') {
        (deps.reload ?? (() => window.location.reload()))();
        return 'reloading';
      }
      setOidcSession(probe.session, probe.clockOffsetMs);
      window.dispatchEvent(new Event(AUTH_EVENTS.resumed));
      return 'resumed';
    }
    case 'none':
      return probe.reason === 'session_expired' ? 'expired' : 'signed-out';
    default:
      return 'error';
  }
}

// ---------------------------------------------------------------------------
// Cross-tab channel
// ---------------------------------------------------------------------------

/**
 * A message on `AUTH_CHANNEL`. The re-auth window posts `signed-in` (it may
 * carry more fields, which are ignored: each tab re-reads the session itself).
 */
export function parseChannelMessage(data: unknown): AuthChannelMessage | null {
  if (!data || typeof data !== 'object') return null;
  const type = (data as { type?: unknown }).type;
  return type === 'signed-in' || type === 'signed-out' ? { type } : null;
}
