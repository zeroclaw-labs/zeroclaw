// How the dashboard decides, from the gateway's public `/health`, whether a
// browser must sign in and how. Pure functions so the decisions are testable
// without a browser.

/** The public health body, as far as signing in is concerned. */
export interface PublicHealth {
  /** True when every request needs a bearer. */
  require_pairing?: boolean;
  paired?: boolean;
  /**
   * Sign-in methods the gateway offers. The in-process gateway omits this
   * and pairs with a code; the preview `zeroclaw-gw` has no pairing-code
   * exchange and takes an existing token instead.
   */
  sign_in?: {
    pairing_code?: boolean;
    bearer?: boolean;
    /** Same-origin path that answers 200 for a token the core accepts. */
    verify?: string;
  };
  /** Present when the gateway is up but cannot reach its core. */
  code?: string;
  error?: string;
  hint?: string;
}

/** A gateway problem shown above the sign-in form. */
export interface LinkBanner {
  code: string;
  error: string;
  hint: string | null;
}

export type LoginMode = 'pairing' | 'bearer';

export interface AuthPosture {
  /** Whether a browser without a token must sign in. */
  requiresPairing: boolean;
  /** Whether this browser may use the dashboard right away. */
  authenticated: boolean;
  loginMode: LoginMode;
  banner: LinkBanner | null;
}

/** The preview gateway's core-link route, used when `verify` is unusable. */
export const DEFAULT_VERIFY_PATH = '/api/gateway/core';

/**
 * The posture for a browser that holds `hasToken`, given `/health` (or
 * `null` when it could not be read). Only an explicit
 * `require_pairing: false` opens the dashboard without a token: a missing
 * field or an unreadable body means sign in.
 */
export function postureFromHealth(
  health: PublicHealth | null,
  hasToken: boolean,
): AuthPosture {
  const open = health?.require_pairing === false;
  const bearerOnly =
    health?.sign_in?.bearer === true && health.sign_in.pairing_code === false;
  const banner =
    health && typeof health.code === 'string' && typeof health.error === 'string'
      ? { code: health.code, error: health.error, hint: health.hint ?? null }
      : null;
  return {
    requiresPairing: !open,
    authenticated: hasToken || open,
    loginMode: bearerOnly ? 'bearer' : 'pairing',
    banner,
  };
}

/**
 * Where to check a token. The path comes from the gateway's own health body,
 * so it is accepted only as a same-origin absolute path; anything else falls
 * back to the default, and the token is never sent to another origin.
 */
export function verifyPath(health: PublicHealth | null): string {
  const path = health?.sign_in?.verify;
  if (
    typeof path === 'string' &&
    path.startsWith('/') &&
    !path.startsWith('//') &&
    !path.includes('\\')
  ) {
    return path;
  }
  return DEFAULT_VERIFY_PATH;
}

export type SignInResult =
  | { ok: true }
  | { ok: false; error: string; banner: LinkBanner | null };

/** What a token check answered: accepted, rejected, or the gateway's trouble. */
export function signInResult(status: number, body: unknown): SignInResult {
  if (status >= 200 && status < 300) return { ok: true };
  const fields = (body && typeof body === 'object' ? body : {}) as Record<string, unknown>;
  const error = typeof fields.error === 'string' ? fields.error : `Sign-in failed (${status})`;
  if (status === 401 || status === 403) {
    return { ok: false, error: 'That token was not accepted.', banner: null };
  }
  const banner =
    typeof fields.code === 'string'
      ? {
          code: fields.code,
          error,
          hint: typeof fields.hint === 'string' ? fields.hint : null,
        }
      : null;
  return { ok: false, error, banner };
}
