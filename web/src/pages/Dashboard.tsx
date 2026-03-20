import { useState, useEffect } from 'react';
import {
  Cpu,
  Clock,
  Globe,
  Database,
  Activity,
  DollarSign,
  Radio,
} from 'lucide-react';
import type { StatusResponse, CostSummary } from '@/types/api';
import { getStatus, getCost } from '@/lib/api';
import { t } from '@/lib/i18n';

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

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showAllChannels, setShowAllChannels] = useState(false);

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

  const maxCost = Math.max(
    cost.session_cost_usd,
    cost.daily_cost_usd,
    cost.monthly_cost_usd,
    0.001
  );

  return (
    <div className="p-6 space-y-6 animate-fade-in">
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
              {t("dashboard.active_channels")}
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
          <div className="space-y-2">
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
    </div>
  );
}
