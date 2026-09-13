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
import { LatestOverlayWriteGate } from './useRunOverlay.logic';

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
  const writeGateRef = useRef<LatestOverlayWriteGate | null>(null);
  if (!writeGateRef.current) writeGateRef.current = new LatestOverlayWriteGate();

  const stop = useCallback(() => {
    if (timerRef.current) {
      window.clearInterval(timerRef.current);
      timerRef.current = 0;
    }
  }, []);

  const setOverlay = useCallback(
    (o: RunOverlay) => {
      writeGateRef.current?.supersedePendingRequests();
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
      const requestRevision = writeGateRef.current?.beginRequest() ?? 0;
      getRunOverlay(sop, runId)
        .then((o) => {
          if (!active || !writeGateRef.current?.isCurrent(requestRevision)) return;
          setOverlayState(o);
          setError(null);
          if (isTerminalRunStatus(o.status)) stop();
        })
        .catch((e: unknown) => {
          if (active && writeGateRef.current?.isCurrent(requestRevision)) {
            setError(e instanceof Error ? e.message : String(e));
          }
        });
    };
    timerRef.current = window.setInterval(poll, POLL_MS);
    poll();
    return () => {
      active = false;
      writeGateRef.current?.supersedePendingRequests();
      stop();
    };
  }, [sop, runId, stop]);

  return { overlay, error, setOverlay };
}
