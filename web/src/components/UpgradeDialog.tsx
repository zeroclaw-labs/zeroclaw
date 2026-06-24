import { useEffect, useId, useRef, useState } from 'react';
import { Check, Loader2, RefreshCw } from 'lucide-react';
import { Button } from '@/components/ui/Button';
import { t } from '@/lib/i18n';
import {
  startUpgrade,
  getUpgradeStatus,
  getStatus,
  type VersionCheckResponse,
  type UpgradeStatusResponse,
} from '@/lib/api';

export interface UpgradeDialogProps {
  /** Whether the dialog is mounted/visible. */
  open: boolean;
  /** Latest version-check result, or null while the first check is in flight. */
  info: VersionCheckResponse | null;
  /** Whether a version check is currently running. */
  loading: boolean;
  /** `gateway.allow_self_upgrade` — gates the Upgrade button. */
  allowSelfUpgrade: boolean;
  /** How a restart is achieved here; `supervised` and `self_respawn` can
   *  auto-restart, `manual` cannot. */
  restartMode?: 'supervised' | 'self_respawn' | 'manual';
  /** Manual-restart command to show after a swap. */
  restartHint?: string;
  /** Trigger a forced re-check against the upstream release feed, bypassing
   *  the server-side 1h cache. The Info view exposes this as a refresh icon
   *  next to the title so users can poke for an update on demand instead of
   *  waiting for the next 6h tick. */
  onRefetch?: () => void;
  /** Close the dialog (Esc, backdrop, or Close button). */
  onClose: () => void;
}

type View = 'info' | 'confirm' | 'progress' | 'restarting' | 'done' | 'failed';

const PHASE_LABELS = [
  'upgrade.phase.preflight',
  'upgrade.phase.download',
  'upgrade.phase.backup',
  'upgrade.phase.verify',
  'upgrade.phase.swap',
  'upgrade.phase.cleanup',
];

const UPGRADE_POLL_MS = 800;
const RESTART_POLL_MS = 2000;
const RESTART_TIMEOUT_MS = 60_000;
/** Delay between detecting the new version is live and reloading the SPA, so
 *  the user sees the ✓ for a beat instead of the page yanking out from under
 *  them. Short enough that nobody waits, long enough to register. */
const RELOAD_AFTER_RECONCILE_MS = 800;

/**
 * Upgrade dialog covering the read-only info view (Phase 1), applying an upgrade
 * (Phase 2), and supervised exit-to-restart with version reconciliation
 * (Phase 3). The gateway only ever exits to restart; relaunch is the
 * supervisor's job, so the `restarting` view polls `/api/status` (not the
 * upgrade endpoint) until the new version reports in.
 */
export function UpgradeDialog({
  open,
  info,
  loading,
  allowSelfUpgrade,
  restartMode,
  restartHint,
  onRefetch,
  onClose,
}: UpgradeDialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();

  const [view, setView] = useState<View>('info');
  const [handoffId, setHandoffId] = useState<string | null>(null);
  const [status, setStatus] = useState<UpgradeStatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reconciled, setReconciled] = useState(false);
  /** Wall-clock when the `restarting` view was first entered. Used to drive
   *  the elapsed-time hint shown to the user while we wait for the new
   *  process to come back up. */
  const [restartStartedAt, setRestartStartedAt] = useState<number | null>(null);
  /** Ticks once a second while restarting so the elapsed counter re-renders
   *  without us having to thread state through every poll callback. */
  const [, setRestartTick] = useState(0);
  const canAutoRestart =
    restartMode === 'supervised' || restartMode === 'self_respawn';
  const [autoRestart, setAutoRestart] = useState(true);

  // Reset on open, and re-attach to an upgrade that is still running server-side.
  useEffect(() => {
    if (!open) return;
    setError(null);
    setReconciled(false);
    setAutoRestart(canAutoRestart);
    setRestartStartedAt(null);
    getUpgradeStatus()
      .then((s) => {
        if (s.state === 'running') {
          setHandoffId(s.handoff_id ?? null);
          setStatus(s);
          setView('progress');
        } else if (s.state === 'restarting') {
          setHandoffId(s.handoff_id ?? null);
          setStatus(s);
          setView('restarting');
        } else {
          setView('info');
        }
      })
      .catch(() => setView('info'));
  }, [open, canAutoRestart]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        // Don't let Esc dismiss the dialog while we're waiting for the new
        // process — closing here would stop the /api/status reconciliation
        // poll that triggers the auto-reload.
        if (view === 'restarting') return;
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, view, onClose]);

  // Poll the upgrade endpoint while running.
  useEffect(() => {
    if (!open || view !== 'progress') return;
    let active = true;
    const tick = async () => {
      try {
        const s = await getUpgradeStatus(handoffId ?? undefined);
        if (!active) return;
        setStatus(s);
        if (s.state === 'done') setView('done');
        else if (s.state === 'failed') {
          setError(s.error ?? 'upgrade failed');
          setView('failed');
        } else if (s.state === 'restarting') setView('restarting');
      } catch {
        /* keep polling */
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), UPGRADE_POLL_MS);
    return () => {
      active = false;
      window.clearInterval(id);
    };
  }, [open, view, handoffId]);

  // While restarting, the gateway exits and the supervisor relaunches it.
  // Reconcile by polling /api/status until the new version reports in,
  // then auto-reload the SPA so the user picks up the freshly-shipped
  // bundle without having to F5 by hand.
  useEffect(() => {
    if (!open || view !== 'restarting') return;
    // Stamp the start of the wait the first time we enter this view, and
    // tick a 1Hz counter so the elapsed-time hint re-renders smoothly.
    if (restartStartedAt == null) setRestartStartedAt(Date.now());
    const tickId = window.setInterval(() => setRestartTick((n) => n + 1), 1000);

    let active = true;
    const deadline = Date.now() + RESTART_TIMEOUT_MS;
    const target = status?.target_version ?? info?.latest_version ?? null;
    const previous = status?.previous_version ?? info?.current_version ?? null;
    const tick = async () => {
      try {
        const s = await getStatus();
        if (!active) return;
        const v = s.version ?? null;
        if (v && v !== previous && (!target || v === target)) {
          setReconciled(true);
          setView('done');
          // The gateway is back on the new version — the new web bundle is
          // now being served. Reload after a short pause so the user sees
          // the success state register before the page swaps out.
          window.setTimeout(() => {
            if (active) window.location.reload();
          }, RELOAD_AFTER_RECONCILE_MS);
          return;
        }
      } catch {
        /* gateway is down mid-restart — keep polling */
      }
      if (Date.now() > deadline && active) {
        // Came back on the old version, or never came back in time.
        setReconciled(false);
        setView('done');
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), RESTART_POLL_MS);
    return () => {
      active = false;
      window.clearInterval(id);
      window.clearInterval(tickId);
    };
  }, [open, view, status, info, restartStartedAt]);

  useEffect(() => {
    if (!open) return;
    const buttons = panelRef.current?.querySelectorAll<HTMLButtonElement>('button');
    buttons?.[buttons.length - 1]?.focus();
  }, [open, view]);

  if (!open) return null;

  const isNewer = info?.is_newer === true;
  const hasError = info?.error != null && info.error !== '';
  const published =
    info?.published_at != null ? new Date(info.published_at).toLocaleDateString() : null;

  const beginUpgrade = async () => {
    setError(null);
    try {
      const res = await startUpgrade({
        version: info?.latest_version ?? null,
        auto_restart: autoRestart && canAutoRestart,
      });
      setHandoffId(res.handoff_id);
      setStatus(null);
      setView('progress');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setView('failed');
    }
  };

  // Backdrop dismissal is suppressed while restarting — see the Esc handler
  // above for the same reason. The browser still has the dialog tree.
  const handleBackdropClick = view === 'restarting' ? undefined : onClose;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={handleBackdropClick}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-md mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="px-6 pt-5 pb-4 flex flex-col gap-3">
          <div className="flex items-center justify-between gap-2">
            <h2 id={titleId} className="text-sm font-semibold text-pc-text">
              {t('upgrade.title')}
            </h2>
            {/* Manual re-check: bypass the server-side 1h cache. Only meaningful
                in the read-only Info view — once an upgrade is in flight the
                version_check result is frozen anyway. The icon mirrors the
                `loading` state by spinning while a check is in flight. */}
            {(view === 'info' || view === 'confirm') && onRefetch && (
              <button
                type="button"
                onClick={onRefetch}
                disabled={loading}
                title={t('upgrade.recheck')}
                aria-label={t('upgrade.recheck')}
                className="inline-flex h-6 w-6 items-center justify-center rounded-[var(--radius-sm)] text-pc-text-muted hover:text-pc-text hover:bg-pc-surface transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                <RefreshCw className={`h-3.5 w-3.5 ${loading ? 'animate-spin' : ''}`} />
              </button>
            )}
          </div>

          {/* ── Version summary (always shown except terminal states) ── */}
          {(view === 'info' || view === 'confirm') &&
            (loading && info == null ? (
              <div className="text-xs text-pc-text-muted">{t('upgrade.checking')}</div>
            ) : hasError ? (
              <div className="text-xs text-pc-text-muted">
                <div className="text-pc-text">{t('upgrade.check_failed')}</div>
                <div className="mt-1 font-mono break-words">{info?.error}</div>
              </div>
            ) : (
              <>
                <dl className="text-xs grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
                  <dt className="text-pc-text-muted">{t('upgrade.current')}</dt>
                  <dd className="text-pc-text font-mono">{info?.current_version}</dd>
                  <dt className="text-pc-text-muted">{t('upgrade.latest')}</dt>
                  <dd className="text-pc-text font-mono">
                    {info?.latest_version ?? '—'}
                    {published && <span className="text-pc-text-muted"> · {published}</span>}
                  </dd>
                </dl>
                {!isNewer && (
                  <div className="text-xs text-pc-text-muted">{t('upgrade.up_to_date')}</div>
                )}
                {isNewer && info?.release_notes && view === 'info' && (
                  <div className="flex flex-col gap-1">
                    <div className="text-xs font-medium text-pc-text">{t('upgrade.notes')}</div>
                    <div className="max-h-48 overflow-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface px-3 py-2 text-xs leading-relaxed text-pc-text-muted whitespace-pre-wrap">
                      {info.release_notes}
                    </div>
                  </div>
                )}
                {isNewer && !allowSelfUpgrade && (
                  <div className="text-xs text-pc-text-muted">{t('upgrade.disabled')}</div>
                )}
                {isNewer && allowSelfUpgrade && canAutoRestart && view === 'info' && (
                  <div className="flex flex-col gap-1">
                    <label className="flex items-center gap-2 text-xs text-pc-text-muted">
                      <input
                        type="checkbox"
                        checked={autoRestart}
                        onChange={(e) => setAutoRestart(e.target.checked)}
                      />
                      {t('upgrade.auto_restart')}
                    </label>
                    {autoRestart && restartMode === 'self_respawn' && (
                      <div className="text-[11px] text-pc-text-muted pl-6">
                        {t('upgrade.self_respawn_note')}
                      </div>
                    )}
                  </div>
                )}
                {isNewer && allowSelfUpgrade && !canAutoRestart && view === 'info' && (
                  <div className="text-xs text-pc-text-muted">
                    {t('upgrade.manual_note')}
                    {restartHint && (
                      <code className="ml-1 font-mono text-pc-text">{restartHint}</code>
                    )}
                  </div>
                )}
              </>
            ))}

          {/* ── Confirm ── */}
          {view === 'confirm' && (
            <div className="text-xs text-pc-text-muted rounded-[var(--radius-md)] border border-pc-border bg-pc-surface px-3 py-2">
              {t('upgrade.confirm_body')}
            </div>
          )}

          {/* ── Progress (active upgrade) ── */}
          {view === 'progress' && (() => {
            const cur = status?.phase ?? 0;
            const pct = Math.round((Math.min(cur, 6) / 6) * 100);
            const lastLine = status?.log_tail?.[status.log_tail.length - 1];
            return (
              <div className="flex flex-col gap-2.5">
                <div className="text-xs text-pc-text flex items-center gap-2">
                  <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin" />
                  {t('upgrade.upgrading')}
                </div>

                {/* Progress bar */}
                <div className="h-1 w-full overflow-hidden rounded-full bg-pc-surface">
                  <div
                    className="h-full rounded-full transition-all duration-500 ease-out"
                    style={{ width: `${pct}%`, background: 'var(--pc-accent)' }}
                  />
                </div>

                {/* Live last-line ticker */}
                {lastLine && (
                  <div className="font-mono text-[11px] text-pc-text-muted truncate" title={lastLine}>
                    {lastLine}
                  </div>
                )}

                {/* Phase checklist — every unfinished step shows a Loader2
                    spinner; the active one is full-strength, the pending ones
                    are faded down so the eye is drawn to where work is
                    actually happening without the list looking dead. */}
                <ol className="text-xs flex flex-col gap-1.5">
                  {PHASE_LABELS.map((key, i) => {
                    const n = i + 1;
                    const done = cur > n;
                    const active = cur === n;
                    return (
                      <li key={key} className="flex items-center gap-2">
                        {done ? (
                          <Check
                            className="h-3.5 w-3.5 shrink-0"
                            style={{ color: 'var(--pc-accent)' }}
                          />
                        ) : active ? (
                          <Loader2 className="h-3.5 w-3.5 shrink-0 animate-spin text-pc-text" />
                        ) : (
                          <Loader2
                            className="h-3.5 w-3.5 shrink-0 animate-spin text-pc-text-muted opacity-30"
                            aria-hidden="true"
                          />
                        )}
                        <span className={done || active ? 'text-pc-text' : 'text-pc-text-muted'}>
                          {t(key)}
                        </span>
                      </li>
                    );
                  })}
                </ol>

                {/* Full log (collapsed) */}
                {status?.log_tail && status.log_tail.length > 0 && (
                  <details className="text-xs">
                    <summary className="cursor-pointer text-pc-text-muted">
                      {t('upgrade.log')}
                    </summary>
                    <pre className="mt-1 max-h-40 overflow-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface px-3 py-2 text-[11px] leading-relaxed text-pc-text-muted whitespace-pre-wrap">
                      {status.log_tail.join('\n')}
                    </pre>
                  </details>
                )}
              </div>
            );
          })()}

          {/* ── Restarting (waiting for the new process to come back) ──
              Dedicated waiting state: large centred spinner, elapsed counter
              so the user can tell the wait is alive, and an explicit hint
              that the page will reload itself the moment the new version
              reports in via /api/status. */}
          {view === 'restarting' && (() => {
            const target = status?.target_version ?? info?.latest_version ?? null;
            const elapsedSec = restartStartedAt != null
              ? Math.max(0, Math.floor((Date.now() - restartStartedAt) / 1000))
              : 0;
            const deadlineSec = Math.floor(RESTART_TIMEOUT_MS / 1000);
            return (
              <div className="flex flex-col items-center gap-3 py-6">
                <Loader2
                  className="h-10 w-10 animate-spin"
                  style={{ color: 'var(--pc-accent)' }}
                />
                <div className="text-sm font-medium text-pc-text text-center">
                  {t('upgrade.restarting')}
                </div>
                <div className="text-xs text-pc-text-muted text-center max-w-[20rem]">
                  {t('upgrade.restart_waiting')}
                  {target && (
                    <>
                      {' '}
                      <span className="font-mono text-pc-text">v{target}</span>
                    </>
                  )}
                </div>
                {/* Indeterminate progress bar — the wait isn't a percentage,
                    so a sweeping bar is more honest than a fake fill. */}
                <div className="h-1 w-full overflow-hidden rounded-full bg-pc-surface">
                  <div
                    className="h-full w-1/3 rounded-full animate-progress-sweep"
                    style={{ background: 'var(--pc-accent)' }}
                  />
                </div>
                <div className="text-[11px] text-pc-text-muted font-mono">
                  {t('upgrade.restart_elapsed')} {elapsedSec}s / {deadlineSec}s
                </div>
              </div>
            );
          })()}

          {/* ── Done ── */}
          {view === 'done' && (
            <div className="text-xs text-pc-text flex flex-col gap-1">
              <div>
                ✓ {t('upgrade.done')}
                {reconciled && info?.latest_version && (
                  <span>
                    {' '}
                    — {t('upgrade.now_running')}{' '}
                    <span className="font-mono">v{info.latest_version}</span>
                  </span>
                )}
              </div>
              {reconciled && (
                <div className="text-pc-text-muted flex items-center gap-1.5">
                  <Loader2 className="h-3 w-3 animate-spin" />
                  {t('upgrade.reloading')}
                </div>
              )}
              {!reconciled && (
                <div className="text-pc-text-muted">
                  {t('upgrade.restart_to_apply')}
                  {restartHint && (
                    <code className="ml-1 font-mono text-pc-text">{restartHint}</code>
                  )}
                </div>
              )}
            </div>
          )}

          {/* ── Failed ── */}
          {view === 'failed' && (
            <div className="text-xs text-pc-text-muted">
              <div className="text-pc-text">✗ {t('upgrade.failed')}</div>
              {error && <div className="mt-1 font-mono break-words">{error}</div>}
            </div>
          )}
        </div>

        {/* ── Footer ── */}
        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          {view === 'info' && info?.release_url && (
            <a
              href={info.release_url}
              target="_blank"
              rel="noopener noreferrer"
              className="mr-auto text-xs text-pc-accent hover:underline"
            >
              {t('upgrade.open_release')}
            </a>
          )}

          {view === 'info' && isNewer && allowSelfUpgrade && (
            <Button variant="primary" onClick={() => setView('confirm')}>
              {t('upgrade.do_upgrade')}
            </Button>
          )}
          {view === 'info' && (
            <Button variant="ghost" onClick={onClose}>
              {t('upgrade.close')}
            </Button>
          )}

          {view === 'confirm' && (
            <>
              <Button variant="ghost" onClick={() => setView('info')}>
                {t('upgrade.cancel')}
              </Button>
              <Button variant="primary" onClick={() => void beginUpgrade()}>
                {t('upgrade.confirm')}
              </Button>
            </>
          )}

          {view === 'progress' && (
            <Button variant="ghost" onClick={onClose}>
              {t('upgrade.close')}
            </Button>
          )}

          {/* During restart we keep the dialog modal — closing it would stop
              the /api/status polling that auto-reloads when the new process
              is live. The user can still dismiss with Esc if they really
              need to, but no inline button invites it. */}

          {(view === 'done' || view === 'failed') && (
            <Button variant="primary" onClick={onClose}>
              {t('upgrade.close')}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

export default UpgradeDialog;
