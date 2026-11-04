import {
  createContext,
  useContext,
  useState,
  useCallback,
  useEffect,
  useRef,
  type ReactNode,
} from 'react';
import React from 'react';
import {
  AUTH_EVENTS,
  getToken as readToken,
  setToken as writeToken,
  clearToken as removeToken,
  isAuthenticated as checkAuth,
  getClockOffsetMs,
  getOidcSession,
  providerAlias,
  markReauthPending,
  setOidcSession,
  subscribeOidcSession,
  type AuthMode,
  type OidcSession,
} from '../lib/auth';
import { pair as apiPair, getPublicHealth } from '../lib/api';
import {
  AUTH_CHANNEL,
  REAUTH_WARNING_MS,
  currentReturnPath,
  expiryPhase,
  fetchAuthMethods,
  fetchAuthSession,
  msUntilExpiry,
  parseChannelMessage,
  postSignOut,
  refreshOidcSession,
  resolveInitialAuth,
  signInUrl,
  type AuthChannelMessage,
  type OidcMethod,
} from '../lib/authSession';
import { isTauri } from '../lib/tauri';

// ---------------------------------------------------------------------------
// Context shape
// ---------------------------------------------------------------------------

/** Why the sign-in screen is showing after a signed-in session. */
export type SignOutNotice =
  /** The user signed out (here or in another tab). `provider` is the alias. */
  | { kind: 'signed-out'; provider: string }
  /** The gateway ended the session (revoked, or the provider was removed). */
  | { kind: 'session-ended' };

export type ReauthAttempt = 'opened' | 'blocked';

export interface AuthState {
  /** The current bearer token, or null if not authenticated. */
  token: string | null;
  /** Whether the user is currently authenticated. */
  isAuthenticated: boolean;
  /** Whether the server requires pairing. Defaults to true (safe fallback). */
  requiresPairing: boolean;
  /** True while the initial auth check is in progress. */
  loading: boolean;
  /** Pair with the agent using a pairing code. Stores the token on success. */
  pair: (code: string) => Promise<void>;
  /** Clear the stored token and sign out. */
  logout: () => void;

  /** `oidc` while signed in through a provider, else `pairing`. */
  mode: AuthMode;
  /** The signed-in session, or null. */
  session: OidcSession | null;
  /** Sign-in providers to offer. Empty when sign-in is off and in the desktop app. */
  providers: OidcMethod[];
  /** The session expires within two minutes. */
  expiryWarning: boolean;
  /** The session expired; the page stays until the user signs in again. */
  reauthRequired: boolean;
  /** A re-auth window is open and hasn't reported back yet. */
  reauthPending: boolean;
  /** Shown on the sign-in screen after a signed-in session ended. */
  signOutNotice: SignOutNotice | null;
  /** Start sign-in with a provider (full-page). */
  signIn: (alias: string) => void;
  /** End the signed-in session. Rejects when the gateway can't confirm it. */
  signOut: () => Promise<void>;
  /** Open the re-auth window for the session's provider. */
  reauthenticate: () => ReauthAttempt;
  /** Re-auth in this tab instead (when the window is blocked). */
  continueReauthHere: () => void;
  /** Re-read the session now, e.g. after finishing sign-in elsewhere. */
  checkSession: () => void;
}

const AuthContext = createContext<AuthState | null>(null);

type Phase = 'loading' | 'open' | 'paired' | 'oidc' | 'signed-out';

// setTimeout clamps larger delays to 1 ms.
const MAX_TIMER_MS = 2 ** 31 - 1;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

export interface AuthProviderProps {
  children: ReactNode;
}

export function AuthProvider({ children }: AuthProviderProps) {
  const [token, setTokenState] = useState<string | null>(readToken);
  const [phase, setPhase] = useState<Phase>(() => (checkAuth() ? 'paired' : 'loading'));
  const [session, setSession] = useState<OidcSession | null>(getOidcSession);
  const [providers, setProviders] = useState<OidcMethod[]>([]);
  const [providersKnown, setProvidersKnown] = useState(false);
  const [expiryWarning, setExpiryWarning] = useState(false);
  const [reauthRequired, setReauthRequired] = useState(false);
  const [reauthPending, setReauthPending] = useState(false);
  const [signOutNotice, setSignOutNotice] = useState<SignOutNotice | null>(null);

  // Event handlers registered once read the latest state through these.
  const phaseRef = useRef(phase);
  const reauthRequiredRef = useRef(reauthRequired);
  const providersRef = useRef(providers);
  phaseRef.current = phase;
  reauthRequiredRef.current = reauthRequired;
  providersRef.current = providers;

  // Sockets and streams read this to stop timed retries while re-auth is due.
  useEffect(() => {
    markReauthPending(reauthRequired);
  }, [reauthRequired]);

  const channelRef = useRef<BroadcastChannel | null>(null);
  const post = useCallback((message: AuthChannelMessage) => {
    channelRef.current?.postMessage(message);
  }, []);

  // Mirror the session held by lib/auth, which api/ws/sse read synchronously.
  // A new or refreshed session clears any expiry state.
  useEffect(
    () =>
      subscribeOidcSession((next) => {
        setSession(next);
        if (next) {
          setPhase('oidc');
          setReauthRequired(false);
          setReauthPending(false);
          setSignOutNotice(null);
        }
      }),
    [],
  );

  // On mount: decide between a stored pairing token, an open gateway, a
  // signed-in session and the sign-in screen.
  useEffect(() => {
    if (checkAuth()) return; // already have a token, no need to check
    let cancelled = false;
    resolveInitialAuth({
      hasToken: false,
      desktop: isTauri(),
      getHealth: getPublicHealth,
      getMethods: () => fetchAuthMethods(),
      getSession: () => fetchAuthSession(),
    })
      .then((initial) => {
        if (cancelled) return;
        switch (initial.kind) {
          case 'open':
            setPhase('open');
            break;
          case 'paired':
            setPhase('paired');
            break;
          case 'signed-in':
            setProviders(initial.providers);
            setProvidersKnown(true);
            setOidcSession(initial.session, initial.clockOffsetMs);
            break;
          case 'signed-out':
            setProviders(initial.providers);
            setProvidersKnown(true);
            setPhase('signed-out');
            break;
        }
      })
      .catch(() => {
        if (!cancelled) setPhase('signed-out');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // A paired browser that lost its token never asked which providers exist.
  // The answer marks them known: marking them before it would re-run this
  // effect, and its cleanup would drop the answer.
  useEffect(() => {
    if (phase !== 'signed-out' || providersKnown || isTauri()) return;
    let cancelled = false;
    fetchAuthMethods()
      .then((methods) => {
        if (cancelled) return;
        if (methods) setProviders(methods.oidc);
        setProvidersKnown(true);
      })
      .catch(() => {
        // No providers to offer; the pairing form stays.
      });
    return () => {
      cancelled = true;
    };
  }, [phase, providersKnown]);

  const dropToSignedOut = useCallback((notice: SignOutNotice | null) => {
    setOidcSession(null);
    setExpiryWarning(false);
    setReauthRequired(false);
    setReauthPending(false);
    setSignOutNotice(notice);
    setPhase('signed-out');
  }, []);

  // Re-read the cookie session. A resumed session lands through the
  // subscription above; while re-auth is pending, a missing session keeps the
  // prompt (and the page) rather than signing out.
  const recheck = useCallback(async () => {
    const outcome = await refreshOidcSession();
    if (phaseRef.current !== 'oidc') return;
    if (outcome === 'expired') {
      setReauthRequired(true);
    } else if (outcome === 'signed-out' && !reauthRequiredRef.current) {
      dropToSignedOut({ kind: 'session-ended' });
    }
  }, [dropToSignedOut]);

  // Auth signals from api/ws/sse.
  useEffect(() => {
    const onUnauthorized = () => {
      const current = phaseRef.current;
      if (current === 'oidc') {
        // After expiry every request is refused; that's still the expiry.
        if (reauthRequiredRef.current) return;
        dropToSignedOut({ kind: 'session-ended' });
        return;
      }
      removeToken();
      setTokenState(null);
      if (current === 'paired') setPhase('signed-out');
    };
    const onReauth = () => {
      if (phaseRef.current === 'oidc') setReauthRequired(true);
    };
    // A socket's session ended (4401). After an early re-auth the browser
    // already holds a newer session: resuming reconnects the socket onto it.
    // Only when there is none does the page ask for a new sign-in.
    const onSessionEnded = () => {
      if (phaseRef.current !== 'oidc') return;
      void refreshOidcSession().then((outcome) => {
        if (phaseRef.current !== 'oidc') return;
        if (outcome !== 'resumed' && outcome !== 'reloading') setReauthRequired(true);
      });
    };
    const onAccessLost = () => {
      if (phaseRef.current === 'oidc') void recheck();
    };
    window.addEventListener(AUTH_EVENTS.unauthorized, onUnauthorized);
    window.addEventListener(AUTH_EVENTS.reauth, onReauth);
    window.addEventListener(AUTH_EVENTS.sessionEnded, onSessionEnded);
    window.addEventListener(AUTH_EVENTS.accessLost, onAccessLost);
    return () => {
      window.removeEventListener(AUTH_EVENTS.unauthorized, onUnauthorized);
      window.removeEventListener(AUTH_EVENTS.reauth, onReauth);
      window.removeEventListener(AUTH_EVENTS.sessionEnded, onSessionEnded);
      window.removeEventListener(AUTH_EVENTS.accessLost, onAccessLost);
    };
  }, [dropToSignedOut, recheck]);

  // Sign-in and sign-out in other tabs, and the re-auth window's report.
  useEffect(() => {
    if (typeof BroadcastChannel === 'undefined') return;
    const channel = new BroadcastChannel(AUTH_CHANNEL);
    channelRef.current = channel;
    channel.onmessage = (event: MessageEvent) => {
      const message = parseChannelMessage(event.data);
      if (!message) return;
      const current = phaseRef.current;
      if (message.type === 'signed-in') {
        if (current === 'oidc' || (current === 'signed-out' && providersRef.current.length > 0)) {
          void recheck();
        }
      } else if (current === 'oidc') {
        const held = getOidcSession();
        dropToSignedOut(
          held
            ? { kind: 'signed-out', provider: providerAlias(held.provider) }
            : { kind: 'session-ended' },
        );
      }
    };
    return () => {
      channelRef.current = null;
      channel.close();
    };
  }, [dropToSignedOut, recheck]);

  // While the re-auth window is open, coming back to this tab re-checks, in
  // case its report was missed.
  useEffect(() => {
    if (!reauthPending) return;
    const onFocus = () => void recheck();
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  }, [reauthPending, recheck]);

  // Expiry: the banner two minutes ahead, then the re-auth prompt. Timers are
  // throttled in background tabs, so returning to the tab re-evaluates.
  useEffect(() => {
    if (!session) {
      setExpiryWarning(false);
      return;
    }
    const offset = getClockOffsetMs();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const evaluate = () => {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      const now = Date.now();
      const current = expiryPhase(session.expires_at, now, offset);
      setExpiryWarning(current === 'warning');
      if (current === 'expired') {
        setReauthRequired(true);
        return;
      }
      const left = msUntilExpiry(session.expires_at, now, offset);
      const next = current === 'active' ? left - REAUTH_WARNING_MS : left;
      timer = setTimeout(evaluate, Math.min(Math.max(next, 0), MAX_TIMER_MS));
    };
    evaluate();
    const onVisible = () => {
      if (document.visibilityState === 'visible') evaluate();
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      if (timer !== undefined) clearTimeout(timer);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [session]);

  // Keep state in sync if localStorage is changed in another tab. A signed-in
  // tab doesn't follow pairing changes elsewhere.
  useEffect(() => {
    const handler = (e: StorageEvent) => {
      if (e.key === 'zeroclaw_token') {
        if (getOidcSession()) return;
        const t = readToken();
        setTokenState(t);
        setPhase((p) => (t ? 'paired' : p === 'paired' ? 'signed-out' : p));
      }
    };
    window.addEventListener('storage', handler);
    return () => window.removeEventListener('storage', handler);
  }, []);

  const pair = useCallback(async (code: string): Promise<void> => {
    const { token: newToken } = await apiPair(code);
    writeToken(newToken);
    setTokenState(newToken);
    setSignOutNotice(null);
    setPhase('paired');
  }, []);

  const signIn = useCallback((alias: string) => {
    window.location.assign(signInUrl(alias, { returnPath: currentReturnPath() }));
  }, []);

  const signOut = useCallback(async (): Promise<void> => {
    const held = getOidcSession();
    if (!held) return;
    try {
      await postSignOut(held.csrf_token);
    } catch (err) {
      // A stale CSRF token (the session was replaced in another tab) is
      // refused; re-read the session and try once more with the new one.
      const outcome = await refreshOidcSession();
      const fresh = getOidcSession();
      if (outcome === 'resumed' && fresh) {
        await postSignOut(fresh.csrf_token);
      } else if (outcome !== 'expired' && outcome !== 'signed-out') {
        throw err;
      }
    }
    post({ type: 'signed-out' });
    dropToSignedOut({ kind: 'signed-out', provider: providerAlias(held.provider) });
  }, [dropToSignedOut, post]);

  const logout = useCallback((): void => {
    if (getOidcSession()) {
      void signOut().catch(() => {
        // The account menu reports sign-out failures; this path has no UI.
      });
      return;
    }
    removeToken();
    setTokenState(null);
    setPhase((p) => (p === 'paired' ? 'signed-out' : p));
  }, [signOut]);

  const reauthenticate = useCallback((): ReauthAttempt => {
    const held = getOidcSession();
    if (!held) return 'blocked';
    const popup = window.open(
      signInUrl(providerAlias(held.provider), { popup: true }),
      'zeroclaw-sign-in',
      'popup=yes,width=520,height=720',
    );
    if (!popup) return 'blocked';
    setReauthPending(true);
    return 'opened';
  }, []);

  const continueReauthHere = useCallback(() => {
    const held = getOidcSession();
    if (!held) return;
    window.location.assign(
      signInUrl(providerAlias(held.provider), { returnPath: currentReturnPath() }),
    );
  }, []);

  const checkSession = useCallback(() => {
    void recheck();
  }, [recheck]);

  const value: AuthState = {
    token,
    isAuthenticated: phase === 'open' || phase === 'paired' || phase === 'oidc',
    requiresPairing: phase !== 'open',
    loading: phase === 'loading',
    pair,
    logout,
    mode: session ? 'oidc' : 'pairing',
    session,
    providers,
    expiryWarning,
    reauthRequired,
    reauthPending,
    signOutNotice,
    signIn,
    signOut,
    reauthenticate,
    continueReauthHere,
    checkSession,
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
