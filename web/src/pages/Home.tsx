import { useState, type ReactNode } from 'react';
import { Link, Navigate, useSearchParams } from 'react-router-dom';
import {
  Activity,
  ArrowRight,
  Bot,
  Clock3,
  Code2,
  Coins,
  HeartPulse,
  History,
  Layers3,
  Puzzle,
  Settings2,
  ShieldCheck,
  Monitor,
  Smartphone,
  Stethoscope,
  Terminal,
  Sparkles,
  Workflow,
  Wrench,
} from 'lucide-react';
import {
  getCost,
  getRunningSessions,
  getSessions,
  getStatus,
  getWorkspaceAvailability,
  type WorkspaceAvailability,
} from '@/lib/api';
import { listRuns, type SopRunSummary } from '@/lib/sops';
import { sessionTarget } from '@/lib/sessionNavigation';
import { formatRelative, formatUsd } from '@/lib/format';
import { useCodeSessions } from '@/hooks/useCodeSessions';
import { usePolling } from '@/hooks/usePolling';
import { useWorkspaceSettings } from '@/components/WorkspaceSettings';
import type { CostSummary, Session, StatusResponse } from '@/types/api';
import { t } from '@/lib/i18n';

type Snapshot<T> = { data: T | null; error: boolean };
const empty = <T,>(): Snapshot<T> => ({ data: null, error: false });

export default function Home() {
  const [params] = useSearchParams();
  return params.has('tab') ? (
    <Navigate to={`/system?${params}`} replace />
  ) : (
    <Overview />
  );
}

function Overview() {
  const [availability, setAvailability] = useState(
    empty<WorkspaceAvailability>,
  );
  const [history, setHistory] = useState(empty<Session[]>);
  const [running, setRunning] = useState(
    empty<Awaited<ReturnType<typeof getRunningSessions>>>,
  );
  const [runs, setRuns] = useState(empty<SopRunSummary[]>);
  const [status, setStatus] = useState(empty<StatusResponse>);
  const [cost, setCost] = useState(empty<CostSummary>);
  const code = useCodeSessions(availability.data?.code === true);
  const settings = useWorkspaceSettings();

  // These are read-only views of the existing owners. One unavailable service
  // must not turn its unknown count into zero or hide the other dashboards.
  usePolling(async (stale) => {
    const update = async <T,>(
      read: () => Promise<T>,
      write: (result: Snapshot<T>) => void,
    ) => {
      try {
        const data = await read();
        if (!stale()) write({ data, error: false });
      } catch {
        if (!stale()) write({ data: null, error: true });
      }
    };
    await Promise.all([
      update(getWorkspaceAvailability, setAvailability),
      update(getSessions, setHistory),
      update(getRunningSessions, setRunning),
      update(getStatus, setStatus),
      update(getCost, setCost),
    ]);
  }, 10000);
  usePolling(
    async (stale) => {
      try {
        const data = await listRuns();
        if (!stale()) setRuns({ data, error: false });
      } catch {
        if (!stale()) setRuns({ data: null, error: true });
      }
    },
    10000,
    [],
    availability.data?.workflows === true,
  );

  const sessions = [...(history.data ?? []), ...code.sessions]
    .sort((a, b) => b.last_activity.localeCompare(a.last_activity))
    .slice(0, 4);
  const activeRuns = availability.data?.workflows
    ? (runs.data ?? []).filter((run) => run.active)
    : [];
  const recentRuns = [...(runs.data ?? [])]
    .sort(
      (a, b) =>
        Number(b.active) - Number(a.active) ||
        b.started_at.localeCompare(a.started_at),
    )
    .slice(0, 4);
  const runningKnown =
    availability.data &&
    running.data &&
    !code.error &&
    code.loaded &&
    (!availability.data.workflows || runs.data);
  const activeCount = runningKnown
    ? running.data!.sessions.length +
      code.sessions.filter((session) => session.state === 'running').length +
      activeRuns.length
    : null;
  const components = Object.entries(
    status.data?.health.components ?? {},
  ).filter(([name]) => !name.startsWith('channel:'));
  const unhealthy = components.filter(
    ([, value]) => !['ok', 'healthy'].includes(value.status.toLowerCase()),
  );
  const healthLabel = status.error
    ? t('home.offline')
    : !status.data || !components.length
      ? '—'
      : t(unhealthy.length ? 'home.check_health' : 'home.healthy');
  const runningKeys = new Set(
    running.data?.sessions.map((session) => `gw_${session.session_id}`),
  );

  return (
    <div className="mx-auto max-w-6xl space-y-7 px-5 py-7 sm:px-8 sm:py-9">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <p className="mb-2 text-xs font-medium uppercase tracking-[0.16em] text-pc-accent">
            {t('home.overview')}
          </p>
          <h1 className="text-2xl font-semibold tracking-tight sm:text-3xl">
            {t('home.heading')}
          </h1>
          <p className="mt-2 max-w-xl text-sm leading-relaxed text-pc-text-secondary">
            {t('home.dashboard_description')}
          </p>
        </div>
        <Link
          to="/system"
          className="mt-1 inline-flex items-center gap-2 rounded-lg border border-pc-border px-3 py-2 text-xs text-pc-text-secondary hover:bg-pc-elevated"
        >
          <Activity className="h-3.5 w-3.5" />
          {t('home.system_details')}
          <ArrowRight className="h-3.5 w-3.5" />
        </Link>
      </div>

      {availability.data?.agents.length === 0 && (
        <Link
          to="/quickstart"
          className="flex items-center justify-between gap-4 rounded-xl border border-pc-accent/30 bg-pc-accent/5 p-4"
        >
          <span>
            <strong className="block text-sm">{t('home.setup_title')}</strong>
            <span className="mt-1 block text-xs text-pc-text-secondary">
              {t('home.setup_hint')}
            </span>
          </span>
          <ArrowRight className="h-4 w-4 shrink-0" />
        </Link>
      )}

      <section
        aria-label={t('home.overview')}
        className="grid grid-cols-2 gap-3 lg:grid-cols-4"
      >
        <Metric
          to="/agents"
          icon={Bot}
          label={t('home.agents_ready')}
          value={availability.data?.agents.length ?? '—'}
          detail={t(
            availability.error
              ? 'home.refresh_failed'
              : 'home.agents_ready_hint',
          )}
        />
        <Metric
          to="/sessions"
          icon={Activity}
          label={t('home.active')}
          value={activeCount ?? '—'}
          detail={t(
            availability.error ||
              running.error ||
              code.error ||
              (availability.data?.workflows && runs.error)
              ? 'home.refresh_failed'
              : 'home.active_hint',
          )}
        />
        <Metric
          to="/system?tab=cost"
          icon={Coins}
          label={t('home.daily_usage')}
          value={
            cost.data
              ? cost.data.daily_cost_usd === 0
                ? '$0.00'
                : formatUsd(cost.data.daily_cost_usd)
              : '—'
          }
          detail={t(
            cost.error ? 'home.refresh_failed' : 'home.daily_usage_hint',
          )}
        />
        <Metric
          to="/system?tab=health"
          icon={HeartPulse}
          label={t('home.system_health')}
          value={healthLabel}
          detail={t(
            status.error
              ? 'home.refresh_failed'
              : unhealthy.length
                ? 'home.health_attention'
                : 'home.health_hint',
          )}
          attention={unhealthy.length > 0 || status.error}
        />
      </section>

      <div className="grid gap-5 lg:grid-cols-2">
        <Panel title={t('home.recent')} to="/sessions" icon={History}>
          {(history.error || code.error) && (
            <Notice>{t('home.refresh_failed')}</Notice>
          )}
          {!history.data && !history.error && (
            <Notice>{t('common.loading')}</Notice>
          )}
          {history.data && !sessions.length && (
            <Notice>
              {t(
                availability.data?.session_persistence === false
                  ? 'home.history_disabled'
                  : 'home.no_sessions',
              )}
            </Notice>
          )}
          {sessions.map((session) => {
            const isCode = 'surface' in session && session.surface === 'code';
            const Icon = isCode ? Code2 : Bot;
            const active =
              runningKeys.has(session.session_key) ||
              ('state' in session && session.state === 'running');
            return (
              <Link
                key={`${isCode ? 'code' : 'chat'}:${session.session_key}`}
                to={sessionTarget(session)}
                className="group flex items-center gap-3 rounded-xl px-3 py-3 hover:bg-pc-elevated"
              >
                <span className="rounded-lg bg-pc-elevated p-2 text-pc-text-secondary">
                  <Icon className="h-4 w-4" />
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium">
                    {session.name ||
                      session.agent_alias ||
                      t('home.untitled_session')}
                  </span>
                  <span className="mt-1 block truncate text-xs text-pc-text-muted">
                    {t(isCode ? 'nav.code' : 'workspace.agent')} ·{' '}
                    {formatRelative(session.last_activity)}
                  </span>
                </span>
                {active && (
                  <span className="text-xs text-pc-accent">
                    {t('code.working')}
                  </span>
                )}
                <ArrowRight className="h-3.5 w-3.5 shrink-0 text-pc-text-muted group-hover:text-pc-accent" />
              </Link>
            );
          })}
        </Panel>
        <Panel title={t('home.sop_activity')} to="/runs" icon={Workflow}>
          {availability.error ? (
            <Notice>{t('home.refresh_failed')}</Notice>
          ) : availability.data?.workflows === false ? (
            <Notice>
              <Link to="/sops" className="hover:text-pc-accent">
                {t('home.sops_not_running')}
              </Link>
            </Notice>
          ) : runs.error ? (
            <Notice>{t('home.refresh_failed')}</Notice>
          ) : !runs.data ? (
            <Notice>{t('common.loading')}</Notice>
          ) : !recentRuns.length ? (
            <Notice>{t('home.no_runs')}</Notice>
          ) : (
            recentRuns.map((run) => (
              <Link
                key={run.run_id}
                to={`/runs/${encodeURIComponent(run.sop_name)}/${encodeURIComponent(run.run_id)}`}
                className="group flex items-center gap-3 rounded-xl px-3 py-3 hover:bg-pc-elevated"
              >
                <span
                  className={`h-2 w-2 shrink-0 rounded-full ${run.status === 'failed' ? 'bg-status-error' : run.active ? 'bg-pc-accent' : 'bg-pc-text-faint'}`}
                />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-medium">
                    {run.sop_name}
                  </span>
                  <span className="mt-1 block text-xs text-pc-text-muted">
                    {formatRelative(run.started_at)}
                  </span>
                </span>
                <span
                  className={`rounded-md px-2 py-1 text-xs ${run.status === 'failed' ? 'bg-status-error/10 text-status-error' : run.active ? 'bg-pc-accent/10 text-pc-accent' : 'text-pc-text-muted'}`}
                >
                  {t(`sops.run_status.${run.status}`)}
                </span>
              </Link>
            ))
          )}
        </Panel>
      </div>

      <section aria-label={t('home.workspaces')}>
        <h2 className="mb-3 text-sm font-medium">{t('home.workspaces')}</h2>
        <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-4">
          {[
            {
              to: '/agent',
              label: 'workspace.agent',
              description: 'home.agent_description',
              icon: Bot,
            },
            {
              to: '/code',
              label: 'nav.code',
              description: 'home.code_description',
              icon: Code2,
            },
            {
              to: '/sops',
              label: 'workspace.sop',
              description: 'home.sop_description',
              icon: Workflow,
            },
            { to: '/admin', label: 'workspace.admin', description: 'admin.description', icon: ShieldCheck },
          ].map(({ to, label, description, icon: Icon }) => (
            <Link
              key={to}
              to={to}
              className="group rounded-xl border border-pc-border bg-pc-surface p-5 transition-colors hover:border-pc-accent/40 hover:bg-pc-elevated"
            >
              <div className="flex items-center gap-2.5">
                <Icon className="h-4 w-4 text-pc-accent" />
                <h3 className="text-sm font-medium">{t(label)}</h3>
                <ArrowRight className="ml-auto h-3.5 w-3.5 text-pc-text-muted group-hover:text-pc-accent" />
              </div>
              <p className="mt-3 text-sm leading-relaxed text-pc-text-secondary">
                {t(description)}
              </p>
            </Link>
          ))}
        </div>
      </section>
      <details className="rounded-xl border border-pc-border bg-pc-surface p-5">
        <summary className="cursor-pointer text-sm font-medium">
          {t('home.more_features')}
        </summary>
        <div className="mt-4 grid gap-x-6 gap-y-1 sm:grid-cols-2 lg:grid-cols-3">
          {[
            {
              to: '/sessions',
              label: 'home.sessions',
              description: 'home.sessions_description',
              icon: History,
            },
            {
              to: '/tools',
              label: 'nav.tools',
              description: 'home.tools_description',
              icon: Wrench,
            },
            {
              to: '/skills',
              label: 'nav.skills',
              description: 'home.skills_description',
              icon: Sparkles,
            },
            {
              to: '/cron',
              label: 'nav.cron',
              description: 'home.cron_description',
              icon: Clock3,
            },
            {
              to: '/integrations',
              label: 'nav.integrations',
              description: 'home.integrations_description',
              icon: Puzzle,
            },
            {
              to: '/system',
              label: 'nav.system',
              description: 'home.system_description',
              icon: Layers3,
            },
            {
              to: '/logs',
              label: 'nav.logs',
              description: 'home.logs_description',
              icon: Terminal,
            },
            {
              to: '/doctor',
              label: 'nav.doctor',
              description: 'home.doctor_description',
              icon: Stethoscope,
            },
            {
              to: '/pairing',
              label: 'nav.pairing',
              description: 'home.pairing_description',
              icon: Smartphone,
            },
            {
              to: '/canvas',
              label: 'nav.canvas',
              description: 'home.canvas_description',
              icon: Monitor,
            },
            {
              to: '/acp-console',
              label: 'nav.acp',
              description: 'home.acp_description',
              icon: Code2,
            },
          ].map(({ to, label, description, icon: Icon }) => (
            <Link
              key={to}
              to={to}
              className="flex items-start gap-3 rounded-lg py-3 hover:text-pc-accent"
            >
              <Icon className="mt-0.5 h-4 w-4 shrink-0 text-pc-text-muted" />
              <span>
                <span className="block text-sm font-medium">{t(label)}</span>
                <span className="mt-1 block text-xs leading-relaxed text-pc-text-muted">
                  {t(description)}
                </span>
              </span>
            </Link>
          ))}
          <button
            type="button"
            onClick={() => settings('/config')}
            className="flex items-start gap-3 rounded-lg py-3 text-left hover:text-pc-accent"
          >
            <Settings2 className="mt-0.5 h-4 w-4 shrink-0 text-pc-text-muted" />
            <span>
              <span className="block text-sm font-medium">
                {t('workspace.settings')}
              </span>
              <span className="mt-1 block text-xs leading-relaxed text-pc-text-muted">
                {t('home.settings_description')}
              </span>
            </span>
          </button>
        </div>
      </details>
    </div>
  );
}

function Metric({
  to,
  icon: Icon,
  label,
  value,
  detail,
  attention = false,
}: {
  to: string;
  icon: typeof Bot;
  label: string;
  value: ReactNode;
  detail: string;
  attention?: boolean;
}) {
  return (
    <Link
      to={to}
      className="rounded-xl border border-pc-border bg-pc-surface px-4 py-4 transition-colors hover:border-pc-border-strong sm:px-5"
    >
      <span className="flex items-center gap-2 text-xs text-pc-text-secondary">
        <Icon className="h-3.5 w-3.5" />
        {label}
      </span>
      <span
        className={`mt-3 block text-xl font-semibold tracking-tight sm:text-2xl ${attention ? 'text-status-warning' : 'text-pc-text'}`}
      >
        {value}
      </span>
      <span className="mt-1.5 block text-xs leading-relaxed text-pc-text-muted">
        {detail}
      </span>
    </Link>
  );
}
function Panel({
  title,
  to,
  icon: Icon,
  children,
}: {
  title: string;
  to: string;
  icon: typeof Bot;
  children: ReactNode;
}) {
  return (
    <section className="min-w-0 rounded-xl border border-pc-border bg-pc-surface p-3">
      <div className="mb-2 flex items-center justify-between gap-3 px-3 py-2">
        <h2 className="flex items-center gap-2 text-sm font-medium">
          <Icon className="h-4 w-4 text-pc-text-muted" />
          {title}
        </h2>
        <Link
          to={to}
          className="text-xs text-pc-text-muted hover:text-pc-accent"
        >
          {t('home.view_all')} →
        </Link>
      </div>
      {children}
    </section>
  );
}
function Notice({ children }: { children: ReactNode }) {
  return (
    <p className="px-3 py-5 text-sm leading-relaxed text-pc-text-muted">
      {children}
    </p>
  );
}
