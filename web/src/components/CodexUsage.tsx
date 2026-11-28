import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from 'react';
import { Gauge } from 'lucide-react';
import { AUTH_EVENTS, subscribeOidcSession } from '@/lib/auth';
import {
  CodexUsageChecker,
  formatPercent,
  type CodexUsageFetcher,
  type CodexUsageOk,
  type CodexUsageWindow,
} from '@/lib/codexUsage.logic';
import { t } from '@/lib/i18n';
import { Button } from '@/components/ui';

function localTime(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return t('agent.codex_usage_unknown');
  return date.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

function WindowRow({ label, window }: { label: string; window: CodexUsageWindow }) {
  const used = window.used_percent === null
    ? t('agent.codex_usage_unknown')
    : t('agent.codex_usage_used').replace('{percent}', formatPercent(window.used_percent));
  const reset = t('agent.codex_usage_resets').replace(
    '{time}',
    window.resets_at ? localTime(window.resets_at) : t('agent.codex_usage_unknown'),
  );
  return (
    <div className="flex flex-col gap-0.5">
      <dt className="text-pc-text-secondary">{label}</dt>
      <dd className="text-pc-text">
        {used}
        <span className="block text-pc-text-secondary">{reset}</span>
      </dd>
    </div>
  );
}

function Windows({ usage }: { usage: CodexUsageOk }) {
  return (
    <>
      <p className="break-all text-pc-text-secondary">
        {t('agent.codex_usage_route').replace('{route}', () => usage.route)}
      </p>
      <dl className="flex flex-col gap-2">
        {usage.five_hour && <WindowRow label={t('agent.codex_usage_five_hour')} window={usage.five_hour} />}
        {usage.weekly && <WindowRow label={t('agent.codex_usage_weekly')} window={usage.weekly} />}
      </dl>
      <p className="text-pc-text-secondary">
        {t('agent.codex_usage_checked').replace('{time}', localTime(usage.checked_at))}
      </p>
    </>
  );
}

/**
 * Operator-only, on-demand Codex subscription usage, beside the model picker.
 * Nothing is requested until "Check Codex usage" is pressed. The chat socket
 * owns the route; loading the model picker's label doesn't change it.
 */
export default function CodexUsage({
  agentAlias,
  modelInfoVersion,
  switching,
  connected,
  busy,
  sessionId,
  fetchUsage,
}: {
  agentAlias: string;
  modelInfoVersion: number;
  switching: boolean;
  connected: boolean;
  busy: boolean;
  sessionId: string;
  fetchUsage: CodexUsageFetcher;
}) {
  const checker = useMemo(() => new CodexUsageChecker(fetchUsage), [fetchUsage]);
  const state = useSyncExternalStore(
    (listener) => checker.subscribe(listener),
    () => checker.state,
  );
  const [open, setOpen] = useState(false);
  const panelId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);

  // Before paint: an old result must not flicker under a new chat/route.
  useLayoutEffect(() => {
    checker.reset();
  }, [checker, agentAlias, modelInfoVersion, sessionId, connected, busy, switching]);

  useLayoutEffect(() => {
    const clear = () => checker.reset();
    const unsubscribe = subscribeOidcSession(clear);
    const events = [...Object.values(AUTH_EVENTS), 'storage', 'blur', 'pagehide'];
    events.forEach((name) => window.addEventListener(name, clear));
    document.addEventListener('visibilitychange', clear);
    return () => {
      unsubscribe();
      events.forEach((name) => window.removeEventListener(name, clear));
      document.removeEventListener('visibilitychange', clear);
      clear();
    };
  }, [checker]);

  useEffect(() => {
    if (!open) return;
    const close = () => { checker.reset(); setOpen(false); };
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented || e.isComposing) return;
      close();
      const active = document.activeElement;
      if (!active || active === document.body || rootRef.current?.contains(active)) {
        buttonRef.current?.focus();
      }
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open, checker]);

  const loading = state.kind === 'loading';
  const usage = state.kind === 'ok' ? state.usage : null;

  return (
    <div className="relative min-w-0" ref={rootRef}>
      <button
        ref={buttonRef}
        type="button"
        title={t('agent.codex_usage')}
        onClick={() => { checker.reset(); setOpen(!open); }}
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        className="flex items-center gap-1.5 px-3 h-7 rounded-[var(--radius-md)] text-xs font-medium border border-pc-border bg-pc-elevated text-pc-text-secondary transition-colors hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
      >
        <Gauge className="h-3 w-3 shrink-0" aria-hidden="true" />
        <span className="truncate">{t('agent.codex_usage')}</span>
      </button>

      {open && (
        <section
          id={panelId}
          aria-labelledby={`${panelId}-h`}
          className="absolute end-0 mt-1.5 z-50 w-72 max-w-[85vw] rounded-[var(--radius-md)] border border-pc-border bg-pc-elevated p-3 text-xs shadow-[var(--pc-shadow-md)] flex flex-col gap-2"
        >
          <h2 id={`${panelId}-h`} className="text-sm font-semibold text-pc-text">
            {t('agent.codex_usage_heading')}
          </h2>
          <p className="text-pc-text-secondary">{t('agent.codex_usage_note')}</p>
          <Button
            size="sm"
            variant="ghost"
            disabled={busy || loading}
            onClick={() => void checker.check({ connected, switching })}
          >
            {loading ? t('agent.codex_usage_checking') : t('agent.codex_usage_check')}
          </Button>
          <div role="status" aria-live="polite" className="flex flex-col gap-2">
            {busy && <p className="text-pc-text-secondary">{t('agent.codex_usage_busy')}</p>}
            {state.kind === 'problem' && (
              <p className="text-status-error">{t(`agent.codex_usage_p_${state.problem}`)}</p>
            )}
            {usage && <Windows usage={usage} />}
          </div>
        </section>
      )}
    </div>
  );
}
