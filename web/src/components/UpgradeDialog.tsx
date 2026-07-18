import { useCallback, useEffect, useId, useRef, useState } from 'react';
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
  /** `gateway.check_updates` — false means passive polling is off. The dialog
   *  can still surface a manual re-check button (see `onRefetch`), but shows
   *  a dedicated "checks disabled" state instead of a stale/empty version
   *  summary so the operator cannot mistake an unchecked state for
   *  "up to date". */
  checkUpdatesEnabled: boolean;
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
  checkUpdatesEnabled,
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
  /** True once the restart poll times out without detecting a new version.
   *  Stops the spinner and shows a manual-refresh hint in its place. */
  const [pollTimedOut, setPollTimedOut] = useState(false);
  /** Wall-clock when the `restarting` view was first entered. Stored in a ref
   *  rather than state so that the initial stamp doesn't cause the restarting
   *  effect to teardown+rebuild (which would discard the first tick). */
  const restartStartedAtRef = useRef<number | null>(null);
  /** Ticks once a second while restarting so the elapsed counter re-renders
   *  without us having to thread state through every poll callback. The value
   *  itself is not used — only the re-render it triggers matters. */
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const [, setRestartTick] = useState(0);
  const canAutoRestart =
    restartMode === 'supervised' || restartMode === 'self_respawn';
  const [autoRestart, setAutoRestart] = useState(true);
  /** Version that was running when the upgrade started (or when we re-attached
   *  to an in-progress restart). Kept as both a ref (stable closure capture in
   *  the polling effect) and state (drives UI guards that re-render on change). */
  const baselineVersionRef = useRef<string | null>(null);
  const [baselineVersion, setBaselineVersion] = useState<string | null>(null);
  const setBaseline = useCallback((v: string | null) => {
    baselineVersionRef.current = v;
    setBaselineVersion(v);
  }, []);

  // Primitive string so the polling effect dep doesn't react to object identity churn.
  const targetVersion = status?.target_version ?? info?.latest_version ?? null;

  // Reset on open, and re-attach to an upgrade that is still running server-side.
  useEffect(() => {
    if (!open) return;
    setError(null);
    setReconciled(false);
    setPollTimedOut(false);
    setAutoRestart(canAutoRestart);
    restartStartedAtRef.current = null;
    setBaseline(null);
    getUpgradeStatus()
      .then((s) => {
        if (s.state === 'running') {
          setHandoffId(s.handoff_id ?? null);
          setStatus(s);
          setView('progress');
        } else if (s.state === 'restarting') {
          setBaseline(s.previous_version ?? null);
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
        if (view === 'restarting' || (view === 'done' && !!baselineVersion && !reconciled && !pollTimedOut)) return;
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, view, reconciled, pollTimedOut, baselineVersion, onClose]);

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
        } else if (s.state === 'restarting') {
          // Fall back to server's previous_version if baseline wasn't set yet.
          if (!baselineVersionRef.current) setBaseline(s.previous_version ?? null);
          setView('restarting');
        }
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

  // Poll /api/status while restarting or done-but-unreconciled; reload when the
  // new version reports in. Baseline is fixed at upgrade-start time (not derived
  // from render-time state) so it survives setStatus(null) without drifting.
  useEffect(() => {
    const isWaiting =
      view === 'restarting' ||
      (view === 'done' && !reconciled && !pollTimedOut);
    if (!open || !isWaiting) return;

    const baseline = baselineVersionRef.current;
    // No baseline → skip polling entirely. This prevents an unrelated gateway
    // restart from triggering a false reload when we have nothing to compare.
    if (!baseline) return;

    if (view === 'restarting') {
      if (restartStartedAtRef.current == null) restartStartedAtRef.current = Date.now();
    }
    // 1Hz tick only while restarting so the elapsed counter re-renders.
    const tickId = view === 'restarting'
      ? window.setInterval(() => setRestartTick((n) => n + 1), 1000)
      : null;

    let active = true;
    let inFlight = false;
    // Cancel the reload only when the dialog is dismissed; a view transition
    // (restarting → done) must not suppress it.
    let reloadCancelled = false;
    const deadline = Date.now() + RESTART_TIMEOUT_MS;

    const tick = async () => {
      if (inFlight) return;
      inFlight = true;
      try {
        const s = await getStatus();
        if (!active) return;
        const v = s.version ?? null;
        if (v && v !== baseline && (!targetVersion || v === targetVersion)) {
          active = false;
          window.clearInterval(id);
          setReconciled(true);
          if (view === 'restarting') setView('done');
          window.setTimeout(() => {
            if (!reloadCancelled) window.location.reload();
          }, RELOAD_AFTER_RECONCILE_MS);
          return;
        }
      } catch {
        /* gateway is down mid-restart — keep polling */
      } finally {
        inFlight = false;
      }
      if (!active) return;
      if (Date.now() > deadline) {
        active = false;
        window.clearInterval(id);
        setPollTimedOut(true);
        if (view === 'restarting') setView('done');
      }
    };

    void tick();
    const id = window.setInterval(() => void tick(), RESTART_POLL_MS);
    return () => {
      active = false;
      window.clearInterval(id);
      if (tickId !== null) window.clearInterval(tickId);
      if (!open) reloadCancelled = true;
    };
  }, [open, view, reconciled, pollTimedOut, targetVersion]);

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
      // Capture baseline before setStatus(null) — info?.current_version is the
      // only reliable source of the old version string at this point.
      setBaseline(info?.current_version ?? null);
      setStatus(null);
      setView('progress');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setView('failed');
    }
  };

  // Backdrop dismissal is suppressed while restarting or waiting for a manual
  // restart — closing either would stop the /api/status poll that auto-reloads.
  const handleBackdropClick = (view === 'restarting' || (view === 'done' && !!baselineVersion && !reconciled && !pollTimedOut)) ? undefined : onClose;

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
            ) : !checkUpdatesEnabled && info == null ? (
              // `gateway.check_updates=false` and no manual re-check has been
              // triggered yet. Render an explicit "checks disabled" state
              // instead of falling through to the version summary — which
              // would otherwise show an empty latest-version row plus the
              // misleading `up_to_date` message (an operator who intentionally
              // disabled polling has not established that they're up to date).
              // The refresh button in the header remains active so a manual
              // one-shot re-check can still be run on demand.
              <div className="text-xs text-pc-text-muted">
                {t('upgrade.checks_disabled')}
              </div>
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
            const startedAt = restartStartedAtRef.current;
            const elapsedSec = startedAt != null
              ? Math.max(0, Math.floor((Date.now() - startedAt) / 1000))
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
                  {targetVersion && (
                    <>
                      {' '}
                      <span className="font-mono text-pc-text">v{targetVersion}</span>
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
                {reconciled && targetVersion && (
                  <span>
                    {' '}
                    — {t('upgrade.now_running')}{' '}
                    <span className="font-mono">v{targetVersion}</span>
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
                <div className="text-pc-text-muted flex flex-col gap-1">
                  <div>
                    {t('upgrade.restart_to_apply')}
                    {restartHint && (
                      <code className="ml-1 font-mono text-pc-text">{restartHint}</code>
                    )}
                  </div>
                  {baselineVersion && !pollTimedOut && (
                    <div className="flex items-center gap-1.5">
                      <Loader2 className="h-3 w-3 animate-spin" />
                      {t('upgrade.waiting_for_restart')}
                    </div>
                  )}
                  {pollTimedOut && (
                    <div className="text-pc-text-muted">
                      {t('upgrade.poll_timed_out')}
                    </div>
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

          {/* During restart or while waiting for manual restart, keep the dialog
              modal — closing stops the /api/status poll that triggers auto-reload.
              Once reconciled (reload armed) or on failure, show Close normally. */}

          {(view === 'failed' || (view === 'done' && (reconciled || !baselineVersion || pollTimedOut))) && (
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
