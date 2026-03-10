import { useEffect, useState } from 'react';
import type { ComponentType, ReactNode, SVGProps } from 'react';
import {
  Activity,
  ChevronDown,
  Clock3,
  Cpu,
  Database,
  DollarSign,
  Globe2,
  Radio,
  ShieldCheck,
  Sparkles,
} from 'lucide-react';
import type { CostSummary, StatusResponse } from '@/types/api';
import { getCost, getStatus } from '@/lib/api';
import { t } from '@/lib/i18n';

type DashboardSectionKey = 'cost' | 'channels' | 'health';

interface DashboardSectionState {
  cost: boolean;
  channels: boolean;
  health: boolean;
}

interface CollapsibleSectionProps {
  title: string;
  subtitle: string;
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  sectionKey: DashboardSectionKey;
  openState: DashboardSectionState;
  onToggle: (section: DashboardSectionKey) => void;
  children: ReactNode;
}

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function formatUSD(value: number): string {
  return `$${value.toFixed(4)}`;
}

function healthColor(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'bg-emerald-400';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'bg-amber-400';
    default:
      return 'bg-rose-500';
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'border-emerald-500/30';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'border-amber-400/30';
    default:
      return 'border-rose-500/35';
  }
}

function CollapsibleSection({
  title,
  subtitle,
  icon: Icon,
  sectionKey,
  openState,
  onToggle,
  children,
}: CollapsibleSectionProps) {
  const isOpen = openState[sectionKey];

  return (
    <section className="electric-card motion-rise">
      <button
        type="button"
        onClick={() => onToggle(sectionKey)}
        aria-expanded={isOpen}
        className="group flex w-full items-center justify-between gap-4 rounded-xl px-4 py-4 text-left md:px-5"
      >
        <div className="flex items-center gap-3">
          <div className="electric-icon h-10 w-10 rounded-xl">
            <Icon className="h-5 w-5" />
          </div>
          <div>
            <h2 className="text-base font-semibold text-white">{title}</h2>
            <p className="text-xs uppercase tracking-[0.13em] text-[#7ea5eb]">{subtitle}</p>
          </div>
        </div>
        <ChevronDown
          className={[
            'h-5 w-5 text-[#7ea5eb] transition-transform duration-300',
            isOpen ? 'rotate-180' : 'rotate-0',
          ].join(' ')}
        />
      </button>
      <div
        className={[
          'grid overflow-hidden transition-[grid-template-rows,opacity] duration-300 ease-out',
          isOpen ? 'grid-rows-[1fr] opacity-100' : 'grid-rows-[0fr] opacity-0',
        ].join(' ')}
      >
        <div className="min-h-0 border-t border-[#18356f] px-4 pb-4 pt-4 md:px-5">{children}</div>
      </div>
    </section>
  );
}

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sectionsOpen, setSectionsOpen] = useState<DashboardSectionState>({
    cost: true,
    channels: true,
    health: true,
  });

  useEffect(() => {
    Promise.all([getStatus(), getCost()])
      .then(([statusPayload, costPayload]) => {
        setStatus(statusPayload);
        setCost(costPayload);
      })
      .catch((err: unknown) => {
        const message = err instanceof Error ? err.message : t('dashboard.load_unknown_error');
        setError(message);
      });
  }, []);

  const toggleSection = (section: DashboardSectionKey) => {
    setSectionsOpen((prev) => ({
      ...prev,
      [section]: !prev[section],
    }));
  };

  if (error) {
    return (
      <div className="electric-card p-5 text-rose-200">
        <h2 className="text-lg font-semibold text-rose-100">{t('dashboard.load_failed')}</h2>
        <p className="mt-2 text-sm text-rose-200/90">{error}</p>
      </div>
    );
  }

  if (!status || !cost) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="electric-loader h-12 w-12 rounded-full" />
      </div>
    );
  }

  const maxCost = Math.max(cost.session_cost_usd, cost.daily_cost_usd, cost.monthly_cost_usd, 0.001);

  return (
    <div className="space-y-5 md:space-y-6">
      <section className="hero-panel motion-rise">
        <div className="relative z-10 flex flex-wrap items-start justify-between gap-4">
          <div>
            <p className="text-xs uppercase tracking-[0.22em] text-[#8fb8ff]">{t('dashboard.hero_eyebrow')}</p>
            <h1 className="mt-2 text-2xl font-semibold tracking-[0.03em] text-white md:text-3xl">
              {t('dashboard.hero_title')}
            </h1>
            <p className="mt-2 max-w-2xl text-sm text-[#b3cbf8] md:text-base">
              {t('dashboard.hero_subtitle')}
            </p>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <span className="status-pill">
              <Sparkles className="h-3.5 w-3.5" />
              {t('dashboard.live_gateway')}
            </span>
            <span className="status-pill">
              <ShieldCheck className="h-3.5 w-3.5" />
              {status.paired ? t('dashboard.paired') : t('dashboard.unpaired')}
            </span>
          </div>
        </div>
      </section>

      <section className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4">
        <article className="electric-card motion-rise motion-delay-1 p-4">
          <div className="metric-head">
            <Cpu className="h-4 w-4" />
            <span>{t('dashboard.provider_model')}</span>
          </div>
          <p className="metric-value mt-3">{status.provider ?? t('dashboard.unknown_provider')}</p>
          <p className="metric-sub mt-1 truncate">{status.model}</p>
        </article>

        <article className="electric-card motion-rise motion-delay-2 p-4">
          <div className="metric-head">
            <Clock3 className="h-4 w-4" />
            <span>{t('dashboard.uptime')}</span>
          </div>
          <p className="metric-value mt-3">{formatUptime(status.uptime_seconds)}</p>
          <p className="metric-sub mt-1">{t('dashboard.since_last_restart')}</p>
        </article>

        <article className="electric-card motion-rise motion-delay-3 p-4">
          <div className="metric-head">
            <Globe2 className="h-4 w-4" />
            <span>{t('dashboard.gateway_port')}</span>
          </div>
          <p className="metric-value mt-3">:{status.gateway_port}</p>
          <p className="metric-sub mt-1">{status.locale}</p>
        </article>

        <article className="electric-card motion-rise motion-delay-4 p-4">
          <div className="metric-head">
            <Database className="h-4 w-4" />
            <span>{t('dashboard.memory_backend')}</span>
          </div>
          <p className="metric-value mt-3 capitalize">{status.memory_backend}</p>
          <p className="metric-sub mt-1">{status.paired ? t('dashboard.pairing_active') : t('dashboard.no_paired_devices')}</p>
        </article>
      </section>

      <div className="space-y-4">
        <CollapsibleSection
          title={t('dashboard.cost_pulse')}
          subtitle={t('dashboard.cost_subtitle')}
          icon={DollarSign}
          sectionKey="cost"
          openState={sectionsOpen}
          onToggle={toggleSection}
        >
          <div className="space-y-4">
            {[
              { label: t('dashboard.session'), value: cost.session_cost_usd },
              { label: t('dashboard.daily'), value: cost.daily_cost_usd },
              { label: t('dashboard.monthly'), value: cost.monthly_cost_usd },
            ].map(({ label, value }) => (
              <div key={label}>
                <div className="mb-1.5 flex items-center justify-between text-sm">
                  <span className="text-[#9bb8ec]">{label}</span>
                  <span className="font-semibold text-white">{formatUSD(value)}</span>
                </div>
                <div className="h-2.5 overflow-hidden rounded-full bg-[#061230]">
                  <div
                    className="electric-progress h-full rounded-full"
                    style={{ width: `${Math.max((value / maxCost) * 100, 3)}%` }}
                  />
                </div>
              </div>
            ))}

            <div className="grid grid-cols-2 gap-3 pt-2">
                <div className="metric-pill">
                <span>{t('cost.total_tokens')}</span>
                <strong>{cost.total_tokens.toLocaleString()}</strong>
              </div>
              <div className="metric-pill">
                <span>{t('cost.requests')}</span>
                <strong>{cost.request_count.toLocaleString()}</strong>
              </div>
            </div>
          </div>
        </CollapsibleSection>

        <CollapsibleSection
          title={t('dashboard.channel_activity')}
          subtitle={t('dashboard.channel_subtitle')}
          icon={Radio}
          sectionKey="channels"
          openState={sectionsOpen}
          onToggle={toggleSection}
        >
          {Object.entries(status.channels).length === 0 ? (
            <p className="text-sm text-[#8aa8df]">{t('dashboard.no_channels')}</p>
          ) : (
            <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
              {Object.entries(status.channels).map(([name, active]) => (
                <div
                  key={name}
                  className="rounded-xl border border-[#1d3770] bg-[#05112c]/90 px-3 py-2.5"
                >
                  <div className="flex items-center justify-between">
                    <span className="text-sm capitalize text-white">{name}</span>
                    <span className="flex items-center gap-2 text-xs text-[#8baee7]">
                      <span
                        className={[
                          'inline-block h-2.5 w-2.5 rounded-full',
                          active ? 'bg-emerald-400 shadow-[0_0_12px_0_rgba(52,211,153,0.8)]' : 'bg-slate-500',
                        ].join(' ')}
                      />
                      {active ? t('dashboard.active') : t('dashboard.inactive')}
                    </span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </CollapsibleSection>

        <CollapsibleSection
          title={t('dashboard.component_health')}
          subtitle={t('dashboard.component_subtitle')}
          icon={Activity}
          sectionKey="health"
          openState={sectionsOpen}
          onToggle={toggleSection}
        >
          {Object.entries(status.health.components).length === 0 ? (
            <p className="text-sm text-[#8aa8df]">{t('dashboard.no_component_health')}</p>
          ) : (
            <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
              {Object.entries(status.health.components).map(([name, component]) => (
                <div
                  key={name}
                  className={[
                    'rounded-xl border bg-[#05112c]/80 px-3 py-3',
                    healthBorder(component.status),
                  ].join(' ')}
                >
                  <div className="flex items-center justify-between">
                    <p className="text-sm font-semibold capitalize text-white">{name}</p>
                    <span className={['inline-block h-2.5 w-2.5 rounded-full', healthColor(component.status)].join(' ')} />
                  </div>
                  <p className="mt-1 text-xs uppercase tracking-[0.12em] text-[#87a9e5]">
                    {component.status}
                  </p>
                  {component.restart_count > 0 && (
                    <p className="mt-2 text-xs text-amber-300">
                      {t('dashboard.restarts')}: {component.restart_count}
                    </p>
                  )}
                </div>
              ))}
            </div>
          )}
        </CollapsibleSection>
      </div>
    </div>
  );
}
