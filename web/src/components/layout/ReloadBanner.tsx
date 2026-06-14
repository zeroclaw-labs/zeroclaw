import { useState } from 'react';
import { useLocation } from 'react-router-dom';
import { AlertTriangle, X } from 'lucide-react';
import { getDrift, getReloadStatus, type DriftEntry } from '@/lib/api';
import ReloadDaemonButton from '@/components/sections/ReloadDaemonButton';
import { useReloadAvailable } from '@/lib/reloadAvailability';
import { usePolling } from '@/hooks/usePolling';

const POLL_INTERVAL_MS = 5_000;

interface BannerState {
  pendingReload: boolean;
  drifted: DriftEntry[];
}

/**
 * Layout-level banner. Polls the gateway for two distinct reload triggers:
 *
 * - `pending_reload`: config writes have landed in this session, subsystems
 *   may need a reload to apply (channels rebind, providers swap keys, etc.).
 * - `drifted`: on-disk config diverges from the running daemon's loaded
 *   state, typically because an external editor touched the file.
 *
 * Hidden when both signals are clear. Shows the same `ReloadDaemonButton`
 * the Config page already uses — when reload completes, both signals clear
 * (the server-side flag resets and the daemon re-reads disk).
 */
export default function ReloadBanner() {
  const [state, setState] = useState<BannerState | null>(null);
  const [pollKey, setPollKey] = useState(0);
  // Signature of the banner content the user last dismissed. The banner
  // re-appears when the underlying signal changes (new drift paths, or
  // pending flips back on) because the recomputed signature won't match.
  const [dismissedSig, setDismissedSig] = useState<string | null>(null);
  const location = useLocation();
  // Whether the in-UI reload action can actually succeed from this origin.
  // When false (remote host, pairing off) we drop the dead button and reword
  // the notice to point the operator at the CLI / a loopback session.
  const reloadAvailable = useReloadAvailable();

  // Poll only while the tab is visible (no background churn); re-arm on reload.
  usePolling(
    async (isStale) => {
      try {
        const [{ pending_reload }, { drifted }] = await Promise.all([
          getReloadStatus(),
          getDrift(),
        ]);
        if (!isStale()) {
          setState({ pendingReload: pending_reload, drifted });
        }
      } catch {
        // Network blip or auth lapse: keep the prior state.
      }
    },
    POLL_INTERVAL_MS,
    [pollKey],
  );

  if (!state || (!state.pendingReload && state.drifted.length === 0)) {
    return null;
  }

  const { pendingReload, drifted } = state;
  const driftedCount = drifted.length;
  const isQuickstart = location.pathname.startsWith('/quickstart');
  if (isQuickstart && pendingReload && driftedCount === 0) {
    return (
      <div className="px-4 py-3 border-b border-status-info/20 bg-status-info/[0.06] flex items-start gap-3">
        <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5 text-status-info" />
        <p className="text-sm font-medium text-pc-text">
          Changes saved. Continue setup.
        </p>
      </div>
    );
  }

  // Content signature for the warning banner. Dismissal is keyed to this so
  // a fresh change (different pending/drift state) surfaces the banner again.
  const sig = `${pendingReload ? 1 : 0}|${drifted
    .map((d) => d.path)
    .sort()
    .join(',')}`;
  if (dismissedSig === sig) {
    return null;
  }

  return (
    <div className="px-4 py-3 border-b border-status-warning/25 bg-status-warning/[0.06] flex items-center gap-3">
      <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5 text-status-warning" />
      <div className="flex-1 min-w-0">
        <p className="text-sm font-medium text-pc-text">
          {pendingReload && driftedCount > 0
            ? 'Config changed this session and on-disk drift detected'
            : pendingReload
              ? 'Config changed — reload daemon to apply'
              : `${driftedCount} path${driftedCount === 1 ? '' : 's'} differ from on-disk`}
        </p>
        {driftedCount > 0 && (
          <ul className="text-xs mt-1 flex flex-col gap-0.5 text-pc-text-muted">
            {drifted.slice(0, 4).map((d) => (
              <li key={d.path} className="font-mono break-all">
                {d.path}
                {d.secret && (
                  <span className="text-pc-text-faint">
                    {' '}
                    (secret)
                  </span>
                )}
              </li>
            ))}
            {driftedCount > 4 && (
              <li className="text-pc-text-faint">
                …and {driftedCount - 4} more
              </li>
            )}
          </ul>
        )}
        {!reloadAvailable && (
          <p className="text-xs mt-1 text-pc-text-muted">
            Reloading isn’t available from this remote session. Apply it with{' '}
            <code className="font-mono">zeroclaw reload</code> on the host, or
            from a loopback (localhost) session.
          </p>
        )}
      </div>
      {reloadAvailable && (
        <ReloadDaemonButton onReloaded={() => setPollKey((k) => k + 1)} />
      )}
      <button
        type="button"
        onClick={() => setDismissedSig(sig)}
        aria-label="Dismiss"
        title="Dismiss"
        className="flex-shrink-0 p-1 rounded-[var(--radius-sm)] text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
