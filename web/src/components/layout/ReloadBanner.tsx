import { useEffect, useState } from 'react';
import { useLocation } from 'react-router-dom';
import { AlertTriangle } from 'lucide-react';
import { getDrift, getReloadStatus, type DriftEntry } from '@/lib/api';
import ReloadDaemonButton from '@/components/onboard/ReloadDaemonButton';

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
  const location = useLocation();

  useEffect(() => {
    let cancelled = false;

    async function pollOnce() {
      try {
        const [{ pending_reload }, { drifted }] = await Promise.all([
          getReloadStatus(),
          getDrift(),
        ]);
        if (!cancelled) {
          setState({ pendingReload: pending_reload, drifted });
        }
      } catch {
        // Network blip or auth lapse: keep the prior state.
      }
    }

    pollOnce();
    const interval = setInterval(pollOnce, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [pollKey]);

  if (!state || (!state.pendingReload && state.drifted.length === 0)) {
    return null;
  }

  const { pendingReload, drifted } = state;
  const driftedCount = drifted.length;
  const isOnboarding = location.pathname.startsWith('/onboard');
  if (isOnboarding && pendingReload && driftedCount === 0) {
    return (
      <div
        className="px-4 py-3 border-b flex items-start gap-3"
        style={{
          background: 'rgba(14, 165, 233, 0.06)',
          borderColor: 'rgba(14, 165, 233, 0.2)',
        }}
      >
        <AlertTriangle
          className="h-4 w-4 flex-shrink-0 mt-0.5"
          style={{ color: 'var(--pc-accent)' }}
        />
        <p
          className="text-sm font-medium"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          Changes saved. Continue onboarding.
        </p>
      </div>
    );
  }

  return (
    <div
      className="px-4 py-3 border-b flex items-center gap-3"
      style={{
        background: 'rgba(245, 180, 0, 0.06)',
        borderColor: 'rgba(245, 180, 0, 0.25)',
      }}
    >
      <AlertTriangle
        className="h-4 w-4 flex-shrink-0 mt-0.5"
        style={{ color: 'var(--color-status-warning, #f5b400)' }}
      />
      <div className="flex-1 min-w-0">
        <p
          className="text-sm font-medium"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          {pendingReload && driftedCount > 0
            ? 'Config changed this session and on-disk drift detected'
            : pendingReload
              ? 'Config changed — reload daemon to apply'
              : `${driftedCount} path${driftedCount === 1 ? '' : 's'} differ from on-disk`}
        </p>
        {driftedCount > 0 && (
          <ul
            className="text-xs mt-1 flex flex-col gap-0.5"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            {drifted.slice(0, 4).map((d) => (
              <li key={d.path} className="font-mono break-all">
                {d.path}
                {d.secret && (
                  <span style={{ color: 'var(--pc-text-faint)' }}>
                    {' '}
                    (secret)
                  </span>
                )}
              </li>
            ))}
            {driftedCount > 4 && (
              <li style={{ color: 'var(--pc-text-faint)' }}>
                …and {driftedCount - 4} more
              </li>
            )}
          </ul>
        )}
      </div>
      <ReloadDaemonButton onReloaded={() => setPollKey((k) => k + 1)} />
    </div>
  );
}
