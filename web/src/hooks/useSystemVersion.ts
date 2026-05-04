import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getSystemVersion,
  type SystemVersion,
} from '@/lib/api';

const POLL_INTERVAL_MS = 60_000;

export interface UseSystemVersionResult {
  version: SystemVersion | null;
  error: Error | null;
  loading: boolean;
  refetch: () => void;
}

/**
 * Polls `GET /api/system/version` every 60 seconds.
 *
 * Surfaces the dashboard's "Up to date / Update available" state without
 * needing to refresh the page. The first response gates the rest of the
 * SystemCard so it can render a coherent state immediately.
 */
export function useSystemVersion(): UseSystemVersionResult {
  const [version, setVersion] = useState<SystemVersion | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const mounted = useRef(true);

  const fetchOnce = useCallback(async () => {
    try {
      const v = await getSystemVersion();
      if (!mounted.current) return;
      setVersion(v);
      setError(null);
    } catch (e) {
      if (!mounted.current) return;
      setError(e instanceof Error ? e : new Error(String(e)));
    } finally {
      if (mounted.current) setLoading(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    fetchOnce();
    const id = window.setInterval(fetchOnce, POLL_INTERVAL_MS);
    return () => {
      mounted.current = false;
      window.clearInterval(id);
    };
  }, [fetchOnce]);

  return { version, error, loading, refetch: fetchOnce };
}
