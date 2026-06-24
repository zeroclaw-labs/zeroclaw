import { useEffect, useRef, type DependencyList } from "react";

/**
 * Poll `callback` on an interval, but only while the browser tab is visible.
 *
 * - Fires once immediately on mount / re-arm (a no-op if the tab starts hidden).
 * - Skips interval ticks while `document.hidden` — no background network churn
 *   on a tab the user isn't looking at (the previous always-on 5s polling kept
 *   hammering the gateway forever, even when backgrounded).
 * - Fires an immediate catch-up poll the moment the tab becomes visible again,
 *   so returning to the tab shows fresh data without waiting a full interval.
 * - Clears the interval + listener on unmount or when `enabled` flips false.
 *
 * The callback receives `isStale()` — true once this effect has been cleaned up
 * (unmounted or re-armed via `deps`). Guard any `setState` with it so a slow
 * in-flight request can't write stale data after the inputs changed (replaces
 * the hand-rolled `cancelled` flags the call sites used to carry).
 *
 * `deps` re-arms the loop like a normal effect dependency array.
 */
export function usePolling(
  callback: (isStale: () => boolean) => void | Promise<void>,
  intervalMs: number,
  deps: DependencyList = [],
  enabled = true,
): void {
  const cbRef = useRef(callback);
  cbRef.current = callback;

  useEffect(() => {
    if (!enabled) return;
    let stale = false;
    const isStale = () => stale;
    const run = () => {
      if (!document.hidden) void cbRef.current(isStale);
    };
    run(); // immediate poll (no-op when hidden)
    const id = window.setInterval(run, intervalMs);
    const onVisibility = () => {
      if (!document.hidden) void cbRef.current(isStale);
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      stale = true;
      window.clearInterval(id);
      document.removeEventListener("visibilitychange", onVisibility);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intervalMs, enabled, ...deps]);
}
