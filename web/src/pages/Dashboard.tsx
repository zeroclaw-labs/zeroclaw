import { useState, useEffect, useCallback } from 'react';
import {
  Cpu,
  Clock,
  Globe,
  Database,
  Activity,
  DollarSign,
  Radio,
  LayoutDashboard,
  Users,
  MessageSquare,
  ChevronRight,
  Wifi,
} from 'lucide-react';
import type { StatusResponse, CostSummary, Session, ChannelDetail } from '@/types/api';
import { getStatus, getCost, getSessions, getChannels } from '@/lib/api';
import { useSSE } from '@/hooks/useSSE';
import { t } from '@/lib/i18n';

type TabId = 'overview' | 'sessions' | 'channels';

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

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
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

const STATUS_CARDS = [
  {
    icon: Cpu,
    accent: "var(--pc-accent)",
    labelKey: "dashboard.provider_model",
    getValue: (s: StatusResponse) => s.provider ?? "Unknown",
    getSub: (s: StatusResponse) => s.model ?? "",
  },
  {
    icon: Clock,
    accent: "#34d399",
    labelKey: "dashboard.uptime",
    getValue: (s: StatusResponse) => formatUptime(s.uptime_seconds),
    getSub: () => t("dashboard.since_last_restart"),
  },
  {
    icon: Globe,
    accent: "#a78bfa",
    labelKey: "dashboard.gateway_port",
    getValue: (s: StatusResponse) => `:${s.gateway_port}`,
    getSub: () => "",
  },
  {
    icon: Database,
    accent: "#fbbf24",
    labelKey: "dashboard.memory_backend",
    getValue: (s: StatusResponse) => s.memory_backend,
    getSub: (s: StatusResponse) =>
      `${t("dashboard.paired")}: ${s.paired ? t("dashboard.paired_yes") : t("dashboard.paired_no")}`,
  },
];

const TABS: { id: TabId; labelKey: string; icon: typeof LayoutDashboard }[] = [
  { id: 'overview', labelKey: 'dashboard.tab_overview', icon: LayoutDashboard },
  { id: 'sessions', labelKey: 'dashboard.tab_sessions', icon: Users },
  { id: 'channels', labelKey: 'dashboard.tab_channels', icon: Wifi },
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
            <p className="text-lg font-semibold truncate capitalize" style={{ color: "var(--pc-text-primary)" }}>{getValue(status)}</p>
            <p className="text-sm truncate" style={{ color: "var(--pc-text-muted)" }}>{getSub(status)}</p>
          </div>
        ))}
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
                <div
                  key={name}
                  className="flex items-center justify-between py-2.5 px-3 rounded-xl transition-all"
                  style={{ background: "var(--pc-bg-elevated)" }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.background = "var(--pc-hover)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.background = "var(--pc-bg-elevated)";
                  }}
                >
                  <span
                    className="text-sm font-medium capitalize"
                    style={{ color: "var(--pc-text-primary)" }}
                  >
                    {name}
                  </span>
                  <div className="flex items-center gap-2">
                    <span
                      className="status-dot"
                      style={
                        active
                          ? {
                              background: "var(--color-status-success)",
                              boxShadow: "0 0 6px var(--color-status-success)",
                            }
                          : { background: "var(--pc-text-faint)" }
                      }
                    />
                    <span className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
                      {active ? t("dashboard.active") : t("dashboard.inactive")}
                    </span>
                  </div>
                </div>
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
          <div className="grid grid-cols-2 gap-3">
            {Object.entries(status.health.components).length === 0 ? (
              <p
                className="text-sm col-span-2"
                style={{ color: "var(--pc-text-faint)" }}
              >
                {t("dashboard.no_components")}
              </p>
            ) : (
              Object.entries(status.health.components).map(([name, comp]) => (
                <div
                  key={name}
                  className="rounded-2xl p-3 transition-all"
                  style={{
                    border: `1px solid ${healthBorder(comp.status)}`,
                    background: healthBg(comp.status),
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.transform = "scale(1.02)";
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.transform = "scale(1)";
                  }}
                >
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className="status-dot"
                      style={{
                        background: healthColor(comp.status),
                        boxShadow: `0 0 6px ${healthColor(comp.status)}`,
                      }}
                    />
                    <span
                      className="text-sm font-medium truncate capitalize"
                      style={{ color: "var(--pc-text-primary)" }}
                    >
                      {name}
                    </span>
                  </div>
                  <p className="text-xs capitalize" style={{ color: "var(--pc-text-muted)" }}>
                    {comp.status}
                  </p>
                  {comp.restart_count > 0 && (
                    <p
                      className="text-xs mt-1"
                      style={{ color: "var(--color-status-warning)" }}
                    >
                      {t("dashboard.restarts")}: {comp.restart_count}
                    </p>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </>
  );
}

// ---------------------------------------------------------------------------
// Sessions Tab
// ---------------------------------------------------------------------------

function SessionsTab() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);

  const { events } = useSSE({
    filterTypes: ['session_update', 'session_created', 'session_closed'],
    autoConnect: true,
  });

  const loadSessions = useCallback(() => {
    getSessions()
      .then((data) => {
        setSessions(Array.isArray(data) ? data : []);
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

  // React to SSE events for real-time updates
  useEffect(() => {
    if (events.length === 0) return;
    loadSessions();
  }, [events.length, loadSessions]);

  if (loading) {
    return (
      <div className="flex items-center justify-center h-48">
        <div className="flex items-center gap-3">
          <div
            className="h-6 w-6 border-2 rounded-full animate-spin"
            style={{ borderColor: "var(--pc-border)", borderTopColor: "var(--pc-accent)" }}
          />
          <span className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
            {t("dashboard.loading_sessions")}
          </span>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div
        className="rounded-2xl border p-4"
        style={{ background: "rgba(239, 68, 68, 0.08)", borderColor: "rgba(239, 68, 68, 0.2)", color: "#f87171" }}
      >
        {t("dashboard.load_sessions_error")}: {error}
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
      {/* Session List */}
      <div className="lg:col-span-2 card p-5 animate-slide-in-up">
        <div className="flex items-center gap-2 mb-5">
          <Users className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: "var(--pc-text-primary)" }}
          >
            {t("dashboard.sessions_title")}
          </h2>
          <span
            className="ml-auto text-xs font-mono px-2 py-0.5 rounded-full"
            style={{ background: "rgba(var(--pc-accent-rgb), 0.1)", color: "var(--pc-accent)" }}
          >
            {sessions.length}
          </span>
        </div>

        {sessions.length === 0 ? (
          <p className="text-sm py-8 text-center" style={{ color: "var(--pc-text-faint)" }}>
            {t("dashboard.no_sessions")}
          </p>
        ) : (
          <div className="space-y-2 overflow-y-auto max-h-96">
            {sessions.map((session) => (
              <button
                key={session.session_id}
                onClick={() => setSelectedSession(session)}
                className="w-full text-left flex items-center justify-between py-3 px-4 rounded-xl transition-all"
                style={{
                  background: selectedSession?.session_id === session.session_id
                    ? "rgba(var(--pc-accent-rgb), 0.08)"
                    : "var(--pc-bg-elevated)",
                  border: selectedSession?.session_id === session.session_id
                    ? "1px solid rgba(var(--pc-accent-rgb), 0.2)"
                    : "1px solid transparent",
                }}
                onMouseEnter={(e) => {
                  if (selectedSession?.session_id !== session.session_id) {
                    e.currentTarget.style.background = "var(--pc-hover)";
                  }
                }}
                onMouseLeave={(e) => {
                  if (selectedSession?.session_id !== session.session_id) {
                    e.currentTarget.style.background = "var(--pc-bg-elevated)";
                  }
                }}
              >
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className="text-sm font-medium font-mono truncate"
                      style={{ color: "var(--pc-text-primary)" }}
                    >
                      {session.session_id?.slice(0, 8) ?? '???'}...
                    </span>
                  </div>
                  <div className="flex items-center gap-3 text-xs" style={{ color: "var(--pc-text-muted)" }}>
                    <span className="flex items-center gap-1">
                      <MessageSquare className="h-3 w-3" />
                      {session.message_count}
                    </span>
                    <span>{formatRelative(session.last_activity)}</span>
                  </div>
                </div>
                <ChevronRight
                  className="h-4 w-4 flex-shrink-0"
                  style={{ color: "var(--pc-text-faint)" }}
                />
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Session Details Panel */}
      <div className="card p-5 animate-slide-in-up">
        <div className="flex items-center gap-2 mb-5">
          <Activity className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
          <h2
            className="text-sm font-semibold uppercase tracking-wider"
            style={{ color: "var(--pc-text-primary)" }}
          >
            {t("dashboard.session_details")}
          </h2>
        </div>

        {selectedSession ? (
          <div className="space-y-4">
            {[
              { label: t("dashboard.session_id"), value: selectedSession.session_id },
              { label: t("dashboard.session_started"), value: formatTime(selectedSession.created_at) },
              { label: t("dashboard.session_last_activity"), value: formatRelative(selectedSession.last_activity) },
              { label: t("dashboard.session_messages"), value: String(selectedSession.message_count) },
            ].map(({ label, value }) => (
              <div key={label}>
                <p className="text-xs uppercase tracking-wider mb-1" style={{ color: "var(--pc-text-faint)" }}>
                  {label}
                </p>
                <p
                  className="text-sm font-medium capitalize truncate"
                  style={{ color: "var(--pc-text-primary)" }}
                >
                  {value}
                </p>
              </div>
            ))}
          </div>
        ) : (
          <p className="text-sm py-8 text-center" style={{ color: "var(--pc-text-faint)" }}>
            {t("dashboard.session_history")}
          </p>
        )}
      </div>
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
        setChannels(Array.isArray(data) ? data : []);
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
        style={{ background: "rgba(239, 68, 68, 0.08)", borderColor: "rgba(239, 68, 68, 0.2)", color: "#f87171" }}
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
            <div className="flex items-center gap-3">
              <div
                className="p-2 rounded-2xl"
                style={{ background: `rgba(var(--pc-accent-rgb), 0.08)`, color: "var(--pc-accent)" }}
              >
                <Radio className="h-5 w-5" />
              </div>
              <div>
                <h3
                  className="text-sm font-semibold capitalize"
                  style={{ color: "var(--pc-text-primary)" }}
                >
                  {channel.name}
                </h3>
                <span className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
                  {channel.type}
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

          {/* Status Badge */}
          <div className="flex items-center gap-2 mb-3">
            <span
              className="text-[10px] uppercase font-medium px-2 py-0.5 rounded-full"
              style={{
                background: channel.status === 'active'
                  ? 'rgba(0, 230, 138, 0.1)'
                  : channel.status === 'error'
                    ? 'rgba(255, 68, 102, 0.1)'
                    : 'rgba(var(--pc-accent-rgb), 0.08)',
                color: channel.status === 'active'
                  ? '#34d399'
                  : channel.status === 'error'
                    ? '#f87171'
                    : 'var(--pc-text-muted)',
              }}
            >
              {channel.status}
            </span>
            <span
              className="text-[10px] uppercase font-medium px-2 py-0.5 rounded-full"
              style={{
                background: channel.enabled
                  ? 'rgba(0, 230, 138, 0.1)'
                  : 'rgba(255, 68, 102, 0.1)',
                color: channel.enabled ? '#34d399' : '#f87171',
              }}
            >
              {channel.enabled ? t("dashboard.channel_enabled") : t("dashboard.channel_disabled")}
            </span>
          </div>

          {/* Stats */}
          <div
            className="pt-3 border-t space-y-2"
            style={{ borderColor: "var(--pc-border)" }}
          >
            <div className="flex justify-between text-xs">
              <span style={{ color: "var(--pc-text-muted)" }}>{t("dashboard.health")}</span>
              <span className="capitalize" style={{ color: healthColor(channel.health) }}>
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

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showAllChannels, setShowAllChannels] = useState(false);
  const [activeTab, setActiveTab] = useState<TabId>('overview');

  useEffect(() => {
    Promise.all([getStatus(), getCost()])
      .then(([s, c]) => {
        setStatus(s);
        setCost(c);
      })
      .catch((err) => setError(err.message));
  }, []);

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: "rgba(239, 68, 68, 0.08)", borderColor: "rgba(239, 68, 68, 0.2)", color: "#f87171", }}>
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
      {/* Tab Navigation */}
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
    </div>
  );
}
