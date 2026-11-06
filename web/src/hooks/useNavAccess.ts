import { useEffect, useState, useSyncExternalStore } from 'react';
import { getAvailableAgents, type AvailableAgent } from '@/lib/api';
import { getOidcSession, subscribeOidcSession, type OidcSession } from '@/lib/auth';
import { navAccessFor, type NavAccess } from '@/lib/navAccess';

/**
 * What the current caller can reach, read from the session `lib/auth` holds.
 * It needs no `<AuthProvider>`: with no signed-in session (a paired browser,
 * or a component rendered on its own) it reports full access.
 */
export function useNavAccess(): NavAccess {
  return navAccessFor(useOidcSession());
}

/** The signed-in dashboard session `lib/auth` holds, or null when paired. */
export function useOidcSession(): OidcSession | null {
  return useSyncExternalStore(subscribeOidcSession, getOidcSession, getOidcSession);
}

export interface AvailableAgentsState {
  loading: boolean;
  agents: AvailableAgent[];
  error: string | null;
}

/** `GET /api/agents/available`, loaded once per mount while `enabled`. */
export function useAvailableAgents(enabled: boolean): AvailableAgentsState {
  const [state, setState] = useState<AvailableAgentsState>({
    loading: enabled,
    agents: [],
    error: null,
  });
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    setState((s) => ({ ...s, loading: true, error: null }));
    getAvailableAgents()
      .then((agents) => {
        if (!cancelled) setState({ loading: false, agents, error: null });
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setState({
            loading: false,
            agents: [],
            error: err instanceof Error ? err.message : String(err),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [enabled]);
  return state;
}
