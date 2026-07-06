// Reload-availability gate (mirrors the gateway's `/admin/reload` admission).
//
// The gateway allows `POST /admin/reload` when the caller is on loopback, OR
// (for remote callers) only when both `allow_remote_admin` and pairing are
// enabled and the request is authed. The web client can't see
// `allow_remote_admin`, so the strongest correct proxy it can compute is:
//
//   reloadAvailable = isLoopbackHost() || requirePairing
//
// - `isLoopbackHost()` is synchronous (derived from window.location.hostname).
// - `requirePairing` comes from `GET /health` (`require_pairing` field). We
//   fetch it once per session and cache the result at module scope so every
//   consumer shares a single request.
// - If the health fetch fails, we default `requirePairing = true` — fail OPEN.
//   Better to show the button (and let the gateway reject it) than to wrongly
//   hide a reload the operator legitimately can perform.
// - While the value is still loading, we treat reload as available so the
//   button never flash-hides on first paint.

import { useEffect, useState } from 'react';
import { getPublicHealth } from './api';

const LOOPBACK_HOSTNAMES = new Set([
  'localhost',
  '127.0.0.1',
  '::1',
  '[::1]',
]);

/**
 * True when the page is served from a loopback origin. The gateway always
 * permits reload from loopback, regardless of pairing or remote-admin config.
 */
export function isLoopbackHost(): boolean {
  const host = window.location.hostname;
  if (LOOPBACK_HOSTNAMES.has(host)) return true;
  // Covers `<name>.localhost` (a reserved loopback TLD).
  return host.endsWith('.localhost');
}

// Module-level cache so multiple hook consumers share a single /health fetch
// for the lifetime of the session. `null` = not yet started.
let requirePairingPromise: Promise<boolean> | null = null;

function fetchRequirePairing(): Promise<boolean> {
  if (!requirePairingPromise) {
    requirePairingPromise = getPublicHealth()
      .then((health) => health.require_pairing)
      // Fail OPEN: if /health is unreachable, assume pairing is required so
      // the reload affordance stays visible rather than wrongly hidden.
      .catch(() => true);
  }
  return requirePairingPromise;
}

/**
 * Whether the in-UI "Reload daemon" action is expected to succeed.
 *
 * Returns the loopback short-circuit synchronously; for remote hosts it
 * reflects the cached `require_pairing` health value once it resolves. While
 * that fetch is in flight (remote host only) it returns `true` so the action
 * does not flash-hide before the gateway capability is known.
 */
export function useReloadAvailable(): boolean {
  const loopback = isLoopbackHost();

  // `undefined` until the /health fetch resolves. Only consulted for remote
  // hosts; loopback short-circuits to available without ever reading it.
  const [requirePairing, setRequirePairing] = useState<boolean | undefined>(
    undefined,
  );

  useEffect(() => {
    if (loopback) return; // loopback never needs the health value
    let cancelled = false;
    void fetchRequirePairing().then((value) => {
      if (!cancelled) setRequirePairing(value);
    });
    return () => {
      cancelled = true;
    };
  }, [loopback]);

  if (loopback) return true;
  // Still loading the health value: treat as available (don't flash-hide).
  if (requirePairing === undefined) return true;
  return requirePairing;
}
