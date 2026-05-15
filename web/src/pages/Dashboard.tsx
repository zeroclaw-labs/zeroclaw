import { useState, useEffect, useCallback, useMemo } from 'react';
import { Link, useNavigate, useSearchParams } from 'react-router-dom';
import {
  Clock,
  Globe,
  Activity,
  ArrowUpDown,
  DollarSign,
  Radio,
  LayoutDashboard,
  Users,
  MessageSquare,
  Wifi,
  Plus,
  Trash2,
  Eye,
  X,
  Bot,
  Filter,
  Heart,
  ChevronRight,
  Cpu,
  MemoryStick,
  Brain,
  Search,
} from 'lucide-react';
import type {
  StatusResponse,
  CostSummary,
  Session,
  ChannelDetail,
  SessionMessageRow,
  ProcessStats,
} from '@/types/api';
import {
  getStatus,
  getCost,
  getSessions,
  getChannels,
  getSessionMessages,
  deleteSession,
  getMemory,
  storeMemory,
  deleteMemory,
  getMapKeys,
} from '@/lib/api';
import { resolveModelToProviderType } from '@/lib/configuredModels';
import type { CostRange } from '@/lib/api';
import type { MemoryEntry } from '@/types/api';
import { loadAgentSummaries, toggleAgentEnabled, type AgentSummary } from '@/lib/agents';
import AgentCard from '@/components/AgentCard';
import EntityLink from '@/components/EntityLink';
import EntityEnabledToggle from '@/components/EntityEnabledToggle';
import { useSSE } from '@/hooks/useSSE';
import { t } from '@/lib/i18n';

type TabId = 'overview' | 'sessions' | 'channels' | 'memories' | 'health' | 'cost';

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

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '—';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let v = bytes;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(v >= 100 ? 0 : v >= 10 ? 1 : 2)} ${units[i]}`;
}

function ProcessRamCard({ process }: { process?: ProcessStats }) {
  const supported = !!process && process.rss_bytes > 0;
  const hasTotal = supported && (process?.system_ram_total_bytes ?? 0) > 0;
  const pct = hasTotal
    ? (process!.rss_bytes / process!.system_ram_total_bytes!) * 100
    : null;
  return (
    <div className="card p-5 animate-slide-in-up">
      <div className="flex items-center gap-3 mb-3">
        <div
          className="p-2 rounded-2xl"
          style={{ background: 'rgba(var(--pc-accent-rgb), 0.08)', color: '#fbbf24' }}
        >
          <MemoryStick className="h-5 w-5" />
        </div>
        <span
          className="text-xs uppercase tracking-wider font-medium"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          RAM
        </span>
      </div>
      <p
        className="text-lg font-semibold truncate"
        style={{ color: 'var(--pc-text-primary)' }}
      >
        {supported ? formatBytes(process!.rss_bytes) : '—'}
      </p>
      <p className="text-sm truncate" style={{ color: 'var(--pc-text-muted)' }}>
        {pct !== null
          ? `${pct.toFixed(pct < 1 ? 2 : 1)}% of ${formatBytes(process!.system_ram_total_bytes)}`
          : supported
            ? 'resident (zeroclaw)'
            : 'not supported on this platform'}
      </p>
    </div>
  );
}

function ProcessCpuCard({ process }: { process?: ProcessStats }) {
  const supported = !!process && process.cpu_percent !== null;
  const pct = supported ? Math.max(0, process!.cpu_percent ?? 0) : 0;
  const ncpu = process?.num_cpus ?? 0;
  return (
    <div className="card p-5 animate-slide-in-up">
      <div className="flex items-center gap-3 mb-3">
        <div
          className="p-2 rounded-2xl"
          style={{ background: 'rgba(var(--pc-accent-rgb), 0.08)', color: '#a78bfa' }}
        >
          <Cpu className="h-5 w-5" />
        </div>
        <span
          className="text-xs uppercase tracking-wider font-medium"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          CPU
        </span>
      </div>
      <p
        className="text-lg font-semibold truncate"
        style={{ color: 'var(--pc-text-primary)' }}
      >
        {supported ? `${pct.toFixed(1)}%` : '—'}
      </p>
      <p className="text-sm truncate" style={{ color: 'var(--pc-text-muted)' }}>
        {supported
          ? ncpu > 0
            ? `${ncpu} cores · ${(pct / ncpu).toFixed(1)}% normalized`
            : 'across all cores'
          : 'not supported on this platform'}
      </p>
    </div>
  );
}

function formatLocalDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      year: 'numeric',
      month: 'short',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    });
  } catch {
    return iso;
  }
}

function formatRelative(iso: string): string {
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    return `${days}d ago`;
  } catch {
    return iso;
  }
}

function healthColor(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'var(--color-status-success)';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'var(--color-status-warning)';
    default:
      return 'var(--color-status-error)';
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'rgba(0, 230, 138, 0.2)';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'rgba(255, 170, 0, 0.2)';
    default:
      return 'rgba(255, 68, 102, 0.2)';
  }
}

function healthBg(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'rgba(0, 230, 138, 0.05)';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'rgba(255, 170, 0, 0.05)';
    default:
      return 'rgba(255, 68, 102, 0.05)';
  }
}

// Genuinely process-global tiles only. Provider/Model and Memory Backend
// were single-agent leftovers from pre-v0.8.0 and are gone: each agent now
// picks its own model_provider and memory backend (shown per agent on the
// agent cards above this grid).
const STATUS_CARDS = [
  {
    icon: Clock,
    accent: "#34d399",
    labelKey: "dashboard.uptime",
    getValue: (s: StatusResponse) => formatUptime(s.uptime_seconds),
    getSub: (_s: StatusResponse) => t("dashboard.since_last_restart"),
  },
  {
    icon: Globe,
    accent: "#a78bfa",
    labelKey: "dashboard.gateway_port",
    getValue: (s: StatusResponse) => `:${s.gateway_port}`,
    getSub: (_s: StatusResponse) => "",
  },
];

const TABS: { id: TabId; labelKey: string; icon: typeof LayoutDashboard }[] = [
  { id: 'overview', labelKey: 'dashboard.tab_overview', icon: LayoutDashboard },
  { id: 'sessions', labelKey: 'dashboard.tab_sessions', icon: Users },
  { id: 'channels', labelKey: 'dashboard.tab_channels', icon: Wifi },
  { id: 'memories', labelKey: 'dashboard.tab_memories', icon: Brain },
  { id: 'health', labelKey: 'dashboard.tab_health', icon: Heart },
  { id: 'cost', labelKey: 'dashboard.tab_cost', icon: DollarSign },
];

// ---------------------------------------------------------------------------
// Overview Tab (existing dashboard content)
// ---------------------------------------------------------------------------

function OverviewTab({
  status,
  cost,
  showAllChannels,
  setShowAllChannels,
}: {
  status: StatusResponse;
  cost: CostSummary;
  showAllChannels: boolean;
  setShowAllChannels: (fn: (v: boolean) => boolean) => void;
}) {
  const maxCost = Math.max(
    cost.session_cost_usd,
    cost.daily_cost_usd,
    cost.monthly_cost_usd,
    0.001
  );

  return (
    <>
      {/* Status Cards Grid */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {STATUS_CARDS.map(({ icon: Icon, accent, labelKey, getValue, getSub }) => (
          <div key={labelKey} className="card p-5 animate-slide-in-up">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 rounded-2xl" style={{ background: `rgba(var(--pc-accent-rgb), 0.08)`, color: accent, }}>
                <Icon className="h-5 w-5" />
              </div>
              <span className="text-xs uppercase tracking-wider font-medium" style={{ color: "var(--pc-text-muted)" }}>{t(labelKey)}</span>
            </div>
            <p className="text-lg font-semibold truncate" style={{ color: "var(--pc-text-primary)" }}>{getValue(status)}</p>
            <p className="text-sm truncate" style={{ color: "var(--pc-text-muted)" }}>{getSub(status)}</p>
          </div>
        ))}
        <ProcessRamCard process={status.process} />
        <ProcessCpuCard process={status.process} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6 stagger-children">
        {/* Cost Widget */}
        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <DollarSign className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
            <h2 className="text-sm font-semibold uppercase tracking-wider" style={{ color: "var(--pc-text-primary)" }}>{t("dashboard.cost_overview")}</h2>
          </div>
          <div className="space-y-4">
            {[
              {
                label: t("dashboard.session_label"),
                value: cost.session_cost_usd,
                color: "var(--pc-accent)",
              },
              {
                label: t("dashboard.daily_label"),
                value: cost.daily_cost_usd,
                color: "#34d399",
              },
              {
                label: t("dashboard.monthly_label"),
                value: cost.monthly_cost_usd,
                color: "#a78bfa",
              },
            ].map(({ label, value, color }) => (
              <div key={label}>
                <div className="flex justify-between text-sm mb-1.5">
                  <span style={{ color: "var(--pc-text-muted)" }}>{label}</span>
                  <span
                    className="font-medium font-mono"
                    style={{ color: "var(--pc-text-primary)" }}
                  >
                    {formatUSD(value)}
                  </span>
                </div>
                <div
                  className="w-full h-1.5 rounded-full overflow-hidden"
                  style={{ background: "var(--pc-hover)" }}
                >
                  <div
                    className="h-full rounded-full progress-bar-animated transition-all duration-700 ease-out"
                    style={{
                      width: `${Math.max((value / maxCost) * 100, 2)}%`,
                      background: color,
                    }}
                  />
                </div>
              </div>
            ))}
          </div>
          <div
            className="mt-5 pt-4 border-t flex justify-between text-sm"
            style={{ borderColor: "var(--pc-border)" }}
          >
            <span style={{ color: "var(--pc-text-muted)" }}>
              {t("dashboard.total_tokens_label")}
            </span>
            <span className="font-mono" style={{ color: "var(--pc-text-primary)" }}>
              {cost.total_tokens.toLocaleString()}
            </span>
          </div>
          <div className="flex justify-between text-sm mt-1">
            <span style={{ color: "var(--pc-text-muted)" }}>
              {t("dashboard.requests_label")}
            </span>
            <span className="font-mono" style={{ color: "var(--pc-text-primary)" }}>
              {cost.request_count.toLocaleString()}
            </span>
          </div>
        </div>

        {/* Active Channels */}
        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Radio className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.channels")}
            </h2>
            <button
              onClick={() => setShowAllChannels((v) => !v)}
              className="ml-auto flex items-center gap-1 rounded-full px-2.5 py-1 text-[10px] font-medium border transition-all"
              style={
                showAllChannels
                  ? {
                      background: "rgba(var(--pc-accent-rgb), 0.1)",
                      borderColor: "rgba(var(--pc-accent-rgb), 0.3)",
                      color: "var(--pc-accent-light)",
                    }
                  : {
                      background: "rgba(0, 230, 138, 0.08)",
                      borderColor: "rgba(0, 230, 138, 0.25)",
                      color: "#34d399",
                    }
              }
              aria-label={
                showAllChannels
                  ? t("dashboard.filter_active")
                  : t("dashboard.filter_all")
              }
            >
              {showAllChannels
                ? t("dashboard.filter_all")
                : t("dashboard.filter_active")}
            </button>
          </div>
          <div className="space-y-2 overflow-y-auto max-h-48 pr-1">
            {Object.entries(status.channels).length === 0 ? (
              <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
                {t("dashboard.no_channels")}
              </p>
            ) : (() => {
              const entries = Object.entries(status.channels).filter(
                ([, active]) => showAllChannels || active
              );
              if (entries.length === 0) {
                return (
                  <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
                    {t("dashboard.no_active_channels")}
                  </p>
                );
              }
              return entries.map(([name, active]) => (
                <EntityLink
                  key={name}
                  kind="channel"
                  id={name}
                  className="flex items-center justify-between py-2.5 px-3 rounded-xl transition-all hover:opacity-90"
                  style={{ background: 'var(--pc-bg-elevated)' }}
                  title={`Open channels.${name} config`}
                >
                  <span
                    className="text-sm font-mono font-medium"
                    style={{ color: 'var(--pc-text-primary)' }}
                  >
                    {name}
                  </span>
                  <span className="flex items-center gap-2">
                    <span
                      className="status-dot"
                      style={
                        active
                          ? {
                              background: 'var(--color-status-success)',
                              boxShadow: '0 0 6px var(--color-status-success)',
                            }
                          : { background: 'var(--pc-text-faint)' }
                      }
                    />
                    <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                      {active ? t('dashboard.active') : t('dashboard.inactive')}
                    </span>
                  </span>
                </EntityLink>
              ));
            })()}
          </div>
        </div>

        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Activity className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.component_health")}
            </h2>
          </div>
          {(() => {
            const components = status.health?.components ?? {};
            // Drop `channel:<type>.<alias>` rows: per-channel health lives in
            // the Channels tab where every channel already has its own card.
            // Component Health is for process-level supervisors only
            // (gateway, daemon, scheduler, ...).
            const entries = Object.entries(components).filter(
              ([name]) => !name.startsWith('channel:'),
            );
            if (entries.length === 0) {
              return (
                <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
                  {t("dashboard.no_components")}
                </p>
              );
            }
            const sorted = entries.slice().sort((a, b) => a[0].localeCompare(b[0]));
            return (
              <div className="space-y-2">
                {sorted.map(([name, comp]) => {
                  const display = name;
                  const lastErr = comp.last_error ?? null;
                  const lastOk = comp.last_ok ?? null;
                  return (
                    <div
                      key={name}
                      className="rounded-xl px-3 py-2"
                      style={{
                        border: `1px solid ${healthBorder(comp.status)}`,
                        background: healthBg(comp.status),
                      }}
                    >
                      <div className="flex items-center gap-2 mb-0.5">
                        <span
                          className="status-dot flex-shrink-0"
                          style={{
                            background: healthColor(comp.status),
                            boxShadow: `0 0 6px ${healthColor(comp.status)}`,
                          }}
                        />
                        <span
                          className="text-sm font-medium font-mono break-all"
                          style={{ color: "var(--pc-text-primary)" }}
                        >
                          {display}
                        </span>
                        <span
                          className="ml-auto text-[10px] uppercase font-medium px-1.5 py-0.5 rounded-full flex-shrink-0"
                          style={{
                            color: healthColor(comp.status),
                            background: 'transparent',
                            border: `1px solid ${healthBorder(comp.status)}`,
                          }}
                        >
                          {comp.status}
                        </span>
                      </div>
                      {lastErr ? (
                        <p
                          className="text-[11px] mt-1 font-mono break-words"
                          style={{ color: 'var(--color-status-error)' }}
                          title={lastErr}
                        >
                          ⚠ {lastErr.length > 120 ? lastErr.slice(0, 117) + '…' : lastErr}
                        </p>
                      ) : null}
                      <div className="flex items-center gap-3 text-[11px] mt-0.5" style={{ color: "var(--pc-text-muted)" }}>
                        {lastOk && (
                          <span title={`last ok: ${lastOk}`}>
                            ok {formatRelative(lastOk)}
                          </span>
                        )}
                        {comp.restart_count > 0 && (
                          <span style={{ color: "var(--color-status-warning)" }}>
                            {t("dashboard.restarts")}: {comp.restart_count}
                          </span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            );
          })()}
        </div>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// Sessions Tab
// ---------------------------------------------------------------------------

type SessionSort =
  | 'activity-desc'
  | 'activity-asc'
  | 'created-desc'
  | 'created-asc'
  | 'messages-desc'
  | 'messages-asc';

const SESSION_SORT_OPTIONS: { value: SessionSort; label: string }[] = [
  { value: 'activity-desc', label: 'Recent activity' },
  { value: 'activity-asc', label: 'Oldest activity' },
  { value: 'created-desc', label: 'Newest first' },
  { value: 'created-asc', label: 'Oldest first' },
  { value: 'messages-desc', label: 'Busiest' },
  { value: 'messages-asc', label: 'Quietest' },
];

function isSessionSort(v: string): v is SessionSort {
  return SESSION_SORT_OPTIONS.some((o) => o.value === v);
}

function SessionsTab() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchParams, setSearchParams] = useSearchParams();
  const agentFilter = searchParams.get('agent') ?? '';
  const channelFilter = searchParams.get('channel') ?? '';
  const searchQuery = searchParams.get('q') ?? '';
  const sortRaw = searchParams.get('sort') ?? '';
  const sortBy: SessionSort = isSessionSort(sortRaw) ? sortRaw : 'activity-desc';
  const setFilter = (key: 'agent' | 'channel' | 'q' | 'sort', value: string) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (value) next.set(key, value);
        else next.delete(key);
        return next;
      },
      { replace: true },
    );
  const setAgentFilter = (v: string) => setFilter('agent', v);
  const setChannelFilter = (v: string) => setFilter('channel', v);
  const setSearchQuery = (v: string) => setFilter('q', v);
  const setSortBy = (v: SessionSort) =>
    setFilter('sort', v === 'activity-desc' ? '' : v);
  const [inspect, setInspect] = useState<{
    session: Session;
    messages: SessionMessageRow[] | null;
    error: string | null;
  } | null>(null);
  const [inspectNewestFirst, setInspectNewestFirst] = useState(true);
  const [deleting, setDeleting] = useState<string | null>(null);

  const { events } = useSSE({
    filterTypes: ['session_update', 'session_created', 'session_closed'],
    autoConnect: true,
  });

  const loadSessions = useCallback(() => {
    getSessions()
      .then((data) => {
        setSessions(data);
        setLoading(false);
      })
      .catch((err) => {
        setError(err.message);
        setLoading(false);
      });
  }, []);

  useEffect(() => {
    loadSessions();
  }, [loadSessions]);

  useEffect(() => {
    if (events.length === 0) return;
    loadSessions();
  }, [events.length, loadSessions]);

  const knownAgents = useMemo(() => {
    const s = new Set<string>();
    for (const r of sessions) if (r.agent_alias) s.add(r.agent_alias);
    return Array.from(s).sort();
  }, [sessions]);

  const knownChannels = useMemo(() => {
    const s = new Set<string>();
    for (const r of sessions) if (r.channel_id) s.add(r.channel_id);
    return Array.from(s).sort();
  }, [sessions]);

  const visible = useMemo(() => {
    const needle = searchQuery.trim().toLowerCase();
    const filtered = sessions.filter((s) => {
      if (agentFilter && s.agent_alias !== agentFilter) return false;
      if (channelFilter && s.channel_id !== channelFilter) return false;
      if (needle) {
        const haystack = [
          s.session_id,
          s.session_key,
          s.name ?? '',
          s.agent_alias ?? '',
          s.channel_id ?? '',
        ]
          .join(' ')
          .toLowerCase();
        if (!haystack.includes(needle)) return false;
      }
      return true;
    });
    const sorted = [...filtered];
    sorted.sort((a, b) => {
      switch (sortBy) {
        case 'activity-asc':
          return a.last_activity.localeCompare(b.last_activity);
        case 'created-desc':
          return b.created_at.localeCompare(a.created_at);
        case 'created-asc':
          return a.created_at.localeCompare(b.created_at);
        case 'messages-desc':
          return b.message_count - a.message_count;
        case 'messages-asc':
          return a.message_count - b.message_count;
        case 'activity-desc':
        default:
          return b.last_activity.localeCompare(a.last_activity);
      }
    });
    return sorted;
  }, [sessions, agentFilter, channelFilter, searchQuery, sortBy]);

  const openInspect = (session: Session) => {
    setInspect({ session, messages: null, error: null });
    getSessionMessages(session.session_key)
      .then((resp) =>
        setInspect((curr) =>
          curr && curr.session.session_key === session.session_key
            ? { ...curr, messages: resp.messages }
            : curr,
        ),
      )
      .catch((err) =>
        setInspect((curr) =>
          curr && curr.session.session_key === session.session_key
            ? { ...curr, error: err.message }
            : curr,
        ),
      );
  };

  const handleDelete = async (session: Session) => {
    if (deleting) return;
    if (!window.confirm(`Delete session ${session.session_id}? This cannot be undone.`)) {
      return;
    }
    setDeleting(session.session_key);
    try {
      await deleteSession(session.session_key);
      setSessions((prev) => prev.filter((s) => s.session_key !== session.session_key));
      if (inspect?.session.session_key === session.session_key) setInspect(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeleting(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-48">
        <div className="flex items-center gap-3">
          <div
            className="h-6 w-6 border-2 rounded-full animate-spin"
            style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
          />
          <span className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            {t('dashboard.loading_sessions')}
          </span>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div
        className="rounded-2xl border p-4"
        style={{
          background: 'var(--color-status-error-alpha-08)',
          borderColor: 'var(--color-status-error-alpha-20)',
          color: 'var(--color-status-error)',
        }}
      >
        {t('dashboard.load_sessions_error')}: {error}
      </div>
    );
  }

  return (
    <div className="card p-5 animate-slide-in-up space-y-4">
      <div className="flex items-center gap-2 flex-wrap">
        <Users className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          {t('dashboard.sessions_title')}
        </h2>
        <span
          className="text-xs font-mono px-2 py-0.5 rounded-full"
          style={{ background: 'rgba(var(--pc-accent-rgb), 0.1)', color: 'var(--pc-accent)' }}
        >
          {visible.length}
          {visible.length !== sessions.length ? ` / ${sessions.length}` : ''}
        </span>

        <div className="ml-auto flex items-center gap-2 flex-wrap">
          <div className="relative">
            <Search
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <input
              type="search"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search…"
              className="input-electric pl-7 pr-2 py-1 text-xs w-40"
              title="Substring match on id, key, name, agent, channel"
              aria-label="Search sessions"
            />
          </div>
          <div className="relative">
            <ArrowUpDown
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={sortBy}
              onChange={(e) =>
                setSortBy(e.target.value as SessionSort)
              }
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Sort sessions"
              aria-label="Sort sessions"
            >
              {SESSION_SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Bot
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={agentFilter}
              onChange={(e) => setAgentFilter(e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Filter by owning agent"
            >
              <option value="">All agents</option>
              {knownAgents.map((a) => (
                <option key={a} value={a}>
                  {a}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Filter
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={channelFilter}
              onChange={(e) => setChannelFilter(e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Filter by owning channel"
            >
              <option value="">All channels</option>
              {knownChannels.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </div>
        </div>
      </div>

      {visible.length === 0 ? (
        <p className="text-sm py-8 text-center" style={{ color: 'var(--pc-text-faint)' }}>
          {sessions.length === 0
            ? t('dashboard.no_sessions')
            : 'No sessions match the current search and filters'}
        </p>
      ) : (
        <div className="space-y-2 overflow-y-auto max-h-[32rem]">
          {visible.map((session) => (
            <div
              key={session.session_key}
              className="flex items-center justify-between py-3 px-4 rounded-xl"
              style={{ background: 'var(--pc-bg-elevated)', border: '1px solid transparent' }}
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-start gap-2 mb-1 flex-wrap">
                  <span
                    className="text-sm font-medium font-mono break-all"
                    style={{ color: 'var(--pc-text-primary)' }}
                  >
                    {session.session_id}
                  </span>
                  {session.agent_alias && (
                    <EntityLink
                      kind="agent"
                      id={session.agent_alias}
                      className="text-[10px] font-medium px-2 py-0.5 rounded-full flex-shrink-0 hover:underline"
                      style={{
                        background: 'rgba(var(--pc-accent-rgb), 0.10)',
                        color: 'var(--pc-accent-light)',
                      }}
                      title={`Open agents.${session.agent_alias} config`}
                    >
                      {session.agent_alias}
                    </EntityLink>
                  )}
                  {session.channel_id && (
                    <EntityLink
                      kind="channel"
                      id={session.channel_id}
                      className="text-[10px] font-mono px-2 py-0.5 rounded-full flex-shrink-0 hover:underline"
                      style={{
                        background: 'rgba(167, 139, 250, 0.10)',
                        color: '#a78bfa',
                      }}
                      title={`Open channels.${session.channel_id} config`}
                    >
                      {session.channel_id}
                    </EntityLink>
                  )}
                </div>
                <div
                  className="flex items-center gap-3 text-xs"
                  style={{ color: 'var(--pc-text-muted)' }}
                >
                  <span className="flex items-center gap-1">
                    <MessageSquare className="h-3 w-3" />
                    {session.message_count}
                  </span>
                  <span>{formatRelative(session.last_activity)}</span>
                </div>
              </div>
              <div className="flex items-center gap-1 flex-shrink-0">
                <button
                  type="button"
                  onClick={() => openInspect(session)}
                  className="p-1.5 rounded-lg hover:bg-[var(--pc-hover)]"
                  title="View messages"
                  style={{ color: 'var(--pc-text-muted)' }}
                >
                  <Eye className="h-4 w-4" />
                </button>
                <button
                  type="button"
                  onClick={() => handleDelete(session)}
                  disabled={deleting === session.session_key}
                  className="p-1.5 rounded-lg hover:bg-[var(--pc-hover)] disabled:opacity-50"
                  title="Delete session"
                  style={{ color: 'var(--color-status-error)' }}
                >
                  <Trash2 className="h-4 w-4" />
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {inspect && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center p-4"
          style={{ background: 'rgba(0,0,0,0.5)' }}
          onClick={() => setInspect(null)}
        >
          <div
            className="card p-5 w-full max-w-3xl max-h-[80vh] overflow-hidden flex flex-col"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-start justify-between mb-4 gap-3">
              <div className="min-w-0">
                <p
                  className="text-xs uppercase tracking-wider mb-1"
                  style={{ color: 'var(--pc-text-faint)' }}
                >
                  Session
                </p>
                <p
                  className="text-sm font-mono break-all"
                  style={{ color: 'var(--pc-text-primary)' }}
                >
                  {inspect.session.session_id}
                </p>
                <div className="flex items-center gap-2 mt-1 flex-wrap">
                  {inspect.session.agent_alias && (
                    <EntityLink
                      kind="agent"
                      id={inspect.session.agent_alias}
                      className="text-[10px] font-medium px-2 py-0.5 rounded-full hover:underline"
                      style={{
                        background: 'rgba(var(--pc-accent-rgb), 0.10)',
                        color: 'var(--pc-accent-light)',
                      }}
                    >
                      {inspect.session.agent_alias}
                    </EntityLink>
                  )}
                  {inspect.session.channel_id && (
                    <EntityLink
                      kind="channel"
                      id={inspect.session.channel_id}
                      className="text-[10px] font-mono px-2 py-0.5 rounded-full hover:underline"
                      style={{
                        background: 'rgba(167, 139, 250, 0.10)',
                        color: '#a78bfa',
                      }}
                    >
                      {inspect.session.channel_id}
                    </EntityLink>
                  )}
                </div>
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                {inspect.messages && inspect.messages.length > 1 && (
                  <button
                    type="button"
                    onClick={() => setInspectNewestFirst((v) => !v)}
                    className="text-[10px] font-medium px-2 py-1 rounded-lg hover:bg-[var(--pc-hover)] border"
                    style={{
                      color: 'var(--pc-text-muted)',
                      borderColor: 'var(--pc-border)',
                    }}
                    title="Flip transcript order"
                  >
                    {inspectNewestFirst ? 'newest first' : 'oldest first'}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setInspect(null)}
                  className="p-1 rounded-lg hover:bg-[var(--pc-hover)]"
                  style={{ color: 'var(--pc-text-muted)' }}
                  title="Close"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
            </div>
            <div className="flex-1 overflow-y-auto space-y-3 pr-1">
              {inspect.error ? (
                <p className="text-sm" style={{ color: 'var(--color-status-error)' }}>
                  {inspect.error}
                </p>
              ) : inspect.messages === null ? (
                <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
                  Loading transcript…
                </p>
              ) : inspect.messages.length === 0 ? (
                <p className="text-sm" style={{ color: 'var(--pc-text-faint)' }}>
                  No persisted messages for this session.
                </p>
              ) : (
                (inspectNewestFirst
                  ? inspect.messages.slice().reverse()
                  : inspect.messages
                ).map((m, i) => (
                  <div
                    key={i}
                    className="rounded-xl px-3 py-2"
                    style={{ background: 'var(--pc-bg-elevated)' }}
                  >
                    <div className="flex items-baseline justify-between gap-3 mb-1">
                      <p
                        className="text-[10px] uppercase tracking-wider font-mono"
                        style={{ color: 'var(--pc-text-faint)' }}
                      >
                        {m.role}
                      </p>
                      {m.created_at && (
                        <p
                          className="text-[10px] font-mono whitespace-nowrap"
                          style={{ color: 'var(--pc-text-faint)' }}
                          title={m.created_at}
                        >
                          {formatLocalDateTime(m.created_at)}
                        </p>
                      )}
                    </div>
                    <p
                      className="text-sm whitespace-pre-wrap break-words"
                      style={{ color: 'var(--pc-text-primary)' }}
                    >
                      {m.content}
                    </p>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Channels Tab
// ---------------------------------------------------------------------------

function ChannelsTab() {
  const [channels, setChannels] = useState<ChannelDetail[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const { events } = useSSE({
    filterTypes: ['channel_update', 'channel_status'],
    autoConnect: true,
  });

  const loadChannels = useCallback(() => {
    getChannels()
      .then((data) => {
        setChannels(data);
        setLoading(false);
      })
      .catch((err) => {
        setError(err.message);
        setLoading(false);
      });
  }, []);

  useEffect(() => {
    loadChannels();
  }, [loadChannels]);

  // React to SSE events for real-time updates
  useEffect(() => {
    if (events.length === 0) return;
    loadChannels();
  }, [events.length, loadChannels]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-48">
        <div className="flex items-center gap-3">
          <div
            className="h-6 w-6 border-2 rounded-full animate-spin"
            style={{ borderColor: "var(--pc-border)", borderTopColor: "var(--pc-accent)" }}
          />
          <span className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
            {t("dashboard.loading_channels")}
          </span>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div
        className="rounded-2xl border p-4"
        style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}
      >
        {t("dashboard.load_channels_error")}: {error}
      </div>
    );
  }

  if (channels.length === 0) {
    return (
      <div className="card p-5 animate-slide-in-up">
        <p className="text-sm py-8 text-center" style={{ color: "var(--pc-text-faint)" }}>
          {t("dashboard.no_channels_detail")}
        </p>
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4 stagger-children">
      {channels.map((channel) => (
        <div
          key={channel.name}
          className="card p-5 animate-slide-in-up transition-all"
          style={{
            border: `1px solid ${healthBorder(channel.health)}`,
            background: healthBg(channel.health),
          }}
          onMouseEnter={(e) => {
            e.currentTarget.style.transform = "translateY(-2px)";
            e.currentTarget.style.boxShadow = `0 4px 12px ${healthBorder(channel.health)}`;
          }}
          onMouseLeave={(e) => {
            e.currentTarget.style.transform = "translateY(0)";
            e.currentTarget.style.boxShadow = "none";
          }}
        >
          {/* Header */}
          <div className="flex items-center justify-between mb-4">
            <div className="flex items-center gap-3 min-w-0">
              <div
                className="p-2 rounded-2xl flex-shrink-0"
                style={{ background: `rgba(var(--pc-accent-rgb), 0.08)`, color: "var(--pc-accent)" }}
              >
                <Radio className="h-5 w-5" />
              </div>
              <div className="min-w-0">
                <EntityLink
                  kind="channel"
                  id={channel.name}
                  className="text-sm font-semibold font-mono break-all hover:underline"
                  title={`Open channels.${channel.name} config`}
                >
                  <span style={{ color: 'var(--pc-text-primary)' }}>{channel.name}</span>
                </EntityLink>
                <span className="text-xs block" style={{ color: 'var(--pc-text-muted)' }}>
                  {channel.owning_agent ? (
                    <>
                      owned by{' '}
                      <EntityLink
                        kind="agent"
                        id={channel.owning_agent}
                        className="hover:underline font-mono"
                        title={`Open agents.${channel.owning_agent} config`}
                      >
                        {channel.owning_agent}
                      </EntityLink>
                    </>
                  ) : (
                    'no owning agent'
                  )}
                </span>
              </div>
            </div>
            <span
              className="status-dot"
              style={{
                background: healthColor(channel.health),
                boxShadow: `0 0 6px ${healthColor(channel.health)}`,
              }}
            />
          </div>

          <div className="flex items-center gap-2 mb-3">
            <EntityEnabledToggle
              prefix={`channels.${channel.type}.${channel.alias}`}
              enabled={channel.enabled}
              onChange={(next) =>
                setChannels((prev) =>
                  prev.map((c) =>
                    c.name === channel.name ? { ...c, enabled: next } : c,
                  ),
                )
              }
            />
          </div>

          {/* Stats. `message_count` / `last_message_at` come back as
              hardcoded 0 / null from the gateway — drop those rows until the
              backend wires real counters. Health stays since it reflects the
              listener supervisor's state. */}
          <div
            className="pt-3 border-t space-y-2"
            style={{ borderColor: "var(--pc-border)" }}
          >
            <div className="flex justify-between text-xs">
              <span style={{ color: "var(--pc-text-muted)" }}>{t("dashboard.health")}</span>
              <span style={{ color: healthColor(channel.health) }}>
                {channel.health}
              </span>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Dashboard Component
// ---------------------------------------------------------------------------

const TAB_IDS: TabId[] = ['overview', 'sessions', 'channels', 'memories', 'health', 'cost'];

function parseTab(raw: string | null): TabId {
  if (raw && (TAB_IDS as string[]).includes(raw)) return raw as TabId;
  return 'overview';
}

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [costRange, setCostRange] = useState<CostRange>('today');
  const [error, setError] = useState<string | null>(null);
  const [showAllChannels, setShowAllChannels] = useState(false);
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = parseTab(searchParams.get('tab'));
  const setActiveTab = (id: TabId) => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (id === 'overview') next.delete('tab');
        else next.set('tab', id);
        // Filters belong to specific tabs; drop them when leaving so deep
        // links don't drag a stale agent= into the wrong tab.
        if (id !== 'sessions' && id !== 'memories') {
          next.delete('agent');
        }
        if (id !== 'sessions') {
          next.delete('channel');
        }
        if (id !== 'memories') {
          next.delete('category');
        }
        return next;
      },
      { replace: true },
    );
  };

  useEffect(() => {
    let cancelled = false;
    const refresh = () => {
      Promise.all([getStatus(), getCost(costRange)])
        .then(([s, c]) => {
          if (cancelled) return;
          setStatus(s);
          setCost(c);
        })
        .catch((err) => {
          if (!cancelled) setError(err.message);
        });
    };
    refresh();
    // Uptime ticks every second on the server; poll every 5s so the tile and
    // health badges stay live without hammering the gateway.
    const id = window.setInterval(refresh, 5000);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [costRange]);

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
          {t("dashboard.load_error")}: {error}
        </div>
      </div>
    );
  }

  if (!status || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: "var(--pc-border)", borderTopColor: "var(--pc-accent)", }}/>
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <AgentsSection />

      {/* Global system stats — tab navigation */}
      <div
        className="flex items-center gap-1 p-1 rounded-2xl"
        style={{ background: "var(--pc-bg-elevated)" }}
      >
        {TABS.map(({ id, labelKey, icon: Icon }) => (
          <button
            key={id}
            onClick={() => setActiveTab(id)}
            className="flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all"
            style={
              activeTab === id
                ? {
                    background: "var(--pc-bg-primary)",
                    color: "var(--pc-accent)",
                    boxShadow: "0 1px 3px rgba(0, 0, 0, 0.1)",
                  }
                : {
                    background: "transparent",
                    color: "var(--pc-text-muted)",
                  }
            }
            onMouseEnter={(e) => {
              if (activeTab !== id) {
                e.currentTarget.style.color = "var(--pc-text-primary)";
              }
            }}
            onMouseLeave={(e) => {
              if (activeTab !== id) {
                e.currentTarget.style.color = "var(--pc-text-muted)";
              }
            }}
          >
            <Icon className="h-4 w-4" />
            {t(labelKey)}
          </button>
        ))}
      </div>

      {/* Tab Content */}
      {activeTab === 'overview' && (
        <OverviewTab
          status={status}
          cost={cost}
          showAllChannels={showAllChannels}
          setShowAllChannels={setShowAllChannels}
        />
      )}
      {activeTab === 'sessions' && <SessionsTab />}
      {activeTab === 'channels' && <ChannelsTab />}
      {activeTab === 'memories' && <MemoriesTab />}
      {activeTab === 'health' && <HealthTab status={status} />}
      {activeTab === 'cost' && (
        <CostTab cost={cost} range={costRange} onRangeChange={setCostRange} />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Health Tab
// ---------------------------------------------------------------------------

function HealthTab({ status }: { status: StatusResponse }) {
  const components = status.health?.components ?? {};
  const entries = Object.entries(components).filter(
    ([name]) => !name.startsWith('channel:'),
  );
  if (entries.length === 0) {
    return (
      <div className="card p-5 animate-slide-in-up">
        <p className="text-sm" style={{ color: 'var(--pc-text-faint)' }}>
          {t('dashboard.no_components')}
        </p>
      </div>
    );
  }
  const sorted = entries.slice().sort((a, b) => a[0].localeCompare(b[0]));
  return (
    <div className="card p-5 animate-slide-in-up">
      <div className="flex items-center gap-2 mb-5">
        <Activity className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          {t('dashboard.component_health')}
        </h2>
      </div>
      <div className="space-y-2">
        {sorted.map(([name, comp]) => {
          const lastErr = comp.last_error ?? null;
          const lastOk = comp.last_ok ?? null;
          return (
            <div
              key={name}
              className="rounded-xl px-3 py-2"
              style={{
                border: `1px solid ${healthBorder(comp.status)}`,
                background: healthBg(comp.status),
              }}
            >
              <div className="flex items-center gap-2 mb-0.5">
                <span
                  className="status-dot flex-shrink-0"
                  style={{
                    background: healthColor(comp.status),
                    boxShadow: `0 0 6px ${healthColor(comp.status)}`,
                  }}
                />
                <span
                  className="text-sm font-medium font-mono break-all"
                  style={{ color: 'var(--pc-text-primary)' }}
                >
                  {name}
                </span>
                <span
                  className="ml-auto text-[10px] uppercase font-medium px-1.5 py-0.5 rounded-full flex-shrink-0"
                  style={{
                    color: healthColor(comp.status),
                    background: 'transparent',
                    border: `1px solid ${healthBorder(comp.status)}`,
                  }}
                >
                  {comp.status}
                </span>
              </div>
              {lastErr && (
                <p
                  className="text-[11px] mt-1 font-mono break-words"
                  style={{ color: 'var(--color-status-error)' }}
                  title={lastErr}
                >
                  ⚠ {lastErr}
                </p>
              )}
              <div
                className="flex items-center gap-3 text-[11px] mt-0.5"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                {lastOk && <span title={`last ok: ${lastOk}`}>ok {formatRelative(lastOk)}</span>}
                {comp.restart_count > 0 && (
                  <span style={{ color: 'var(--color-status-warning)' }}>
                    {t('dashboard.restarts')}: {comp.restart_count}
                  </span>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Cost Tab — full by-model + by-agent rollup
// ---------------------------------------------------------------------------

// Cost dashboard: per-day totals plus per-agent and per-model rollups
// with input / output / cached token splits. Both rollups are daily-scoped
// at the tracker level so they survive daemon restarts. The model row
// click-through resolves the provider type by walking configured
// `providers.models.<type>.<alias>.model` (the model id is the rate
// sheet key; the alias is its home) and lands on that type's Costs tab.
const COST_RANGE_OPTIONS: { value: CostRange; label: string; window: string }[] = [
  { value: 'today', label: 'Today', window: 'today' },
  { value: 'last_7_days', label: 'Last 7 days', window: 'last 7 days' },
  { value: 'last_30_days', label: 'Last 30 days', window: 'last 30 days' },
  { value: 'current_month', label: 'This month', window: 'this month' },
  { value: 'all_time', label: 'All time', window: 'all time' },
];

function CostTab({
  cost,
  range,
  onRangeChange,
}: {
  cost: CostSummary;
  range: CostRange;
  onRangeChange: (next: CostRange) => void;
}) {
  const byModel = Object.values(cost.by_model);
  const byAgent = Object.values(cost.by_agent);
  const navigate = useNavigate();
  const windowLabel =
    COST_RANGE_OPTIONS.find((o) => o.value === range)?.window ?? String(range);
  // Cache the model→type lookup once resolved so consecutive clicks
  // are instant. Starts empty: the per-row click handler resolves
  // on-demand, so there's no race with an in-flight initial fetch.
  const [modelToType, setModelToType] = useState<Record<string, string>>({});

  const openModelRates = async (modelId: string) => {
    let map = modelToType;
    if (!(modelId in map)) {
      try {
        const fresh = await resolveModelToProviderType('models');
        map = fresh;
        setModelToType(fresh);
      } catch {
        /* fall through to the unscoped rates editor */
      }
    }
    const type = map[modelId];
    if (type) {
      navigate(
        `/config/providers.models/${encodeURIComponent(type)}?tab=costs`,
      );
    } else {
      // No configured provider claims this model id; land on the
      // standalone Rates view with the URL params hydrating the
      // category so the operator can pick the right slot manually.
      navigate(
        `/config/cost?tab=rates&category=models&resource=${encodeURIComponent(modelId)}`,
      );
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-2">
        <label className="text-xs uppercase tracking-wider" style={{ color: 'var(--pc-text-secondary)' }}>
          Window
        </label>
        <select
          value={range}
          onChange={(e) => onRangeChange(e.target.value as CostRange)}
          className="input-electric text-sm px-2 py-1 appearance-none cursor-pointer"
        >
          {COST_RANGE_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {opt.label}
            </option>
          ))}
        </select>
      </div>
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
      <div className="card p-5 animate-slide-in-up">
        <div className="flex items-center gap-2 mb-5">
          <DollarSign className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Spend totals
          </h2>
        </div>
        <dl className="space-y-2 text-sm">
          {[
            ['Today', formatUSD(cost.daily_cost_usd)],
            ['This month', formatUSD(cost.monthly_cost_usd)],
          ].map(([label, value]) => (
            <div key={label} className="flex justify-between">
              <dt style={{ color: 'var(--pc-text-muted)' }}>{label}</dt>
              <dd className="font-mono" style={{ color: 'var(--pc-text-primary)' }}>
                {value}
              </dd>
            </div>
          ))}
        </dl>
        <p
          className="text-xs mt-3"
          style={{ color: 'var(--pc-text-faint)' }}
        >
          Daily and monthly aggregates over <code>state/costs.jsonl</code>.
          Per-agent and per-model rows below are scoped to today.
        </p>
      </div>

      <div className="card p-5 animate-slide-in-up">
        <div className="flex items-center gap-2 mb-5">
          <Bot className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Spend by agent · {windowLabel}
          </h2>
        </div>
        {byAgent.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-faint)' }}>
            No per-agent tracking. Enable <code>[cost].track_per_agent</code>.
          </p>
        ) : (
          <ul className="space-y-2 text-sm">
            {byAgent
              .slice()
              .sort((a, b) => b.cost_usd - a.cost_usd)
              .map((row) => (
                <li
                  key={row.agent_alias}
                  className="flex flex-col gap-1 rounded-xl px-3 py-2"
                  style={{ background: 'var(--pc-bg-elevated)' }}
                >
                  <div className="flex items-center justify-between gap-3">
                    <EntityLink
                      kind="agent"
                      id={row.agent_alias}
                      className="font-mono hover:underline"
                      title={`agents.${row.agent_alias}`}
                    >
                      agents.{row.agent_alias}
                    </EntityLink>
                    <span className="font-mono" style={{ color: 'var(--pc-text-primary)' }}>
                      {formatUSD(row.cost_usd)}
                    </span>
                  </div>
                  <div
                    className="flex items-center gap-3 text-xs flex-wrap"
                    style={{ color: 'var(--pc-text-muted)' }}
                  >
                    <span>{row.request_count} exchanges</span>
                    <span>{row.input_tokens.toLocaleString()} input tokens</span>
                    {row.cached_input_tokens > 0 && (
                      <span>{row.cached_input_tokens.toLocaleString()} cached</span>
                    )}
                    <span>{row.output_tokens.toLocaleString()} output tokens</span>
                  </div>
                </li>
              ))}
          </ul>
        )}
      </div>

      <div className="card p-5 lg:col-span-2 animate-slide-in-up">
        <div className="flex items-center gap-2 mb-5">
          <DollarSign className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Spend by model · {windowLabel}
          </h2>
        </div>
        {byModel.length === 0 ? (
          <p className="text-sm" style={{ color: 'var(--pc-text-faint)' }}>
            No model usage recorded in this window.
          </p>
        ) : (
          <ul className="space-y-2 text-sm">
            {byModel
              .slice()
              .sort((a, b) => b.cost_usd - a.cost_usd)
              .map((row) => (
                <li
                  key={row.model}
                  className="flex flex-col gap-1 rounded-xl px-3 py-2"
                  style={{ background: 'var(--pc-bg-elevated)' }}
                >
                  <div className="flex items-center justify-between gap-3">
                    <button
                      type="button"
                      onClick={() => void openModelRates(row.model)}
                      className="font-mono break-all hover:underline text-left"
                      style={{ color: 'var(--pc-text-primary)', background: 'transparent' }}
                      title={`Open the rate sheet entry for ${row.model}`}
                    >
                      {row.model}
                    </button>
                    <span className="font-mono" style={{ color: 'var(--pc-text-primary)' }}>
                      {formatUSD(row.cost_usd)}
                    </span>
                  </div>
                  <div
                    className="flex items-center gap-3 text-xs flex-wrap"
                    style={{ color: 'var(--pc-text-muted)' }}
                  >
                    <span>{row.request_count} exchanges</span>
                    <span>{row.input_tokens.toLocaleString()} input tokens</span>
                    {row.cached_input_tokens > 0 && (
                      <span>{row.cached_input_tokens.toLocaleString()} cached</span>
                    )}
                    <span>{row.output_tokens.toLocaleString()} output tokens</span>
                  </div>
                </li>
              ))}
          </ul>
        )}
      </div>
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Memories Tab — mirrors SessionsTab's shape (agent + category filters in
// URL params, per-row Delete) but for `getMemory()` results. The fuller
// /memory page (with the add-entry form) stays the canonical entry point
// for creating new rows; this tab is the cross-agent inspection surface.
// ---------------------------------------------------------------------------

type MemorySort = 'newest' | 'oldest' | 'key-asc' | 'key-desc';

const MEMORY_SORT_OPTIONS: { value: MemorySort; label: string }[] = [
  { value: 'newest', label: 'Newest first' },
  { value: 'oldest', label: 'Oldest first' },
  { value: 'key-asc', label: 'Key A → Z' },
  { value: 'key-desc', label: 'Key Z → A' },
];

function isMemorySort(v: string): v is MemorySort {
  return MEMORY_SORT_OPTIONS.some((o) => o.value === v);
}

function MemoriesTab() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchParams, setSearchParams] = useSearchParams();
  const agentFilter = searchParams.get('agent') ?? '';
  const categoryFilter = searchParams.get('category') ?? '';
  const searchQuery = searchParams.get('q') ?? '';
  const sortRaw = searchParams.get('sort') ?? '';
  const sortBy: MemorySort = isMemorySort(sortRaw) ? sortRaw : 'newest';
  // Debounced query so each keystroke doesn't fire a recall request to the
  // backend (which then hits the configured memory store — markdown read,
  // sqlite scan, qdrant vector search, etc.).
  const [debouncedQuery, setDebouncedQuery] = useState(searchQuery);
  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(searchQuery), 250);
    return () => window.clearTimeout(id);
  }, [searchQuery]);
  const [knownAgents, setKnownAgents] = useState<string[]>([]);
  const [deleting, setDeleting] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [showAddForm, setShowAddForm] = useState(false);
  const [formKey, setFormKey] = useState('');
  const [formContent, setFormContent] = useState('');
  const [formCategory, setFormCategory] = useState('');
  const [formAgent, setFormAgent] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const toggleExpanded = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const setFilter = (key: 'agent' | 'category' | 'q' | 'sort', value: string) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (value) next.set(key, value);
        else next.delete(key);
        return next;
      },
      { replace: true },
    );
  const setSearchQuery = (v: string) => setFilter('q', v);
  const setSortBy = (v: MemorySort) =>
    setFilter('sort', v === 'newest' ? '' : v);

  const reload = useCallback(() => {
    setLoading(true);
    getMemory(
      debouncedQuery.trim() || undefined,
      categoryFilter || undefined,
      agentFilter || undefined,
    )
      .then((rows) => {
        setEntries(rows);
        setLoading(false);
      })
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
        setLoading(false);
      });
  }, [agentFilter, categoryFilter, debouncedQuery]);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    getMapKeys('agents')
      .then((r) => setKnownAgents(r.keys))
      .catch(() => {
        /* dropdown stays empty; filter still works as a typed value */
      });
  }, []);

  const knownCategories = useMemo(() => {
    const s = new Set<string>();
    for (const e of entries) if (e.category) s.add(e.category);
    return Array.from(s).sort();
  }, [entries]);

  const visibleEntries = useMemo(() => {
    const sorted = [...entries];
    sorted.sort((a, b) => {
      switch (sortBy) {
        case 'oldest':
          return a.timestamp.localeCompare(b.timestamp);
        case 'key-asc':
          return a.key.localeCompare(b.key);
        case 'key-desc':
          return b.key.localeCompare(a.key);
        case 'newest':
        default:
          return b.timestamp.localeCompare(a.timestamp);
      }
    });
    return sorted;
  }, [entries, sortBy]);

  const handleDelete = async (entry: MemoryEntry) => {
    if (deleting) return;
    if (!window.confirm(`Delete memory ${entry.key}? This cannot be undone.`)) return;
    setDeleting(entry.id);
    try {
      // Per-agent rows resolve through the agent's own memory backend; the
      // install-wide entries (agent_alias == null) hit the gateway's default
      // handle. Without this, deleting a per-agent row from the dashboard
      // hits the wrong backend and silently no-ops.
      await deleteMemory(entry.key, entry.agent_alias ?? undefined);
      setEntries((prev) => prev.filter((e) => e.id !== entry.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setDeleting(null);
    }
  };

  const handleAdd = async () => {
    if (!formKey.trim() || !formContent.trim()) {
      setFormError('Key and content are required');
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await storeMemory(
        formKey.trim(),
        formContent.trim(),
        formCategory.trim() || undefined,
        formAgent.trim() || undefined,
      );
      setShowAddForm(false);
      setFormKey('');
      setFormContent('');
      setFormCategory('');
      setFormAgent('');
      reload();
    } catch (e) {
      setFormError(e instanceof Error ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-48">
        <div className="flex items-center gap-3">
          <div
            className="h-6 w-6 border-2 rounded-full animate-spin"
            style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
          />
          <span className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
            Loading memories…
          </span>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div
        className="rounded-2xl border p-4"
        style={{
          background: 'var(--color-status-error-alpha-08)',
          borderColor: 'var(--color-status-error-alpha-20)',
          color: 'var(--color-status-error)',
        }}
      >
        {error}
      </div>
    );
  }

  return (
    <div className="card p-5 animate-slide-in-up space-y-4">
      <div className="flex items-center gap-2 flex-wrap">
        <Brain className="h-5 w-5" style={{ color: 'var(--pc-accent)' }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-primary)' }}
        >
          Memories
        </h2>
        <span
          className="text-xs font-mono px-2 py-0.5 rounded-full"
          style={{ background: 'rgba(var(--pc-accent-rgb), 0.1)', color: 'var(--pc-accent)' }}
        >
          {visibleEntries.length}
          {visibleEntries.length !== entries.length ? ` / ${entries.length}` : ''}
        </span>
        <button
          type="button"
          onClick={() => {
            setShowAddForm(true);
            setFormError(null);
            // Default the modal's agent select to whichever agent the
            // list is currently filtered to. Operators who narrowed the
            // view to "clamps" almost always want their new row written
            // there too; the alternative is forgetting to pick and
            // landing on the install-wide backend.
            setFormAgent(agentFilter);
          }}
          className="btn-electric text-xs ml-2 inline-flex items-center gap-1 px-2.5 py-1 rounded-lg"
          title="Add a memory entry"
        >
          <Plus className="h-3 w-3" />
          Add memory
        </button>

        <div className="ml-auto flex items-center gap-2 flex-wrap">
          <div className="relative">
            <Search
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <input
              type="search"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Search…"
              className="input-electric pl-7 pr-2 py-1 text-xs w-40"
              title="Backend-side recall — searches across the configured memory store. Combined with the agent and category filters."
              aria-label="Search memories"
            />
          </div>
          <div className="relative">
            <ArrowUpDown
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={sortBy}
              onChange={(e) => setSortBy(e.target.value as MemorySort)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Sort memories"
              aria-label="Sort memories"
            >
              {MEMORY_SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Bot
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={agentFilter}
              onChange={(e) => setFilter('agent', e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Filter by owning agent"
            >
              <option value="">All agents</option>
              {knownAgents.map((a) => (
                <option key={a} value={a}>
                  {a}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Filter
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: 'var(--pc-text-faint)' }}
            />
            <select
              value={categoryFilter}
              onChange={(e) => setFilter('category', e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title="Filter by category"
            >
              <option value="">All categories</option>
              {knownCategories.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </div>
        </div>
      </div>

      {visibleEntries.length === 0 ? (
        <p className="text-sm py-8 text-center" style={{ color: 'var(--pc-text-faint)' }}>
          No memories match the current search and filters
        </p>
      ) : (
        <div className="space-y-2 overflow-y-auto max-h-[32rem]">
          {visibleEntries.map((entry) => (
            <div
              key={entry.id}
              className="flex items-start justify-between gap-3 py-3 px-4 rounded-xl"
              style={{ background: 'var(--pc-bg-elevated)' }}
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-start gap-2 mb-1 flex-wrap">
                  <span
                    className="text-sm font-medium font-mono break-all"
                    style={{ color: 'var(--pc-text-primary)' }}
                  >
                    {entry.key}
                  </span>
                  {entry.agent_alias && (
                    <EntityLink
                      kind="agent"
                      id={entry.agent_alias}
                      className="text-[10px] font-medium px-2 py-0.5 rounded-full flex-shrink-0 hover:underline"
                      style={{
                        background: 'rgba(var(--pc-accent-rgb), 0.10)',
                        color: 'var(--pc-accent-light)',
                      }}
                      title={`Open agents.${entry.agent_alias} config`}
                    >
                      {entry.agent_alias}
                    </EntityLink>
                  )}
                  {entry.category && (
                    <span
                      className="text-[10px] font-mono px-2 py-0.5 rounded-full flex-shrink-0"
                      style={{
                        background: 'rgba(167, 139, 250, 0.10)',
                        color: '#a78bfa',
                      }}
                    >
                      {entry.category}
                    </span>
                  )}
                </div>
                <MemoryContent
                  content={entry.content}
                  expanded={expanded.has(entry.id)}
                  onToggle={() => toggleExpanded(entry.id)}
                />
                <p
                  className="text-[10px] font-mono mt-1"
                  style={{ color: 'var(--pc-text-faint)' }}
                  title={entry.timestamp}
                >
                  {formatLocalDateTime(entry.timestamp)}
                </p>
              </div>
              <button
                type="button"
                onClick={() => handleDelete(entry)}
                disabled={deleting === entry.id}
                className="p-1.5 rounded-lg hover:bg-[var(--pc-hover)] disabled:opacity-50 flex-shrink-0"
                title="Delete memory"
                style={{ color: 'var(--color-status-error)' }}
              >
                <Trash2 className="h-4 w-4" />
              </button>
            </div>
          ))}
        </div>
      )}

      {showAddForm && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center p-4"
          style={{ background: 'rgba(0,0,0,0.5)' }}
          onClick={() => setShowAddForm(false)}
        >
          <div
            className="card p-6 w-full max-w-md"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between mb-4">
              <h3
                className="text-lg font-semibold"
                style={{ color: 'var(--pc-text-primary)' }}
              >
                Add memory
              </h3>
              <button
                type="button"
                onClick={() => setShowAddForm(false)}
                className="p-1 rounded-lg hover:bg-[var(--pc-hover)]"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            {formError && (
              <div
                className="mb-4 rounded-xl border p-3 text-sm"
                style={{
                  background: 'var(--color-status-error-alpha-08)',
                  borderColor: 'var(--color-status-error-alpha-20)',
                  color: 'var(--color-status-error)',
                }}
              >
                {formError}
              </div>
            )}
            <div className="space-y-4">
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  Key <span style={{ color: 'var(--color-status-error)' }}>*</span>
                </label>
                <input
                  type="text"
                  value={formKey}
                  onChange={(e) => setFormKey(e.target.value)}
                  placeholder="e.g. user_preferences"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  Content <span style={{ color: 'var(--color-status-error)' }}>*</span>
                </label>
                <textarea
                  value={formContent}
                  onChange={(e) => setFormContent(e.target.value)}
                  placeholder="Memory content…"
                  rows={4}
                  className="input-electric w-full px-3 py-2.5 text-sm resize-none"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  Category (optional)
                </label>
                <input
                  type="text"
                  value={formCategory}
                  onChange={(e) => setFormCategory(e.target.value)}
                  placeholder="e.g. preferences, context, facts"
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: 'var(--pc-text-secondary)' }}
                >
                  Agent (optional)
                </label>
                <select
                  value={formAgent}
                  onChange={(e) => setFormAgent(e.target.value)}
                  className="input-electric w-full px-3 py-2.5 text-sm appearance-none cursor-pointer"
                >
                  <option value="">Install-wide (no agent attribution)</option>
                  {knownAgents.map((a) => (
                    <option key={a} value={a}>
                      {a}
                    </option>
                  ))}
                </select>
                <p
                  className="text-[11px] mt-1"
                  style={{ color: 'var(--pc-text-faint)' }}
                >
                  Picks which agent's memory backend the row lands in. Leave
                  blank to write to the gateway's default backend.
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-3 mt-6">
              <button
                type="button"
                onClick={() => setShowAddForm(false)}
                className="btn-secondary px-4 py-2 text-sm font-medium"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={handleAdd}
                disabled={submitting}
                className="btn-electric px-4 py-2 text-sm font-medium disabled:opacity-50"
              >
                {submitting ? 'Saving…' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

// Collapse anything that wouldn't fit comfortably inline. The thresholds
// are deliberately low — a one-paragraph note (~280 chars on one line) is
// fine, but anything multi-line or longer gets a toggle so the operator
// can decide. Avoids the prior bug where a row with 4 newlines and 250
// chars looked truncated (trailing `…` in the markdown body) but had no
// expand affordance.
const MEMORY_PREVIEW_CHARS = 280;
const MEMORY_PREVIEW_NEWLINES = 2;

function MemoryContent({
  content,
  expanded,
  onToggle,
}: {
  content: string;
  expanded: boolean;
  onToggle: () => void;
}) {
  const newlines = (content.match(/\n/g) ?? []).length;
  const oversize =
    content.length > MEMORY_PREVIEW_CHARS || newlines > MEMORY_PREVIEW_NEWLINES;
  const display = !oversize || expanded ? content : truncateForPreview(content);
  return (
    <>
      <p
        className="text-sm whitespace-pre-wrap break-words"
        style={{ color: 'var(--pc-text-secondary)' }}
      >
        {display}
      </p>
      {oversize && (
        <button
          type="button"
          onClick={onToggle}
          className="text-[11px] mt-1 hover:underline"
          style={{ color: 'var(--pc-accent)' }}
        >
          {expanded
            ? 'Collapse'
            : `Expand (${content.length.toLocaleString()} chars, ${newlines + 1} lines)`}
        </button>
      )}
    </>
  );
}

function truncateForPreview(content: string): string {
  // Slice on newlines first so we don't cut mid-paragraph. If that already
  // dropped lines, the `…` reflects real omission. Then char-limit if the
  // newline slice is still too wide; the slice + `…` always means there's
  // more behind the cut.
  const lines = content.split('\n');
  const slicedByNewline = lines.length > MEMORY_PREVIEW_NEWLINES;
  const byNewline = slicedByNewline
    ? lines.slice(0, MEMORY_PREVIEW_NEWLINES).join('\n')
    : content;
  if (byNewline.length > MEMORY_PREVIEW_CHARS) {
    return `${byNewline.slice(0, MEMORY_PREVIEW_CHARS).trimEnd()}…`;
  }
  return slicedByNewline ? `${byNewline}\n…` : byNewline;
}

// ---------------------------------------------------------------------------
// AgentsSection — top-of-dashboard agent grid. Always visible (above the
// global-stats tabs) so the dashboard reads as "many agents + system state"
// rather than "the agent". Same card component used on /agents.
// ---------------------------------------------------------------------------

function AgentsSection() {
  const [agents, setAgents] = useState<AgentSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<Set<string>>(new Set());

  useEffect(() => {
    loadAgentSummaries()
      .then(setAgents)
      .catch((err: unknown) =>
        setError(err instanceof Error ? err.message : 'Failed to load agents'),
      );
  }, []);

  const handleToggle = useCallback(async (agent: AgentSummary) => {
    setToggling((prev) => new Set(prev).add(agent.alias));
    try {
      await toggleAgentEnabled(agent.alias, !agent.enabled);
      setAgents((prev) =>
        prev?.map((a) =>
          a.alias === agent.alias ? { ...a, enabled: !a.enabled } : a,
        ) ?? null,
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : `Failed to toggle ${agent.alias}`);
    } finally {
      setToggling((prev) => {
        const next = new Set(prev);
        next.delete(agent.alias);
        return next;
      });
    }
  }, []);

  // Cap on-dashboard agent cards so 10+ agents don't push the rest of the
  // dashboard below the fold. The full grid lives at /agents (linked via
  // "View all"). Show enabled agents first so the glance is informative
  // even when the cap clips disabled or paused agents.
  const AGENT_GLANCE_LIMIT = 6;
  const sortedAgents = agents
    ? [...agents].sort((a, b) => {
        if (a.enabled !== b.enabled) return a.enabled ? -1 : 1;
        return a.alias.localeCompare(b.alias);
      })
    : null;
  const visibleAgents = sortedAgents ? sortedAgents.slice(0, AGENT_GLANCE_LIMIT) : null;
  const hiddenCount = sortedAgents ? Math.max(0, sortedAgents.length - AGENT_GLANCE_LIMIT) : 0;

  return (
    <section>
      <header className="flex items-center justify-between mb-4">
        <div className="flex items-center gap-2">
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            Agents
          </h2>
          {sortedAgents && sortedAgents.length > 0 && (
            <span
              className="text-xs font-mono px-2 py-0.5 rounded-full"
              style={{ background: 'rgba(var(--pc-accent-rgb), 0.1)', color: 'var(--pc-accent)' }}
            >
              {sortedAgents.length}
            </span>
          )}
        </div>
        <Link
          to="/agents"
          className="text-xs flex items-center gap-1 hover:underline"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          {hiddenCount > 0 ? `View all (${sortedAgents!.length})` : 'View all'}
          <ChevronRight className="h-3 w-3" />
        </Link>
      </header>

      {error && (
        <div
          className="mb-3 px-3 py-2 rounded-xl border text-xs"
          style={{
            background: 'var(--color-status-error-alpha-08)',
            borderColor: 'var(--color-status-error-alpha-20)',
            color: 'var(--color-status-error)',
          }}
        >
          {error}
        </div>
      )}

      {agents === null ? (
        <div
          className="rounded-2xl border p-6 text-center text-sm"
          style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
        >
          Loading agents...
        </div>
      ) : agents.length === 0 ? (
        <div
          className="rounded-2xl border-2 border-dashed p-6 text-center"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <p
            className="text-sm font-medium mb-2"
            style={{ color: 'var(--pc-text-primary)' }}
          >
            No agents configured yet.
          </p>
          <Link
            to="/onboard"
            className="btn-electric inline-flex items-center gap-2 px-3 py-1.5 rounded-xl text-xs"
          >
            <Plus className="h-3.5 w-3.5" />
            Start onboarding
          </Link>
        </div>
      ) : (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
          {visibleAgents!.map((agent) => (
            <AgentCard
              key={agent.alias}
              agent={agent}
              toggling={toggling.has(agent.alias)}
              onToggle={() => handleToggle(agent)}
            />
          ))}
          {hiddenCount > 0 && (
            <Link
              to="/agents"
              className="rounded-2xl border p-5 flex flex-col items-center justify-center text-center transition-colors hover:opacity-90"
              style={{
                background: 'var(--pc-bg-surface)',
                borderColor: 'var(--pc-border)',
                borderStyle: 'dashed',
                color: 'var(--pc-text-muted)',
              }}
            >
              <Plus className="h-6 w-6 mb-2" style={{ color: 'var(--pc-accent)' }} />
              <p className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                {hiddenCount} more {hiddenCount === 1 ? 'agent' : 'agents'}
              </p>
              <p className="text-xs mt-1">View all on /agents</p>
            </Link>
          )}
        </div>
      )}
    </section>
  );
}
