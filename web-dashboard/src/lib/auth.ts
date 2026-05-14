/**
 * Bearer-token storage for authenticated gateway calls.
 *
 * The dashboard shares the `zeroclaw_token` localStorage key with the
 * legacy `web/` app so a user who has paired once works across both
 * surfaces without re-pairing. `auth.ts` stays a thin module — any
 * access to `localStorage` is wrapped in `try/catch` because private
 * browsing modes throw on access.
 */

const TOKEN_KEY = "zeroclaw_token";

export function getToken(): string | null {
  try {
    return localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

export function setToken(token: string): void {
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    // localStorage unavailable (private mode) — best-effort only.
  }
}

export function clearToken(): void {
  try {
    localStorage.removeItem(TOKEN_KEY);
  } catch {
    // Ignore.
  }
}

export function isAuthenticated(): boolean {
  const token = getToken();
  return token !== null && token.length > 0;
}
