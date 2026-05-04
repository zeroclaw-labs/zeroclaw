import { useCallback, useEffect, useRef, useState } from 'react';
import { Download, RefreshCw, AlertTriangle, CheckCircle2 } from 'lucide-react';
import { postSystemUpdate, type UpdateLogLine, type UpdatePhase } from '@/lib/api';
import { SSEClient } from '@/lib/sse';
import { apiOrigin, basePath } from '@/lib/basePath';
import { useSystemVersion } from '@/hooks/useSystemVersion';

const PHASE_LABEL: Record<UpdatePhase, string> = {
  preflight: 'Checking…',
  download: 'Downloading…',
  backup: 'Backing up…',
  validate: 'Validating…',
  swap: 'Installing…',
  smoke_test: 'Verifying…',
  done: 'Done',
  failed: 'Failed',
  rolled_back: 'Rolled back',
};

const TERMINAL: ReadonlySet<UpdatePhase> = new Set([
  'done',
  'failed',
  'rolled_back',
]);

type Banner =
  | { kind: 'idle' }
  | { kind: 'success' }
  | { kind: 'error'; message: string }
  | { kind: 'rolled_back' };

interface Props {
  collapsed: boolean;
}

/**
 * Compact self-update widget for the sidebar footer.
 *
 * - Up to date: tiny `v0.7.4 · Up to date` text. No button.
 * - Update available: prominent "Update to vX.Y.Z" button.
 * - Running: spinner + current phase label, button disabled.
 * - Collapsed sidebar: single icon (download / spinner / check) with tooltip.
 */
export function SidebarUpdate({ collapsed }: Props): React.ReactElement | null {
  const { version, error, refetch } = useSystemVersion();
  const [running, setRunning] = useState(false);
  const [phase, setPhase] = useState<UpdatePhase | null>(null);
  const [banner, setBanner] = useState<Banner>({ kind: 'idle' });
  const sseRef = useRef<SSEClient | null>(null);

  const stopStream = useCallback(() => {
    if (sseRef.current) {
      sseRef.current.disconnect?.();
      sseRef.current = null;
    }
  }, []);

  useEffect(() => () => stopStream(), [stopStream]);

  const startUpdate = useCallback(async () => {
    setBanner({ kind: 'idle' });
    setPhase(null);
    setRunning(true);

    try {
      await postSystemUpdate({});
    } catch (e) {
      setRunning(false);
      const msg = e instanceof Error ? e.message : 'Failed to start update.';
      setBanner({
        kind: 'error',
        message:
          msg.toLowerCase().includes('conflict') || msg.includes('409')
            ? 'Update already running'
            : msg,
      });
      return;
    }

    const client = new SSEClient({
      path: `${apiOrigin}${basePath}/api/system/update/stream`,
      autoReconnect: false,
    });
    sseRef.current = client;

    client.onEvent = (raw) => {
      const event = raw as unknown as UpdateLogLine;
      if (!event.phase) return;
      setPhase(event.phase);
      if (TERMINAL.has(event.phase)) {
        setRunning(false);
        stopStream();
        if (event.phase === 'done') {
          setBanner({ kind: 'success' });
          refetch();
        } else if (event.phase === 'rolled_back') {
          setBanner({ kind: 'rolled_back' });
        } else {
          setBanner({ kind: 'error', message: event.message ?? 'Update failed' });
        }
      }
    };

    client.onError = () => {
      setRunning(false);
      stopStream();
      setBanner({ kind: 'error', message: 'Lost progress stream' });
    };

    client.connect();
  }, [refetch, stopStream]);

  if (error || !version) {
    return null;
  }

  const upToDate = !version.update_available;

  // ── Collapsed: single icon + tooltip ───────────────────────────

  if (collapsed) {
    const tip = running
      ? phase
        ? PHASE_LABEL[phase]
        : 'Updating…'
      : upToDate
        ? `v${version.current} — up to date`
        : `Update available: v${version.latest}`;

    const Icon = running ? RefreshCw : upToDate ? CheckCircle2 : Download;
    const color = running
      ? 'var(--pc-accent)'
      : upToDate
        ? 'var(--pc-text-faint)'
        : 'var(--pc-accent-light)';

    return (
      <button
        type="button"
        onClick={upToDate || running ? undefined : startUpdate}
        disabled={upToDate || running}
        className="group relative w-10 h-10 mx-auto flex items-center justify-center rounded-xl transition-all hover:bg-(--pc-hover)"
        style={{ color }}
        aria-label={tip}
      >
        <Icon className={`h-5 w-5 ${running ? 'animate-spin' : ''}`} />
        <span
          className="absolute left-full ml-2 px-2 py-1 rounded-md text-xs whitespace-nowrap opacity-0 group-hover:opacity-100 transition-opacity pointer-events-none z-9999"
          style={{
            background: 'var(--pc-bg-elevated)',
            color: 'var(--pc-text-primary)',
            border: '1px solid var(--pc-border)',
          }}
        >
          {tip}
        </span>
      </button>
    );
  }

  // ── Expanded: version line + button when relevant ──────────────

  return (
    <div className="px-3 py-2 space-y-1.5">
      {/* Version line */}
      <div
        className="text-[11px] flex items-center gap-1.5"
        style={{ color: 'var(--pc-text-muted)' }}
      >
        {running ? (
          <RefreshCw
            className="h-3 w-3 animate-spin"
            style={{ color: 'var(--pc-accent)' }}
          />
        ) : upToDate ? (
          <CheckCircle2
            className="h-3 w-3"
            style={{ color: '#34d399' }}
          />
        ) : (
          <Download
            className="h-3 w-3"
            style={{ color: 'var(--pc-accent-light)' }}
          />
        )}
        <span className="font-mono">v{version.current}</span>
        <span style={{ color: 'var(--pc-text-faint)' }}>·</span>
        <span>
          {running
            ? phase
              ? PHASE_LABEL[phase]
              : 'Starting…'
            : upToDate
              ? 'Up to date'
              : `→ v${version.latest}`}
        </span>
      </div>

      {/* Action button — only when update available or post-action banner */}
      {!upToDate && !running && banner.kind === 'idle' && (
        <button
          type="button"
          onClick={startUpdate}
          className="w-full inline-flex items-center justify-center gap-1.5 rounded-xl px-3 py-1.5 text-xs font-medium border transition-all"
          style={{
            background: 'rgba(var(--pc-accent-rgb), 0.12)',
            borderColor: 'rgba(var(--pc-accent-rgb), 0.4)',
            color: 'var(--pc-accent-light)',
          }}
        >
          <Download className="h-3.5 w-3.5" />
          Update to v{version.latest}
        </button>
      )}

      {banner.kind === 'success' && (
        <div
          className="flex items-center gap-1.5 text-[11px]"
          style={{ color: '#34d399' }}
        >
          <CheckCircle2 className="h-3 w-3" />
          Updated — restart to run new binary
        </div>
      )}

      {banner.kind === 'rolled_back' && (
        <div
          className="flex items-center gap-1.5 text-[11px]"
          style={{ color: 'var(--color-status-warning)' }}
        >
          <AlertTriangle className="h-3 w-3" />
          Rolled back to previous binary
        </div>
      )}

      {banner.kind === 'error' && (
        <div
          className="flex items-center gap-1.5 text-[11px]"
          style={{ color: 'var(--color-status-error)' }}
        >
          <AlertTriangle className="h-3 w-3" />
          <span className="truncate">{banner.message}</span>
        </div>
      )}
    </div>
  );
}
