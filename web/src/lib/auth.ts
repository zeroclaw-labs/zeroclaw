const TOKEN_KEY = 'zeroclaw_token';

// ---------------------------------------------------------------------------
// Auth mode
// ---------------------------------------------------------------------------
//
// The dashboard authenticates one of two ways:
//
// - `pairing` (the default): a bearer token from a pairing code, kept in
//   localStorage and sent as `Authorization` / `?token=` / `bearer.<token>`.
// - `oidc`: dashboard sign-in through an OIDC provider. The credential is an
//   httpOnly session cookie the browser sends on its own; nothing is stored,
//   and the only secret the page holds is the session's CSRF token, in memory.
//
// The mode is `oidc` exactly while a signed-in session is held here, which only
// happens when the gateway has dashboard sign-in configured.

export type AuthMode = 'pairing' | 'oidc';

/** `GET /api/auth/session` for a cookie session. */
export interface OidcSession {
  kind: 'oidc';
  /** `oidc.<alias>` */
  provider: string;
  principal_id: string;
  display_name: string;
  admin: boolean;
  /** Unix seconds. */
  expires_at: number;
  csrf_token: string;
  features: {
    chat: boolean;
    operator_pages: boolean;
  };
}

/** Header carrying the session's CSRF token on unsafe requests. */
export const CSRF_HEADER = 'X-ZeroClaw-CSRF';

/**
 * Window events the auth layer dispatches. `unauthorized` drops to the sign-in
 * screen; `reauth` asks for a new sign-in while keeping the page;
 * `sessionEnded` (a socket's session ended) and `accessLost` ask the auth
 * provider to re-check the session first; `resumed` tells stopped sockets and
 * streams that a session is usable again.
 */
export const AUTH_EVENTS = {
  unauthorized: 'zeroclaw-unauthorized',
  reauth: 'zeroclaw-reauth',
  sessionEnded: 'zeroclaw-session-ended',
  accessLost: 'zeroclaw-access-lost',
  resumed: 'zeroclaw-auth-resumed',
} as const;

let oidcSession: OidcSession | null = null;
// The session expired and the page waits for a new sign-in. Sockets and
// streams don't retry on a timer meanwhile: every attempt would be refused.
let reauthPending = false;
// Server clock minus browser clock, in ms, measured when the session was read.
// Expiry is computed on the server's clock so a skewed browser clock can't
// make a fresh session look expired.
let clockOffsetMs = 0;
// The principal of the last session this page held, kept after it ends: the
// page behind the sign-in screen still holds that account's state (unsent
// drafts, loaded data), so only the same account may be let back in place.
let lastPrincipalId: string | null = null;
const sessionListeners = new Set<(session: OidcSession | null) => void>();

/** The current auth mode. */
export function getAuthMode(): AuthMode {
  return oidcSession ? 'oidc' : 'pairing';
}

/** The signed-in OIDC session, or null in pairing mode. */
export function getOidcSession(): OidcSession | null {
  return oidcSession;
}

/** The principal of the last session this page held, or null if it held none. */
export function getLastPrincipalId(): string | null {
  return lastPrincipalId;
}

/** Server clock minus browser clock for the held session, in ms. */
export function getClockOffsetMs(): number {
  return clockOffsetMs;
}

/** True while the page waits for a new sign-in after the session expired. */
export function isReauthPending(): boolean {
  return reauthPending;
}

export function markReauthPending(pending: boolean): void {
  reauthPending = pending;
}

/** Enter (or refresh) OIDC mode, or leave it with `null`. */
export function setOidcSession(session: OidcSession | null, offsetMs = 0): void {
  oidcSession = session;
  if (session) lastPrincipalId = session.principal_id;
  clockOffsetMs = session ? offsetMs : 0;
  reauthPending = false;
  for (const listener of [...sessionListeners]) listener(session);
}

/** Observe session changes. Returns the unsubscribe function. */
export function subscribeOidcSession(
  listener: (session: OidcSession | null) => void,
): () => void {
  sessionListeners.add(listener);
  return () => {
    sessionListeners.delete(listener);
  };
}

/** The provider alias of a session (`oidc.corp` → `corp`). */
export function providerAlias(provider: string): string {
  return provider.startsWith('oidc.') ? provider.slice('oidc.'.length) : provider;
}

/** True for methods that change state and so need the CSRF header. */
export function isUnsafeMethod(method: string): boolean {
  const m = method.toUpperCase();
  return m !== 'GET' && m !== 'HEAD' && m !== 'OPTIONS';
}

/**
 * Credential headers for a request in the current mode: the pairing bearer, or
 * in OIDC mode the CSRF token on unsafe methods (the cookie goes on its own).
 */
export function authHeaders(method: string, init?: HeadersInit): Headers {
  const headers = new Headers(init);
  if (oidcSession) {
    if (isUnsafeMethod(method)) headers.set(CSRF_HEADER, oidcSession.csrf_token);
    return headers;
  }
  const token = getToken();
  if (token) headers.set('Authorization', `Bearer ${token}`);
  return headers;
}

// ---------------------------------------------------------------------------
// Pairing token
// ---------------------------------------------------------------------------

function readStoredToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

/**
 * The pairing bearer token to send, or null. Always null in OIDC mode: a
 * signed-in browser must never send `Authorization`, `?token=` or `bearer.`,
 * because the gateway prefers an explicit bearer over the session cookie.
 */
export function getToken(): string | null {
  if (oidcSession) return null;
  return readStoredToken();
}

/**
 * Store an authentication token.
 */
export function setToken(token: string): void {
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    // localStorage may be unavailable (e.g. in some private browsing modes)
  }
}

/**
 * Remove the stored authentication token.
 */
export function clearToken(): void {
  try {
    localStorage.removeItem(TOKEN_KEY);
  } catch {
    // Ignore
  }
}

/**
 * Returns true if a token is currently stored.
 */
export function isAuthenticated(): boolean {
  const token = readStoredToken();
  return token !== null && token.length > 0;
}
