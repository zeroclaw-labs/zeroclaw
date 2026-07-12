// Shared live-overlay polling for a SOP run. One implementation drives the
// run detail page and any future surface (zerocode web bridge included):
// poll every 2s, stop on a terminal status, expose a refresh for
// post-decision updates.
import { useCallback, useEffect, useRef, useState } from 'react';
import {
  getRunOverlay,
  isTerminalRunStatus,
  type RunOverlay,
} from '@/lib/sops';

const POLL_MS = 2000;

export interface RunOverlayState {
  overlay: RunOverlay | null;
  error: string | null;
  /// Replace the overlay from an out-of-band source (e.g. the decide
  /// endpoint returns the refreshed overlay) without waiting for a poll.
  setOverlay: (o: RunOverlay) => void;
}

export function useRunOverlay(sop: string, runId: string): RunOverlayState {
  const [overlay, setOverlayState] = useState<RunOverlay | null>(null);
  const [error, setError] = useState<string | null>(null);
  const timerRef = useRef(0);

  const stop = useCallback(() => {
    if (timerRef.current) {
      window.clearInterval(timerRef.current);
      timerRef.current = 0;
    }
  }, []);

  const setOverlay = useCallback(
    (o: RunOverlay) => {
      setOverlayState(o);
      setError(null);
      if (isTerminalRunStatus(o.status)) stop();
    },
    [stop],
  );

  useEffect(() => {
    if (!sop || !runId) return;
    let active = true;
    const poll = () => {
      getRunOverlay(sop, runId)
        .then((o) => {
          if (!active) return;
          setOverlayState(o);
          setError(null);
          if (isTerminalRunStatus(o.status)) stop();
        })
        .catch((e: unknown) => {
          if (active) setError(e instanceof Error ? e.message : String(e));
        });
    };
    poll();
    timerRef.current = window.setInterval(poll, POLL_MS);
    return () => {
      active = false;
      stop();
    };
  }, [sop, runId, stop]);

  return { overlay, error, setOverlay };
}
