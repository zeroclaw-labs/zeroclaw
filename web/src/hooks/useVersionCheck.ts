import { useState, useEffect, useCallback, useRef } from 'react';
import { checkVersion } from '../lib/api';
import type { VersionCheckResponse } from '../lib/api';

const SIX_HOURS_MS = 6 * 60 * 60 * 1000;
const ONE_HOUR_MS = 60 * 60 * 1000;

interface UseVersionCheckResult {
  info: VersionCheckResponse | null;
  loading: boolean;
  /** Force a fresh check (bypasses the server-side cache). */
  refetch: () => void;
}

/**
 * Poll for a newer release: once on mount, every 6h, and when the tab becomes
 * visible again (if it's been >1h since the last check). Read-only — it never
 * triggers an upgrade. Errors are swallowed into the returned `info.error`
 * field so the version tag degrades gracefully.
 *
 * Pass `enabled = false` (e.g. when `gateway.check_updates` is off) to skip
 * automatic polling. `refetch()` still runs a one-shot check even when
 * automatic polling is disabled — the user pressing the refresh button is an
 * explicit, on-demand check that bypasses the passive-polling flag (matches
 * the operator's intent when opening the upgrade dialog manually).
 */
export function useVersionCheck(enabled = true): UseVersionCheckResult {
  const [info, setInfo] = useState<VersionCheckResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const lastCheckRef = useRef(0);

  const run = useCallback(
    async (force = false) => {
      setLoading(true);
      try {
        const result = await checkVersion(force ? { force: true } : undefined);
        setInfo(result);
      } catch (e) {
        setInfo({
          current_version: '',
          latest_version: null,
          is_newer: false,
          error: e instanceof Error ? e.message : String(e),
        });
      } finally {
        lastCheckRef.current = Date.now();
        setLoading(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (!enabled) return;
    void run();
    const interval = window.setInterval(() => void run(), SIX_HOURS_MS);
    const onVisible = () => {
      if (
        document.visibilityState === 'visible' &&
        Date.now() - lastCheckRef.current > ONE_HOUR_MS
      ) {
        void run();
      }
    };
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      window.clearInterval(interval);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [enabled, run]);

  const refetch = useCallback(() => void run(true), [run]);

  return { info, loading, refetch };
}
