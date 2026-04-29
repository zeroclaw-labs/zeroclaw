// "Reload daemon" button (#6175). Tears down + re-instantiates every daemon
// subsystem in place — same PID. Used when config changes need the daemon
// to re-consume them (channels listener rebind, MCP server respawn, etc.).
//
// UX:
//  1. Click — modal opens explaining what reload does.
//  2. Confirm — POST /admin/reload (raises SIGUSR1 inside the daemon).
//  3. Poll /health every 500ms with timeout 30s. Briefly the daemon is
//     unreachable (gateway listener drops + rebinds); button shows
//     "Reloading..." then "Waiting for daemon..." then "Daemon back ✓".
//  4. After /health responds, the parent's `onReloaded` runs (typically
//     reloads page state).
//
// The modal copy is explicit because `Reload` can mean many things
// elsewhere in software — clarify that nothing is destroyed, the PID
// stays, and connections will briefly drop.

import { useState } from 'react';
import { Loader2, RotateCw, X } from 'lucide-react';
import { ApiError, reloadDaemonAndWait } from '../../lib/api';

interface ReloadDaemonButtonProps {
  /** Called when /health answers post-reload (parent typically reloads its data). */
  onReloaded?: () => void;
  /** Override the default 30s health-poll timeout. */
  timeoutMs?: number;
}

type State =
  | { kind: 'idle' }
  | { kind: 'confirming' }
  | { kind: 'waiting'; since: number }  // POSTing /admin/reload + polling /health
  | { kind: 'back' }            // /health answered after reload
  | { kind: 'error'; message: string };

export default function ReloadDaemonButton({ onReloaded, timeoutMs = 30_000 }: ReloadDaemonButtonProps) {
  const [state, setState] = useState<State>({ kind: 'idle' });

  const triggerReload = async () => {
    setState({ kind: 'waiting', since: Date.now() });
    try {
      await reloadDaemonAndWait(timeoutMs);
      setState({ kind: 'back' });
      // Hold the success state briefly so the user sees the green
      // confirmation, then return to idle and refresh parent data.
      setTimeout(() => {
        setState({ kind: 'idle' });
        onReloaded?.();
      }, 1500);
    } catch (e) {
      const msg =
        e instanceof ApiError
          ? `[${e.envelope.code}] ${e.envelope.message}`
          : e instanceof Error
            ? e.message
            : String(e);
      setState({
        kind: 'error',
        message: `Reload failed: ${msg}. Check the gateway logs (it may still be starting, or it may have crashed).`,
      });
    }
  };

  const isBusy = state.kind === 'waiting' || state.kind === 'back';

  return (
    <>
      <button
        type="button"
        onClick={() => setState({ kind: 'confirming' })}
        disabled={isBusy}
        className="btn-secondary flex items-center gap-2 text-sm px-3 py-2"
        title="Re-read config and re-init every daemon subsystem in place"
      >
        {state.kind === 'waiting' ? (
          <Loader2 className="h-4 w-4 animate-spin" />
        ) : (
          <RotateCw className="h-4 w-4" />
        )}
        {state.kind === 'waiting'
          ? 'Waiting for daemon…'
          : state.kind === 'back'
            ? 'Daemon back ✓'
            : 'Reload daemon'}
      </button>

      {state.kind === 'error' && (
        <div
          className="rounded-xl border p-3 text-sm mt-2"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {state.message}
          <button
            type="button"
            onClick={() => setState({ kind: 'idle' })}
            className="ml-3 underline"
          >
            dismiss
          </button>
        </div>
      )}

      {state.kind === 'confirming' && (
        <div className="fixed inset-0 modal-backdrop flex items-center justify-center z-50">
          <div className="surface-panel p-6 w-full max-w-md mx-4 animate-fade-in-scale">
            <div className="flex items-center justify-between mb-4">
              <h3
                className="text-lg font-semibold flex items-center gap-2"
                style={{ color: 'var(--pc-text-primary)' }}
              >
                <RotateCw className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
                Reload daemon?
              </h3>
              <button
                type="button"
                onClick={() => setState({ kind: 'idle' })}
                className="btn-icon"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            <div className="space-y-3 text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
              <p>
                The daemon process stays running (same PID), but every
                subsystem tears down and re-initializes from the on-disk
                config:
              </p>
              <ul className="list-disc pl-5 space-y-1">
                <li>Gateway listener stops and rebinds (clients briefly see connection-refused).</li>
                <li>Channel listeners (Matrix, Slack, etc.) abort and respawn.</li>
                <li>MCP servers, scheduler, heartbeat re-init.</li>
                <li>Provider clients pick up new API keys / model defaults.</li>
              </ul>
              <p style={{ color: 'var(--pc-text-muted)' }}>
                Use this after editing config from the dashboard. For everyday
                in-flight changes (toggle, comment, etc.) save is enough — reload
                is for changes that need a fresh subsystem (added a channel,
                changed gateway port, swapped memory backend).
              </p>
              <p>
                In-flight HTTP requests will fail. The dashboard auto-reconnects
                when the daemon is back.
              </p>
            </div>

            <div className="flex justify-end gap-3 mt-6">
              <button
                type="button"
                onClick={() => setState({ kind: 'idle' })}
                className="btn-secondary px-4 py-2 text-sm font-medium"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void triggerReload()}
                className="btn-electric px-4 py-2 text-sm font-medium"
              >
                Reload
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
