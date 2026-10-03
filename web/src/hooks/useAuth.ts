import {
  createContext,
  useContext,
  useState,
  useCallback,
  useEffect,
  type ReactNode,
} from 'react';
import React from 'react';
import {
  getToken as readToken,
  setToken as writeToken,
  clearToken as removeToken,
  isAuthenticated as checkAuth,
} from '../lib/auth';
import { pair as apiPair, getPublicHealth, checkBearerToken } from '../lib/api';
import {
  postureFromHealth,
  signInResult,
  verifyPath,
  type LinkBanner,
  type LoginMode,
  type PublicHealth,
} from '../lib/authBootstrap';

// ---------------------------------------------------------------------------
// Context shape
// ---------------------------------------------------------------------------

export interface AuthState {
  /** The current bearer token, or null if not authenticated. */
  token: string | null;
  /** Whether the user is currently authenticated. */
  isAuthenticated: boolean;
  /** Whether the server requires pairing. Defaults to true (safe fallback). */
  requiresPairing: boolean;
  /** True while the initial auth check is in progress. */
  loading: boolean;
  /** How a signed-out browser signs in: a pairing code, or an existing token. */
  loginMode: LoginMode;
  /** A gateway problem to show on the sign-in screen, if any. */
  linkBanner: LinkBanner | null;
  /** Pair with the agent using a pairing code. Stores the token on success. */
  pair: (code: string) => Promise<void>;
  /**
   * Sign in with an existing bearer token: checked with the gateway first,
   * stored only if accepted.
   */
  signInWithToken: (token: string) => Promise<void>;
  /** Clear the stored token and sign out. */
  logout: () => void;
}

const AuthContext = createContext<AuthState | null>(null);

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

export interface AuthProviderProps {
  children: ReactNode;
}

export function AuthProvider({ children }: AuthProviderProps) {
  const [token, setTokenState] = useState<string | null>(readToken);
  const [authenticated, setAuthenticated] = useState<boolean>(checkAuth);
  const [requiresPairing, setRequiresPairing] = useState<boolean>(true);
  const [loading, setLoading] = useState<boolean>(!checkAuth());
  const [health, setHealth] = useState<PublicHealth | null>(null);
  const [loginMode, setLoginMode] = useState<LoginMode>('pairing');
  const [linkBanner, setLinkBanner] = useState<LinkBanner | null>(null);

  // On mount: learn how this gateway signs browsers in. Read even with a
  // stored token, so a token rejected later still lands on the right
  // sign-in screen. Only an explicit `require_pairing: false` opens the
  // dashboard without a token.
  useEffect(() => {
    let cancelled = false;
    getPublicHealth()
      .then((body) => {
        if (cancelled) return;
        const posture = postureFromHealth(body, checkAuth());
        setHealth(body);
        setRequiresPairing(posture.requiresPairing);
        setLoginMode(posture.loginMode);
        setLinkBanner(posture.banner);
        if (posture.authenticated) setAuthenticated(true);
      })
      .catch(() => {
        // health endpoint unreachable — fall back to showing pairing dialog
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Keep state in sync if localStorage is changed in another tab
  useEffect(() => {
    const handler = (e: StorageEvent) => {
      if (e.key === 'zeroclaw_token') {
        const t = readToken();
        setTokenState(t);
        setAuthenticated(t !== null && t.length > 0);
      }
    };
    window.addEventListener('storage', handler);
    return () => window.removeEventListener('storage', handler);
  }, []);

  const pair = useCallback(async (code: string): Promise<void> => {
    const { token: newToken } = await apiPair(code);
    writeToken(newToken);
    setTokenState(newToken);
    setAuthenticated(true);
  }, []);

  const signInWithToken = useCallback(
    async (candidate: string): Promise<void> => {
      const trimmed = candidate.trim();
      const { status, body } = await checkBearerToken(verifyPath(health), trimmed);
      const result = signInResult(status, body);
      if (!result.ok) {
        setLinkBanner(result.banner);
        throw new Error(result.error);
      }
      writeToken(trimmed);
      setTokenState(trimmed);
      setLinkBanner(null);
      setAuthenticated(true);
    },
    [health],
  );

  const logout = useCallback((): void => {
    removeToken();
    setTokenState(null);
    setAuthenticated(false);
  }, []);

  const value: AuthState = {
    token,
    isAuthenticated: authenticated,
    requiresPairing,
    loading,
    loginMode,
    linkBanner,
    pair,
    signInWithToken,
    logout,
  };

  return React.createElement(AuthContext.Provider, { value }, children);
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Access the authentication state from any component inside `<AuthProvider>`.
 * Throws if used outside the provider.
 */
export function useAuth(): AuthState {
  const ctx = useContext(AuthContext);
  if (!ctx) {
    throw new Error('useAuth must be used within an <AuthProvider>');
  }
  return ctx;
}
