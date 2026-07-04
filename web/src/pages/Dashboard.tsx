import { useState, useEffect, useCallback, useMemo } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
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
  Monitor,
  ArrowRight,
} from "lucide-react";
import type {
  StatusResponse,
  CostSummary,
  Session,
  ChannelDetail,
  ChannelReadinessState,
  SessionMessageRow,
  ProcessStats,
  TuiEntry,
} from "@/types/api";
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
  getQuickstartState,
  getTuis,
  listProps,
} from "@/lib/api";
import { resolveModelToProviderType } from "@/lib/configuredModels";
import DoctorFixModal from "@/components/DoctorFixModal";

type CostWindow = "today" | "7d" | "30d" | "month" | "all";

/**
 * A component's `last_error` is sometimes a config path (e.g.
 * `agents.cronos.model_provider`) — the field whose misconfiguration is crashing
 * the supervisor. When we can parse a known config entity out of it, offer the
 * same inline "fix in place" modal the Doctor page uses (edit the entity, save,
 * no navigation). Returns null when nothing parseable is present (error shown as
 * plain text). Mirrors Doctor's `remediationTarget`.
 */
// Convert a `ConfigTab` label ("Providers", "Peer Groups") into the `?tab=`
// URL key. Mirrors the key derivation in Config.tsx `wireTabSpecs` so a deep
// link lands on the same tab the config page renders (shared convention).
function tabSlug(tabLabel: string): string {
  return tabLabel.toLowerCase().replace(/\s+/g, "-");
}

function healthFixTarget(
  err: string | null | undefined,
  tabForPath?: (path: string) => string | undefined,
): { prefix: string; entity: string; href: string } | null {
  if (!err) return null;
  // Anchor at the start (like Doctor's remediationTarget) so a config path is
  // only matched when the error IS one (e.g. "agents.cronos.model_provider"),
  // not when prose merely contains the word — "failed to load agents.json"
  // must NOT yield a bogus Fix target for entity `agents.json`.
  // agents.<alias>[.<field>] — edit the agent; deep-link the field's tab.
  const agent = err.match(/^\s*agents\.([a-z0-9_-]+)(?:\.([a-z0-9_.-]+))?/i);
  if (agent?.[1]) {
    const alias = agent[1];
    const field = agent[2] ?? "";
    // Deep-link the field's tab by reading its backend `ConfigTab` metadata
    // (via the entry index) rather than pattern-matching the field name, so a
    // re-tabbed or newly added field routes correctly with no change here.
    // Falls back to no tab when the entry/tab is unknown (older errors,
    // ungrouped fields, index not yet loaded).
    const tabLabel = field
      ? tabForPath?.(`agents.${alias}.${field}`)
      : undefined;
    const tab = tabLabel ? `?tab=${tabSlug(tabLabel)}` : "";
    return {
      prefix: `agents.${alias}`,
      entity: `agents.${alias}`,
      href: `/config/agents/${encodeURIComponent(alias)}${tab}`,
    };
  }
  // channels.<type>.<alias>
  const chan = err.match(/^\s*channels\.([a-z0-9_-]+)\.([a-z0-9_-]+)/i);
  if (chan?.[1] && chan[2]) {
    return {
      prefix: `channels.${chan[1]}.${chan[2]}`,
      entity: `${chan[1]}.${chan[2]}`,
      href: `/config/channels/${encodeURIComponent(chan[1])}/${encodeURIComponent(chan[2])}`,
    };
  }
  // providers.models.<type>.<alias>
  const prov = err.match(/^\s*providers\.models\.([a-z0-9_-]+)\.([a-z0-9_-]+)/i);
  if (prov?.[1] && prov[2]) {
    return {
      prefix: `providers.models.${prov[1]}.${prov[2]}`,
      entity: `${prov[1]}.${prov[2]}`,
      href: `/config/providers.models/${encodeURIComponent(prov[1])}/${encodeURIComponent(prov[2])}`,
    };
  }
  return null;
}

function costWindowBounds(window: CostWindow): { from?: Date; to?: Date } {
  const now = new Date();
  switch (window) {
    case "today": {
      const start = new Date(now);
      start.setHours(0, 0, 0, 0);
      const end = new Date(start);
      end.setDate(end.getDate() + 1);
      return { from: start, to: end };
    }
    case "7d": {
      const from = new Date(now);
      from.setDate(from.getDate() - 7);
      return { from };
    }
    case "30d": {
      const from = new Date(now);
      from.setDate(from.getDate() - 30);
      return { from };
    }
    case "month": {
      const from = new Date(now.getFullYear(), now.getMonth(), 1, 0, 0, 0, 0);
      const to = new Date(now.getFullYear(), now.getMonth() + 1, 1, 0, 0, 0, 0);
      return { from, to };
    }
    case "all":
    default:
      return {};
  }
}
import type { MemoryEntry } from "@/types/api";
import {
  loadAgentSummaries,
  toggleAgentEnabled,
  type AgentSummary,
} from "@/lib/agents";
import AgentCard from "@/components/AgentCard";
import AgentDrawer from "@/components/AgentDrawer";
import EntityLink from "@/components/EntityLink";
import EntityEnabledToggle from "@/components/EntityEnabledToggle";
import { useSSE } from "@/hooks/useSSE";
import { usePolling } from "@/hooks/usePolling";
import { t } from "@/lib/i18n";
import { StatCard, PageHeader, ConfirmDialog } from "@/components/ui";

type TabId =
  | "overview"
  | "sessions"
  | "channels"
  | "memories"
  | "health"
  | "cost";

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
  if (!Number.isFinite(bytes) || bytes <= 0) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
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
          style={{
            background: "rgba(var(--pc-accent-rgb), 0.08)",
            color: "#fbbf24",
          }}
        >
          <MemoryStick className="h-5 w-5" />
        </div>
        <span
          className="text-xs uppercase tracking-wider font-medium"
          style={{ color: "var(--pc-text-muted)" }}
        >
          {t("dashboard.ram.label")}
        </span>
      </div>
      <p
        className="text-lg font-semibold truncate"
        style={{ color: "var(--pc-text-primary)" }}
      >
        {supported ? formatBytes(process!.rss_bytes) : "—"}
      </p>
      <p className="text-sm truncate" style={{ color: "var(--pc-text-muted)" }}>
        {pct !== null
          ? `${pct.toFixed(pct < 1 ? 2 : 1)}% ${t("dashboard.ram.of")} ${formatBytes(process!.system_ram_total_bytes)}`
          : supported
            ? t("dashboard.ram.resident")
            : t("dashboard.ram.unsupported")}
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
          style={{
            background: "rgba(var(--pc-accent-rgb), 0.08)",
            color: "#a78bfa",
          }}
        >
          <Cpu className="h-5 w-5" />
        </div>
        <span
          className="text-xs uppercase tracking-wider font-medium"
          style={{ color: "var(--pc-text-muted)" }}
        >
          {t("dashboard.cpu.label")}
        </span>
      </div>
      <p
        className="text-lg font-semibold truncate"
        style={{ color: "var(--pc-text-primary)" }}
      >
        {supported ? `${pct.toFixed(1)}%` : "—"}
      </p>
      <p className="text-sm truncate" style={{ color: "var(--pc-text-muted)" }}>
        {supported
          ? ncpu > 0
            ? `${ncpu} ${t("dashboard.cpu.cores")} · ${(pct / ncpu).toFixed(1)}% ${t("dashboard.cpu.normalized")}`
            : t("dashboard.cpu.across_all_cores")
          : t("dashboard.cpu.unsupported")}
      </p>
    </div>
  );
}

function formatLocalDateTime(iso: string): string {
  try {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString(undefined, {
      year: "numeric",
      month: "short",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return iso;
  }
}

function formatRelative(iso: string): string {
  try {
    const diff = Date.now() - new Date(iso).getTime();
    const seconds = Math.floor(diff / 1000);
    if (seconds < 60) return `${seconds}${t("dashboard.rel.seconds_ago")}`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}${t("dashboard.rel.minutes_ago")}`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}${t("dashboard.rel.hours_ago")}`;
    const days = Math.floor(hours / 24);
    return `${days}${t("dashboard.rel.days_ago")}`;
  } catch {
    return iso;
  }
}

function healthColor(status: string): string {
  switch (status.toLowerCase()) {
    case "ok":
    case "healthy":
      return "var(--color-status-success)";
    case "warn":
    case "warning":
    case "degraded":
      return "var(--color-status-warning)";
    default:
      return "var(--color-status-error)";
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case "ok":
    case "healthy":
      return "rgba(0, 230, 138, 0.2)";
    case "warn":
    case "warning":
    case "degraded":
      return "rgba(255, 170, 0, 0.2)";
    default:
      return "rgba(255, 68, 102, 0.2)";
  }
}

function healthBg(status: string): string {
  switch (status.toLowerCase()) {
    case "ok":
    case "healthy":
      return "rgba(0, 230, 138, 0.05)";
    case "warn":
    case "warning":
    case "degraded":
      return "rgba(255, 170, 0, 0.05)";
    default:
      return "rgba(255, 68, 102, 0.05)";
  }
}

function readinessColor(state: ChannelReadinessState): string {
  switch (state) {
    case 'ready':
      return 'var(--color-status-success)';
    case 'missing':
      return 'var(--color-status-error)';
    case 'unknown':
      return 'var(--pc-text-muted)';
  }
}

function readinessLabel(state: ChannelReadinessState): string {
  switch (state) {
    case 'ready':
      return t('dashboard.readiness.ready');
    case 'missing':
      return t('dashboard.readiness.missing');
    case 'unknown':
      return t('dashboard.readiness.not_checked');
  }
}

// Label keys resolved at render; the second tuple element is the readiness
// field the row reads.
const CHANNEL_READINESS_ROWS: Array<[
  string,
  'enabled' | 'bound_to_agent' | 'authenticated' | 'listening',
]> = [
  ['dashboard.readiness.enabled', 'enabled'],
  ['dashboard.readiness.agent', 'bound_to_agent'],
  ['dashboard.readiness.authenticated', 'authenticated'],
  ['dashboard.readiness.listening', 'listening'],
];

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
  { id: "overview", labelKey: "dashboard.tab_overview", icon: LayoutDashboard },
  { id: "sessions", labelKey: "dashboard.tab_sessions", icon: Users },
  { id: "channels", labelKey: "dashboard.tab_channels", icon: Wifi },
  { id: "memories", labelKey: "dashboard.tab_memories", icon: Brain },
  { id: "health", labelKey: "dashboard.tab_health", icon: Heart },
  { id: "cost", labelKey: "dashboard.tab_cost", icon: DollarSign },
];

// ---------------------------------------------------------------------------
// Overview Tab (existing dashboard content)
// ---------------------------------------------------------------------------

function OverviewTab({
  status,
  cost,
  tuis,
  showAllChannels,
  setShowAllChannels,
}: {
  status: StatusResponse;
  cost: CostSummary;
  tuis: TuiEntry[];
  showAllChannels: boolean;
  setShowAllChannels: (fn: (v: boolean) => boolean) => void;
}) {
  const maxCost = Math.max(
    cost.session_cost_usd,
    cost.daily_cost_usd,
    cost.monthly_cost_usd,
    0.001,
  );

  // Component Health → "fix in place" modal target (set when an error row's
  // last_error parses to a config entity). Same modal as the Doctor page.
  const [healthFix, setHealthFix] = useState<{
    prefix: string;
    entity: string;
    href: string;
  } | null>(null);

  // Index of agent config field path -> ConfigTab label, so a health "Fix"
  // deep-link routes to the field's real tab from backend metadata instead of
  // guessing it from the field name. Best-effort: an empty index just omits the
  // ?tab= and lands on the agent's default tab.
  const [fieldTabs, setFieldTabs] = useState<Map<string, string>>(new Map());
  useEffect(() => {
    let cancelled = false;
    void listProps("agents")
      .then((resp) => {
        if (cancelled) return;
        const index = new Map<string, string>();
        for (const e of resp.entries) {
          if (e.tab) index.set(e.path, e.tab);
        }
        setFieldTabs(index);
      })
      .catch(() => {
        /* deep-link tab is best-effort; ignore load failures */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <>
      {/* Status Cards Grid */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {STATUS_CARDS.map(
          ({ icon: Icon, accent, labelKey, getValue, getSub }) => (
            <div key={labelKey} className="card p-5 animate-slide-in-up">
              <div className="flex items-center gap-3 mb-3">
                <div
                  className="p-2 rounded-2xl"
                  style={{
                    background: `rgba(var(--pc-accent-rgb), 0.08)`,
                    color: accent,
                  }}
                >
                  <Icon className="h-5 w-5" />
                </div>
                <span
                  className="text-xs uppercase tracking-wider font-medium"
                  style={{ color: "var(--pc-text-muted)" }}
                >
                  {t(labelKey)}
                </span>
              </div>
              <p
                className="text-lg font-semibold truncate"
                style={{ color: "var(--pc-text-primary)" }}
              >
                {getValue(status)}
              </p>
              <p
                className="text-sm truncate"
                style={{ color: "var(--pc-text-muted)" }}
              >
                {getSub(status)}
              </p>
            </div>
          ),
        )}
        <ProcessRamCard process={status.process} />
        <ProcessCpuCard process={status.process} />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6 stagger-children">
        {/* Cost Widget */}
        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <DollarSign
              className="h-5 w-5"
              style={{ color: "var(--pc-accent)" }}
            />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.cost_overview")}
            </h2>
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
            <span
              className="font-mono"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {cost.total_tokens.toLocaleString()}
            </span>
          </div>
          <div className="flex justify-between text-sm mt-1">
            <span style={{ color: "var(--pc-text-muted)" }}>
              {t("dashboard.requests_label")}
            </span>
            <span
              className="font-mono"
              style={{ color: "var(--pc-text-primary)" }}
            >
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
            ) : (
              (() => {
                const entries = Object.entries(status.channels).filter(
                  ([, active]) => showAllChannels || active,
                );
                if (entries.length === 0) {
                  return (
                    <p
                      className="text-sm"
                      style={{ color: "var(--pc-text-faint)" }}
                    >
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
                    style={{ background: "var(--pc-bg-elevated)" }}
                    title={`${t("dashboard.open_config_prefix")}channels.${name}${t("dashboard.open_config_suffix")}`}
                  >
                    <span
                      className="text-sm font-mono font-medium"
                      style={{ color: "var(--pc-text-primary)" }}
                    >
                      {name}
                    </span>
                    <span className="flex items-center gap-2">
                      <span
                        className="status-dot"
                        style={
                          active
                            ? {
                                background: "var(--color-status-success)",
                                boxShadow:
                                  "0 0 6px var(--color-status-success)",
                              }
                            : { background: "var(--pc-text-faint)" }
                        }
                      />
                      <span
                        className="text-xs"
                        style={{ color: "var(--pc-text-muted)" }}
                      >
                        {active
                          ? t("dashboard.active")
                          : t("dashboard.inactive")}
                      </span>
                    </span>
                  </EntityLink>
                ));
              })()
            )}
          </div>
        </div>

        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Activity
              className="h-5 w-5"
              style={{ color: "var(--pc-accent)" }}
            />
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
              ([name]) => !name.startsWith("channel:"),
            );
            if (entries.length === 0) {
              return (
                <p
                  className="text-sm"
                  style={{ color: "var(--pc-text-faint)" }}
                >
                  {t("dashboard.no_components")}
                </p>
              );
            }
            const sorted = entries
              .slice()
              .sort((a, b) => a[0].localeCompare(b[0]));
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
                            background: "transparent",
                            border: `1px solid ${healthBorder(comp.status)}`,
                          }}
                        >
                          {comp.status}
                        </span>
                      </div>
                      {lastErr ? (
                        (() => {
                          const fix = healthFixTarget(lastErr, (p) =>
                            fieldTabs.get(p),
                          );
                          return (
                            <div className="mt-1 flex items-start gap-2">
                              <p
                                className="flex-1 text-[11px] font-mono break-words"
                                style={{ color: "var(--color-status-error)" }}
                                title={lastErr}
                              >
                                ⚠{" "}
                                {lastErr.length > 120
                                  ? lastErr.slice(0, 117) + "…"
                                  : lastErr}
                              </p>
                              {fix && (
                                <button
                                  type="button"
                                  onClick={() => setHealthFix(fix)}
                                  className="inline-flex h-6 flex-shrink-0 items-center gap-1 rounded-[var(--radius-md)] border border-pc-border bg-transparent px-2 text-[11px] font-medium text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base cursor-pointer"
                                >
                                  {t("dashboard.fix")}
                                  <ArrowRight className="h-3 w-3" />
                                </button>
                              )}
                            </div>
                          );
                        })()
                      ) : null}
                      <div
                        className="flex items-center gap-3 text-[11px] mt-0.5"
                        style={{ color: "var(--pc-text-muted)" }}
                      >
                        {lastOk && (
                          <span title={`${t("dashboard.last_ok_title")} ${lastOk}`}>
                            {t("dashboard.ok_prefix")} {formatRelative(lastOk)}
                          </span>
                        )}
                        {comp.restart_count > 0 && (
                          <span
                            style={{ color: "var(--color-status-warning)" }}
                          >
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

      {/* Connected TUIs */}
      {tuis.length > 0 && (
        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Monitor
              className="h-5 w-5"
              style={{ color: "var(--pc-accent)" }}
            />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.connected_tuis")}
            </h2>
            <span
              className="text-xs font-mono px-2 py-0.5 rounded-full"
              style={{
                background: "rgba(var(--pc-accent-rgb), 0.1)",
                color: "var(--pc-accent)",
              }}
            >
              {tuis.length}
            </span>
          </div>
          <div className="space-y-2 overflow-y-auto max-h-48 pr-1">
            {tuis.map((tui) => (
              <div
                key={tui.tui_id}
                className="flex items-center justify-between py-2.5 px-3 rounded-xl"
                style={{ background: "var(--pc-bg-elevated)" }}
              >
                <div className="flex items-center gap-2">
                  <span
                    className="status-dot flex-shrink-0"
                    style={{
                      background: "var(--color-status-success)",
                      boxShadow: "0 0 6px var(--color-status-success)",
                    }}
                  />
                  <span
                    className="text-sm font-mono font-medium"
                    style={{ color: "var(--pc-text-primary)" }}
                  >
                    {tui.tui_id}
                  </span>
                  <span
                    className="text-xs font-mono px-1.5 py-0.5 rounded"
                    style={{
                      background: "rgba(var(--pc-accent-rgb), 0.08)",
                      color: "var(--pc-text-muted)",
                    }}
                  >
                    {tui.peer_label || tui.transport || t("dashboard.unknown")}
                  </span>
                </div>
                <span
                  className="text-xs"
                  style={{ color: "var(--pc-text-muted)" }}
                  title={tui.connected_at}
                >
                  {formatRelative(tui.connected_at)}
                </span>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Component Health "fix in place" modal — same editor as the Doctor page.
          Opens when an error row with a parseable config entity is actioned. */}
      <DoctorFixModal
        open={healthFix !== null}
        prefix={healthFix?.prefix ?? ""}
        entity={healthFix?.entity ?? ""}
        href={healthFix?.href ?? ""}
        onClose={() => setHealthFix(null)}
      />
    </>
  );
}

// ---------------------------------------------------------------------------
// Sessions Tab
// ---------------------------------------------------------------------------

type SessionSort =
  | "activity-desc"
  | "activity-asc"
  | "created-desc"
  | "created-asc"
  | "messages-desc"
  | "messages-asc";

const SESSION_SORT_OPTIONS: { value: SessionSort; labelKey: string }[] = [
  { value: "activity-desc", labelKey: "dashboard.sort.recent_activity" },
  { value: "activity-asc", labelKey: "dashboard.sort.oldest_activity" },
  { value: "created-desc", labelKey: "dashboard.sort.newest_first" },
  { value: "created-asc", labelKey: "dashboard.sort.oldest_first" },
  { value: "messages-desc", labelKey: "dashboard.sort.busiest" },
  { value: "messages-asc", labelKey: "dashboard.sort.quietest" },
];

function isSessionSort(v: string): v is SessionSort {
  return SESSION_SORT_OPTIONS.some((o) => o.value === v);
}

function SessionsTab() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchParams, setSearchParams] = useSearchParams();
  const agentFilter = searchParams.get("agent") ?? "";
  const channelFilter = searchParams.get("channel") ?? "";
  const searchQuery = searchParams.get("q") ?? "";
  const sortRaw = searchParams.get("sort") ?? "";
  const sortBy: SessionSort = isSessionSort(sortRaw)
    ? sortRaw
    : "activity-desc";
  const setFilter = (key: "agent" | "channel" | "q" | "sort", value: string) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (value) next.set(key, value);
        else next.delete(key);
        return next;
      },
      { replace: true },
    );
  const setAgentFilter = (v: string) => setFilter("agent", v);
  const setChannelFilter = (v: string) => setFilter("channel", v);
  const setSearchQuery = (v: string) => setFilter("q", v);
  const setSortBy = (v: SessionSort) =>
    setFilter("sort", v === "activity-desc" ? "" : v);
  const [inspect, setInspect] = useState<{
    session: Session;
    messages: SessionMessageRow[] | null;
    error: string | null;
  } | null>(null);
  const [inspectNewestFirst, setInspectNewestFirst] = useState(true);
  const [deleting, setDeleting] = useState<string | null>(null);
  // The session queued for deletion; non-null opens the confirm dialog.
  const [pendingDelete, setPendingDelete] = useState<Session | null>(null);

  const { events } = useSSE({
    filterTypes: ["session_update", "session_created", "session_closed"],
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
          s.name ?? "",
          s.agent_alias ?? "",
          s.channel_id ?? "",
        ]
          .join(" ")
          .toLowerCase();
        if (!haystack.includes(needle)) return false;
      }
      return true;
    });
    const sorted = [...filtered];
    sorted.sort((a, b) => {
      switch (sortBy) {
        case "activity-asc":
          return a.last_activity.localeCompare(b.last_activity);
        case "created-desc":
          return b.created_at.localeCompare(a.created_at);
        case "created-asc":
          return a.created_at.localeCompare(b.created_at);
        case "messages-desc":
          return b.message_count - a.message_count;
        case "messages-asc":
          return a.message_count - b.message_count;
        case "activity-desc":
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

  // Runs once the operator confirms in the dialog; the destructive intent is
  // gated by ConfirmDialog rather than the native window.confirm.
  const handleDelete = async (session: Session) => {
    if (deleting) return;
    setPendingDelete(null);
    setDeleting(session.session_key);
    try {
      await deleteSession(session.session_key);
      setSessions((prev) =>
        prev.filter((s) => s.session_key !== session.session_key),
      );
      if (inspect?.session.session_key === session.session_key)
        setInspect(null);
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
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
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
        style={{
          background: "var(--color-status-error-alpha-08)",
          borderColor: "var(--color-status-error-alpha-20)",
          color: "var(--color-status-error)",
        }}
      >
        {t("dashboard.load_sessions_error")}: {error}
      </div>
    );
  }

  return (
    <div className="card p-5 animate-slide-in-up space-y-4">
      <div className="flex items-center gap-2 flex-wrap">
        <Users className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: "var(--pc-text-primary)" }}
        >
          {t("dashboard.sessions_title")}
        </h2>
        <span
          className="text-xs font-mono px-2 py-0.5 rounded-full"
          style={{
            background: "rgba(var(--pc-accent-rgb), 0.1)",
            color: "var(--pc-accent)",
          }}
        >
          {visible.length}
          {visible.length !== sessions.length ? ` / ${sessions.length}` : ""}
        </span>

        <div className="ml-auto flex items-center gap-2 flex-wrap">
          <div className="relative">
            <Search
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <input
              type="search"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder={t("dashboard.search_placeholder")}
              className="input-electric pl-7 pr-2 py-1 text-xs w-40"
              title={t("dashboard.session_search_title")}
              aria-label={t("dashboard.session_search_aria")}
            />
          </div>
          <div className="relative">
            <ArrowUpDown
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={sortBy}
              onChange={(e) => setSortBy(e.target.value as SessionSort)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.session_sort_title")}
              aria-label={t("dashboard.session_sort_title")}
            >
              {SESSION_SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {t(o.labelKey)}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Bot
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={agentFilter}
              onChange={(e) => setAgentFilter(e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.filter_agent_title")}
            >
              <option value="">{t("dashboard.all_agents")}</option>
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
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={channelFilter}
              onChange={(e) => setChannelFilter(e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.filter_channel_title")}
            >
              <option value="">{t("dashboard.all_channels")}</option>
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
        <p
          className="text-sm py-8 text-center"
          style={{ color: "var(--pc-text-faint)" }}
        >
          {sessions.length === 0
            ? t("dashboard.no_sessions")
            : t("dashboard.no_sessions_match")}
        </p>
      ) : (
        <div className="space-y-2 overflow-y-auto max-h-[32rem]">
          {visible.map((session) => (
            <div
              key={session.session_key}
              className="flex items-center justify-between py-3 px-4 rounded-xl"
              style={{
                background: "var(--pc-bg-elevated)",
                border: "1px solid transparent",
              }}
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-start gap-2 mb-1 flex-wrap">
                  <span
                    className="text-sm font-medium font-mono break-all"
                    style={{ color: "var(--pc-text-primary)" }}
                  >
                    {session.session_id}
                  </span>
                  {session.agent_alias && (
                    <EntityLink
                      kind="agent"
                      id={session.agent_alias}
                      className="text-[10px] font-medium px-2 py-0.5 rounded-full flex-shrink-0 hover:underline"
                      style={{
                        background: "rgba(var(--pc-accent-rgb), 0.10)",
                        color: "var(--pc-accent-light)",
                      }}
                      title={`${t("dashboard.open_config_prefix")}agents.${session.agent_alias}${t("dashboard.open_config_suffix")}`}
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
                        background: "rgba(167, 139, 250, 0.10)",
                        color: "#a78bfa",
                      }}
                      title={`${t("dashboard.open_config_prefix")}channels.${session.channel_id}${t("dashboard.open_config_suffix")}`}
                    >
                      {session.channel_id}
                    </EntityLink>
                  )}
                </div>
                <div
                  className="flex items-center gap-3 text-xs"
                  style={{ color: "var(--pc-text-muted)" }}
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
                  title={t("dashboard.view_messages")}
                  style={{ color: "var(--pc-text-muted)" }}
                >
                  <Eye className="h-4 w-4" />
                </button>
                <button
                  type="button"
                  onClick={() => setPendingDelete(session)}
                  disabled={deleting === session.session_key}
                  className="p-1.5 rounded-lg hover:bg-[var(--pc-hover)] disabled:opacity-50"
                  title={t("dashboard.delete_session")}
                  style={{ color: "var(--color-status-error)" }}
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
          style={{ background: "rgba(0,0,0,0.5)" }}
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
                  style={{ color: "var(--pc-text-faint)" }}
                >
                  {t("dashboard.session_label")}
                </p>
                <p
                  className="text-sm font-mono break-all"
                  style={{ color: "var(--pc-text-primary)" }}
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
                        background: "rgba(var(--pc-accent-rgb), 0.10)",
                        color: "var(--pc-accent-light)",
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
                        background: "rgba(167, 139, 250, 0.10)",
                        color: "#a78bfa",
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
                      color: "var(--pc-text-muted)",
                      borderColor: "var(--pc-border)",
                    }}
                    title={t("dashboard.flip_transcript")}
                  >
                    {inspectNewestFirst
                      ? t("dashboard.newest_first_short")
                      : t("dashboard.oldest_first_short")}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setInspect(null)}
                  className="p-1 rounded-lg hover:bg-[var(--pc-hover)]"
                  style={{ color: "var(--pc-text-muted)" }}
                  title={t("common.close")}
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
            </div>
            <div className="flex-1 overflow-y-auto space-y-3 pr-1">
              {inspect.error ? (
                <p
                  className="text-sm"
                  style={{ color: "var(--color-status-error)" }}
                >
                  {inspect.error}
                </p>
              ) : inspect.messages === null ? (
                <p
                  className="text-sm"
                  style={{ color: "var(--pc-text-muted)" }}
                >
                  {t("dashboard.loading_transcript")}
                </p>
              ) : inspect.messages.length === 0 ? (
                <p
                  className="text-sm"
                  style={{ color: "var(--pc-text-faint)" }}
                >
                  {t("dashboard.no_persisted_messages")}
                </p>
              ) : (
                (inspectNewestFirst
                  ? inspect.messages.slice().reverse()
                  : inspect.messages
                ).map((m, i) => (
                  <div
                    key={i}
                    className="rounded-xl px-3 py-2"
                    style={{ background: "var(--pc-bg-elevated)" }}
                  >
                    <div className="flex items-baseline justify-between gap-3 mb-1">
                      <p
                        className="text-[10px] uppercase tracking-wider font-mono"
                        style={{ color: "var(--pc-text-faint)" }}
                      >
                        {m.role}
                      </p>
                      {m.created_at && (
                        <p
                          className="text-[10px] font-mono whitespace-nowrap"
                          style={{ color: "var(--pc-text-faint)" }}
                          title={m.created_at}
                        >
                          {formatLocalDateTime(m.created_at)}
                        </p>
                      )}
                    </div>
                    <p
                      className="text-sm whitespace-pre-wrap break-words"
                      style={{ color: "var(--pc-text-primary)" }}
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

      <ConfirmDialog
        open={pendingDelete !== null}
        danger
        title={t("common.delete")}
        message={`${t("dashboard.confirm_delete_session_prefix")} ${pendingDelete?.session_id ?? ""}${t("dashboard.confirm_delete_suffix")}`}
        confirmLabel={t("common.delete")}
        onConfirm={() => {
          // Close the dialog first (capturing the target): a confirm clicked
          // while an earlier delete is still in flight would otherwise hit
          // handleDelete's `if (deleting) return` and never clear pendingDelete,
          // leaving the dialog stuck open.
          const target = pendingDelete;
          setPendingDelete(null);
          if (target) void handleDelete(target);
        }}
        onClose={() => setPendingDelete(null)}
      />
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
    filterTypes: ["channel_update", "channel_status"],
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
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
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
        style={{
          background: "var(--color-status-error-alpha-08)",
          borderColor: "var(--color-status-error-alpha-20)",
          color: "var(--color-status-error)",
        }}
      >
        {t("dashboard.load_channels_error")}: {error}
      </div>
    );
  }

  if (channels.length === 0) {
    return (
      <div className="card p-5 animate-slide-in-up">
        <p
          className="text-sm py-8 text-center"
          style={{ color: "var(--pc-text-faint)" }}
        >
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
                style={{
                  background: `rgba(var(--pc-accent-rgb), 0.08)`,
                  color: "var(--pc-accent)",
                }}
              >
                <Radio className="h-5 w-5" />
              </div>
              <div className="min-w-0">
                <EntityLink
                  kind="channel"
                  id={channel.name}
                  className="text-sm font-semibold font-mono break-all hover:underline"
                  title={`${t("dashboard.open_config_prefix")}channels.${channel.name}${t("dashboard.open_config_suffix")}`}
                >
                  <span style={{ color: "var(--pc-text-primary)" }}>
                    {channel.name}
                  </span>
                </EntityLink>
                <span
                  className="text-xs block"
                  style={{ color: "var(--pc-text-muted)" }}
                >
                  {channel.owning_agent ? (
                    <>
                      {t("dashboard.owned_by")}{" "}
                      <EntityLink
                        kind="agent"
                        id={channel.owning_agent}
                        className="hover:underline font-mono"
                        title={`${t("dashboard.open_config_prefix")}agents.${channel.owning_agent}${t("dashboard.open_config_suffix")}`}
                      >
                        {channel.owning_agent}
                      </EntityLink>
                    </>
                  ) : (
                    t("dashboard.no_owning_agent")
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
              onChange={() => loadChannels()}
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
            {channel.readiness ? (
              <>
                {CHANNEL_READINESS_ROWS.map(([labelKey, key]) => {
                  const value = channel.readiness?.[key];
                  return value ? (
                    <div key={key} className="flex justify-between gap-3 text-xs">
                      <span style={{ color: "var(--pc-text-muted)" }}>{t(labelKey)}</span>
                      <span style={{ color: readinessColor(value) }}>
                        {readinessLabel(value)}
                      </span>
                    </div>
                  ) : null;
                })}
              </>
            ) : null}
            <div className="flex justify-between text-xs">
              <span style={{ color: "var(--pc-text-muted)" }}>
                {t("dashboard.health")}
              </span>
              <span style={{ color: healthColor(channel.health) }}>
                {channel.health}
              </span>
            </div>
            {channel.readiness?.requirements?.length ? (
              <div className="pt-2 space-y-1">
                {channel.readiness.requirements.map((requirement) => (
                  <p
                    key={requirement}
                    className="text-xs leading-snug"
                    style={{ color: "var(--color-status-warning)" }}
                  >
                    {requirement}
                  </p>
                ))}
              </div>
            ) : null}
            {channel.readiness?.notes?.length ? (
              <div className="pt-2 space-y-1">
                {channel.readiness.notes.map((note) => (
                  <p
                    key={note}
                    className="text-xs leading-snug"
                    style={{ color: "var(--pc-text-muted)" }}
                  >
                    {note}
                  </p>
                ))}
              </div>
            ) : null}
          </div>
        </div>
      ))}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Main Dashboard Component
// ---------------------------------------------------------------------------

const TAB_IDS: TabId[] = [
  "overview",
  "sessions",
  "channels",
  "memories",
  "health",
  "cost",
];

function parseTab(raw: string | null): TabId {
  if (raw && (TAB_IDS as string[]).includes(raw)) return raw as TabId;
  return "overview";
}

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [tuis, setTuis] = useState<TuiEntry[]>([]);
  const [costWindow, setCostWindow] = useState<CostWindow>("today");
  const [error, setError] = useState<string | null>(null);
  const [showAllChannels, setShowAllChannels] = useState(false);
  const [searchParams, setSearchParams] = useSearchParams();
  const activeTab = parseTab(searchParams.get("tab"));
  const setActiveTab = (id: TabId) => {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (id === "overview") next.delete("tab");
        else next.set("tab", id);
        // Filters belong to specific tabs; drop them when leaving so deep
        // links don't drag a stale agent= into the wrong tab.
        if (id !== "sessions" && id !== "memories") {
          next.delete("agent");
        }
        if (id !== "sessions") {
          next.delete("channel");
        }
        if (id !== "memories") {
          next.delete("category");
        }
        return next;
      },
      { replace: true },
    );
  };

  // Uptime ticks every second on the server; poll every 5s so the tile and
  // health badges stay live — but only while the tab is visible (paused when
  // backgrounded), and re-armed when the cost window changes.
  usePolling(
    (isStale) => {
      const { from, to } = costWindowBounds(costWindow);
      Promise.all([getStatus(), getCost(from, to), getTuis()])
        .then(([s, c, t]) => {
          if (isStale()) return;
          setStatus(s);
          setCost(c);
          setTuis(t);
        })
        .catch((err) => {
          if (!isStale()) setError(err.message);
        });
    },
    5000,
    [costWindow],
  );

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div
          className="rounded-2xl border p-4"
          style={{
            background: "var(--color-status-error-alpha-08)",
            borderColor: "var(--color-status-error-alpha-20)",
            color: "var(--color-status-error)",
          }}
        >
          {t("dashboard.load_error")}: {error}
        </div>
      </div>
    );
  }

  if (!status || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: "var(--pc-border)",
            borderTopColor: "var(--pc-accent)",
          }}
        />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <AgentsSection />

      {/* Global system stats — tab navigation. Scrolls horizontally when the
          six tabs don't fit (mobile) instead of overflowing the frame; each
          button keeps its size (flex-shrink-0) so labels never get clipped. */}
      <div
        className="flex items-center gap-1 p-1 rounded-2xl overflow-x-auto"
        style={{ background: "var(--pc-bg-elevated)" }}
        role="tablist"
        aria-label={t("nav.dashboard")}
      >
        {TABS.map(({ id, labelKey, icon: Icon }) => (
          <button
            key={id}
            role="tab"
            aria-selected={activeTab === id}
            onClick={() => setActiveTab(id)}
            className="flex flex-shrink-0 items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all whitespace-nowrap"
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
      {activeTab === "overview" && (
        <OverviewTab
          status={status}
          cost={cost}
          tuis={tuis}
          showAllChannels={showAllChannels}
          setShowAllChannels={setShowAllChannels}
        />
      )}
      {activeTab === "sessions" && <SessionsTab />}
      {activeTab === "channels" && <ChannelsTab />}
      {activeTab === "memories" && <MemoriesTab />}
      {activeTab === "health" && <HealthTab status={status} />}
      {activeTab === "cost" && (
        <CostTab
          cost={cost}
          window={costWindow}
          onWindowChange={setCostWindow}
        />
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
    ([name]) => !name.startsWith("channel:"),
  );
  if (entries.length === 0) {
    return (
      <div className="card p-5 animate-slide-in-up">
        <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
          {t("dashboard.no_components")}
        </p>
      </div>
    );
  }
  const sorted = entries.slice().sort((a, b) => a[0].localeCompare(b[0]));
  return (
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
                  style={{ color: "var(--pc-text-primary)" }}
                >
                  {name}
                </span>
                <span
                  className="ml-auto text-[10px] uppercase font-medium px-1.5 py-0.5 rounded-full flex-shrink-0"
                  style={{
                    color: healthColor(comp.status),
                    background: "transparent",
                    border: `1px solid ${healthBorder(comp.status)}`,
                  }}
                >
                  {comp.status}
                </span>
              </div>
              {lastErr && (
                <p
                  className="text-[11px] mt-1 font-mono break-words"
                  style={{ color: "var(--color-status-error)" }}
                  title={lastErr}
                >
                  ⚠ {lastErr}
                </p>
              )}
              <div
                className="flex items-center gap-3 text-[11px] mt-0.5"
                style={{ color: "var(--pc-text-muted)" }}
              >
                {lastOk && (
                  <span title={`${t("dashboard.last_ok_title")} ${lastOk}`}>
                    {t("dashboard.ok_prefix")} {formatRelative(lastOk)}
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
    </div>
  );
}

// ---------------------------------------------------------------------------
// Cost Tab — full by-model + by-agent rollup
// ---------------------------------------------------------------------------

// Cost dashboard: per-day totals plus per-agent and per-model rollups
// with input / output / cached token splits. Both rollups are daily-scoped
const COST_WINDOW_OPTIONS: { value: CostWindow; labelKey: string }[] = [
  { value: "today", labelKey: "dashboard.cost.today" },
  { value: "7d", labelKey: "dashboard.cost.last_7_days" },
  { value: "30d", labelKey: "dashboard.cost.last_30_days" },
  { value: "month", labelKey: "dashboard.cost.this_month" },
  { value: "all", labelKey: "dashboard.cost.all_time" },
];

function CostTab({
  cost,
  window: costWindow,
  onWindowChange,
}: {
  cost: CostSummary;
  window: CostWindow;
  onWindowChange: (next: CostWindow) => void;
}) {
  const byModel = Object.values(cost.by_model);
  const byAgent = Object.values(cost.by_agent);
  const navigate = useNavigate();
  const windowLabelKey = COST_WINDOW_OPTIONS.find(
    (o) => o.value === costWindow,
  )?.labelKey;
  const windowLabel = windowLabelKey
    ? t(windowLabelKey).toLowerCase()
    : costWindow;

  const openModelRates = async (modelId: string) => {
    const map = await resolveModelToProviderType("models").catch(() => null);
    const type = map?.[modelId];
    if (!type) return;
    navigate(`/config/providers.models/${encodeURIComponent(type)}?tab=costs`);
  };

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-center gap-2">
        <label
          className="text-xs uppercase tracking-wider"
          style={{ color: "var(--pc-text-secondary)" }}
        >
          {t("dashboard.cost.window")}
        </label>
        <select
          value={costWindow}
          onChange={(e) => onWindowChange(e.target.value as CostWindow)}
          className="input-electric text-sm px-2 py-1 appearance-none cursor-pointer"
        >
          {COST_WINDOW_OPTIONS.map((opt) => (
            <option key={opt.value} value={opt.value}>
              {t(opt.labelKey)}
            </option>
          ))}
        </select>
      </div>
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <div className="card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Bot className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.cost.spend_by_agent")} · {windowLabel}
            </h2>
          </div>
          {byAgent.length === 0 ? (
            <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
              {t("dashboard.cost.no_per_agent_pre")}{" "}
              <code>[cost].track_per_agent</code>
              {t("dashboard.cost.no_per_agent_post")}
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
                    style={{ background: "var(--pc-bg-elevated)" }}
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
                      <span
                        className="font-mono"
                        style={{ color: "var(--pc-text-primary)" }}
                      >
                        {formatUSD(row.cost_usd)}
                      </span>
                    </div>
                    <div
                      className="flex items-center gap-3 text-xs flex-wrap"
                      style={{ color: "var(--pc-text-muted)" }}
                    >
                      <span>{row.request_count} {t("dashboard.cost.exchanges")}</span>
                      <span>
                        {row.input_tokens.toLocaleString()} {t("dashboard.cost.input_tokens")}
                      </span>
                      {row.cached_input_tokens > 0 && (
                        <span>
                          {row.cached_input_tokens.toLocaleString()} {t("dashboard.cost.cached")}
                        </span>
                      )}
                      <span>
                        {row.output_tokens.toLocaleString()} {t("dashboard.cost.output_tokens")}
                      </span>
                    </div>
                  </li>
                ))}
            </ul>
          )}
        </div>

        <div className="card p-5 lg:col-span-2 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <DollarSign
              className="h-5 w-5"
              style={{ color: "var(--pc-accent)" }}
            />
            <h2
              className="text-sm font-semibold uppercase tracking-wider"
              style={{ color: "var(--pc-text-primary)" }}
            >
              {t("dashboard.cost.spend_by_model")} · {windowLabel}
            </h2>
          </div>
          {byModel.length === 0 ? (
            <p className="text-sm" style={{ color: "var(--pc-text-faint)" }}>
              {t("dashboard.cost.no_model_usage")}
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
                    style={{ background: "var(--pc-bg-elevated)" }}
                  >
                    <div className="flex items-center justify-between gap-3">
                      <button
                        type="button"
                        onClick={() => void openModelRates(row.model)}
                        className="font-mono break-all hover:underline text-left"
                        style={{
                          color: "var(--pc-text-primary)",
                          background: "transparent",
                        }}
                        title={`${t("dashboard.cost.open_rate_sheet_title")} ${row.model}`}
                      >
                        {row.model}
                      </button>
                      <span
                        className="font-mono"
                        style={{ color: "var(--pc-text-primary)" }}
                      >
                        {formatUSD(row.cost_usd)}
                      </span>
                    </div>
                    <div
                      className="flex items-center gap-3 text-xs flex-wrap"
                      style={{ color: "var(--pc-text-muted)" }}
                    >
                      <span>{row.request_count} {t("dashboard.cost.exchanges")}</span>
                      <span>
                        {row.input_tokens.toLocaleString()} {t("dashboard.cost.input_tokens")}
                      </span>
                      {row.cached_input_tokens > 0 && (
                        <span>
                          {row.cached_input_tokens.toLocaleString()} {t("dashboard.cost.cached")}
                        </span>
                      )}
                      <span>
                        {row.output_tokens.toLocaleString()} {t("dashboard.cost.output_tokens")}
                      </span>
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

type MemorySort = "newest" | "oldest" | "key-asc" | "key-desc";

const MEMORY_SORT_OPTIONS: { value: MemorySort; labelKey: string }[] = [
  { value: "newest", labelKey: "dashboard.mem.sort.newest" },
  { value: "oldest", labelKey: "dashboard.mem.sort.oldest" },
  { value: "key-asc", labelKey: "dashboard.mem.sort.key_asc" },
  { value: "key-desc", labelKey: "dashboard.mem.sort.key_desc" },
];

function isMemorySort(v: string): v is MemorySort {
  return MEMORY_SORT_OPTIONS.some((o) => o.value === v);
}

function MemoriesTab() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchParams, setSearchParams] = useSearchParams();
  const agentFilter = searchParams.get("agent") ?? "";
  const categoryFilter = searchParams.get("category") ?? "";
  const searchQuery = searchParams.get("q") ?? "";
  const sortRaw = searchParams.get("sort") ?? "";
  const sortBy: MemorySort = isMemorySort(sortRaw) ? sortRaw : "newest";
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
  // The entry queued for deletion; non-null opens the confirm dialog.
  const [pendingDelete, setPendingDelete] = useState<MemoryEntry | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [showAddForm, setShowAddForm] = useState(false);
  const [formKey, setFormKey] = useState("");
  const [formContent, setFormContent] = useState("");
  const [formCategory, setFormCategory] = useState("");
  const [formAgent, setFormAgent] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const toggleExpanded = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const setFilter = (key: "agent" | "category" | "q" | "sort", value: string) =>
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (value) next.set(key, value);
        else next.delete(key);
        return next;
      },
      { replace: true },
    );
  const setSearchQuery = (v: string) => setFilter("q", v);
  const setSortBy = (v: MemorySort) =>
    setFilter("sort", v === "newest" ? "" : v);

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
    getMapKeys("agents")
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
        case "oldest":
          return a.timestamp.localeCompare(b.timestamp);
        case "key-asc":
          return a.key.localeCompare(b.key);
        case "key-desc":
          return b.key.localeCompare(a.key);
        case "newest":
        default:
          return b.timestamp.localeCompare(a.timestamp);
      }
    });
    return sorted;
  }, [entries, sortBy]);

  // Runs once the operator confirms in the dialog; the destructive intent is
  // gated by ConfirmDialog rather than the native window.confirm.
  const handleDelete = async (entry: MemoryEntry) => {
    if (deleting) return;
    setPendingDelete(null);
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
      setFormError(t("dashboard.mem.error_key_content_required"));
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
      setFormKey("");
      setFormContent("");
      setFormCategory("");
      setFormAgent("");
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
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
          />
          <span className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
            {t("dashboard.mem.loading")}
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
          background: "var(--color-status-error-alpha-08)",
          borderColor: "var(--color-status-error-alpha-20)",
          color: "var(--color-status-error)",
        }}
      >
        {error}
      </div>
    );
  }

  return (
    <div className="card p-5 animate-slide-in-up space-y-4">
      <div className="flex items-center gap-2 flex-wrap">
        <Brain className="h-5 w-5" style={{ color: "var(--pc-accent)" }} />
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: "var(--pc-text-primary)" }}
        >
          {t("dashboard.mem.heading")}
        </h2>
        <span
          className="text-xs font-mono px-2 py-0.5 rounded-full"
          style={{
            background: "rgba(var(--pc-accent-rgb), 0.1)",
            color: "var(--pc-accent)",
          }}
        >
          {visibleEntries.length}
          {visibleEntries.length !== entries.length
            ? ` / ${entries.length}`
            : ""}
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
          title={t("dashboard.mem.add_title")}
        >
          <Plus className="h-3 w-3" />
          {t("dashboard.add_memory")}
        </button>

        <div className="ml-auto flex items-center gap-2 flex-wrap">
          <div className="relative">
            <Search
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <input
              type="search"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder={t("dashboard.search_placeholder")}
              className="input-electric pl-7 pr-2 py-1 text-xs w-40"
              title={t("dashboard.mem.search_title")}
              aria-label={t("dashboard.mem.search_aria")}
            />
          </div>
          <div className="relative">
            <ArrowUpDown
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={sortBy}
              onChange={(e) => setSortBy(e.target.value as MemorySort)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.mem.sort_title")}
              aria-label={t("dashboard.mem.sort_aria")}
            >
              {MEMORY_SORT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {t(o.labelKey)}
                </option>
              ))}
            </select>
          </div>
          <div className="relative">
            <Bot
              className="absolute left-2 top-1/2 -translate-y-1/2 h-3.5 w-3.5"
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={agentFilter}
              onChange={(e) => setFilter("agent", e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.filter_agent_title")}
            >
              <option value="">{t("dashboard.all_agents")}</option>
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
              style={{ color: "var(--pc-text-faint)" }}
            />
            <select
              value={categoryFilter}
              onChange={(e) => setFilter("category", e.target.value)}
              className="input-electric pl-7 pr-6 py-1 text-xs appearance-none cursor-pointer"
              title={t("dashboard.mem.filter_category_title")}
            >
              <option value="">{t("dashboard.mem.all_categories")}</option>
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
        <p
          className="text-sm py-8 text-center"
          style={{ color: "var(--pc-text-faint)" }}
        >
          {t("dashboard.mem.no_match")}
        </p>
      ) : (
        <div className="space-y-2 overflow-y-auto max-h-[32rem]">
          {visibleEntries.map((entry) => (
            <div
              key={entry.id}
              className="flex items-center justify-between gap-3 py-3 px-4 rounded-xl"
              style={{ background: "var(--pc-bg-elevated)" }}
            >
              <div className="flex-1 min-w-0">
                <div className="flex items-start gap-2 mb-1 flex-wrap">
                  <span
                    className="text-sm font-medium font-mono break-all"
                    style={{ color: "var(--pc-text-primary)" }}
                  >
                    {entry.key}
                  </span>
                  {entry.agent_alias && (
                    <EntityLink
                      kind="agent"
                      id={entry.agent_alias}
                      className="text-[10px] font-medium px-2 py-0.5 rounded-full flex-shrink-0 hover:underline"
                      style={{
                        background: "rgba(var(--pc-accent-rgb), 0.10)",
                        color: "var(--pc-accent-light)",
                      }}
                      title={`${t("dashboard.open_config_prefix")}agents.${entry.agent_alias}${t("dashboard.open_config_suffix")}`}
                    >
                      {entry.agent_alias}
                    </EntityLink>
                  )}
                  {entry.category && (
                    <span
                      className="text-[10px] font-mono px-2 py-0.5 rounded-full flex-shrink-0"
                      style={{
                        background: "rgba(167, 139, 250, 0.10)",
                        color: "#a78bfa",
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
                  style={{ color: "var(--pc-text-faint)" }}
                  title={entry.timestamp}
                >
                  {formatLocalDateTime(entry.timestamp)}
                </p>
              </div>
              <button
                type="button"
                onClick={() => setPendingDelete(entry)}
                disabled={deleting === entry.id}
                className="p-1.5 rounded-lg hover:bg-[var(--pc-hover)] disabled:opacity-50 flex-shrink-0"
                title={t("dashboard.mem.delete")}
                style={{ color: "var(--color-status-error)" }}
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
          style={{ background: "rgba(0,0,0,0.5)" }}
          onClick={() => setShowAddForm(false)}
        >
          <div
            className="card p-6 w-full max-w-md"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="flex items-center justify-between mb-4">
              <h3
                className="text-lg font-semibold"
                style={{ color: "var(--pc-text-primary)" }}
              >
                {t("dashboard.add_memory")}
              </h3>
              <button
                type="button"
                onClick={() => setShowAddForm(false)}
                className="p-1 rounded-lg hover:bg-[var(--pc-hover)]"
                style={{ color: "var(--pc-text-muted)" }}
              >
                <X className="h-4 w-4" />
              </button>
            </div>
            {formError && (
              <div
                className="mb-4 rounded-xl border p-3 text-sm"
                style={{
                  background: "var(--color-status-error-alpha-08)",
                  borderColor: "var(--color-status-error-alpha-20)",
                  color: "var(--color-status-error)",
                }}
              >
                {formError}
              </div>
            )}
            <div className="space-y-4">
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: "var(--pc-text-secondary)" }}
                >
                  {t("dashboard.mem.field_key")}{" "}
                  <span style={{ color: "var(--color-status-error)" }}>*</span>
                </label>
                <input
                  type="text"
                  value={formKey}
                  onChange={(e) => setFormKey(e.target.value)}
                  placeholder={t("dashboard.mem.key_placeholder")}
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: "var(--pc-text-secondary)" }}
                >
                  {t("dashboard.mem.field_content")}{" "}
                  <span style={{ color: "var(--color-status-error)" }}>*</span>
                </label>
                <textarea
                  value={formContent}
                  onChange={(e) => setFormContent(e.target.value)}
                  placeholder={t("dashboard.mem.content_placeholder")}
                  rows={4}
                  className="input-electric w-full px-3 py-2.5 text-sm resize-none"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: "var(--pc-text-secondary)" }}
                >
                  {t("dashboard.mem.field_category")}
                </label>
                <input
                  type="text"
                  value={formCategory}
                  onChange={(e) => setFormCategory(e.target.value)}
                  placeholder={t("dashboard.mem.category_placeholder")}
                  className="input-electric w-full px-3 py-2.5 text-sm"
                />
              </div>
              <div>
                <label
                  className="block text-xs font-semibold mb-1.5 uppercase tracking-wider"
                  style={{ color: "var(--pc-text-secondary)" }}
                >
                  {t("dashboard.mem.field_agent")}
                </label>
                <select
                  value={formAgent}
                  onChange={(e) => setFormAgent(e.target.value)}
                  className="input-electric w-full px-3 py-2.5 text-sm appearance-none cursor-pointer"
                >
                  <option value="">{t("dashboard.mem.install_wide")}</option>
                  {knownAgents.map((a) => (
                    <option key={a} value={a}>
                      {a}
                    </option>
                  ))}
                </select>
                <p
                  className="text-[11px] mt-1"
                  style={{ color: "var(--pc-text-faint)" }}
                >
                  {t("dashboard.mem.agent_hint")}
                </p>
              </div>
            </div>
            <div className="flex justify-end gap-3 mt-6">
              <button
                type="button"
                onClick={() => setShowAddForm(false)}
                className="btn-secondary px-4 py-2 text-sm font-medium"
              >
                {t("dashboard.mem.cancel")}
              </button>
              <button
                type="button"
                onClick={handleAdd}
                disabled={submitting}
                className="btn-electric px-4 py-2 text-sm font-medium disabled:opacity-50"
              >
                {submitting ? t("dashboard.mem.saving") : t("dashboard.mem.save")}
              </button>
            </div>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={pendingDelete !== null}
        danger
        title={t("common.delete")}
        message={`${t("dashboard.mem.confirm_delete_prefix")} ${pendingDelete?.key ?? ""}${t("dashboard.confirm_delete_suffix")}`}
        confirmLabel={t("common.delete")}
        onConfirm={() => {
          // Close the dialog first (capturing the target): a confirm clicked
          // while an earlier delete is still in flight would otherwise hit
          // handleDelete's `if (deleting) return` and never clear pendingDelete,
          // leaving the dialog stuck open.
          const target = pendingDelete;
          setPendingDelete(null);
          if (target) void handleDelete(target);
        }}
        onClose={() => setPendingDelete(null)}
      />
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
        style={{ color: "var(--pc-text-secondary)" }}
      >
        {display}
      </p>
      {oversize && (
        <button
          type="button"
          onClick={onToggle}
          className="text-[11px] mt-1 hover:underline"
          style={{ color: "var(--pc-accent)" }}
        >
          {expanded
            ? t("dashboard.mem.collapse")
            : `${t("dashboard.mem.expand")} (${content.length.toLocaleString()} ${t("dashboard.mem.chars")}, ${newlines + 1} ${t("dashboard.mem.lines")})`}
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
  const lines = content.split("\n");
  const slicedByNewline = lines.length > MEMORY_PREVIEW_NEWLINES;
  const byNewline = slicedByNewline
    ? lines.slice(0, MEMORY_PREVIEW_NEWLINES).join("\n")
    : content;
  if (byNewline.length > MEMORY_PREVIEW_CHARS) {
    return `${byNewline.slice(0, MEMORY_PREVIEW_CHARS).trimEnd()}…`;
  }
  return slicedByNewline ? `${byNewline}\n…` : byNewline;
}

// ---------------------------------------------------------------------------
// Dashboard metrics row — real aggregates derived from the agents list the
// section already loads. Every value is computed from AgentSummary fields, so
// nothing here is fabricated. Metrics that depend on data the page does not
// have are simply omitted.
// ---------------------------------------------------------------------------

function formatMetricUsd(value: number): string {
  if (value <= 0) return "$0";
  if (value < 0.01) return "<$0.01";
  // Below $100 keep cents; the prior `< 1` and `< 100` branches were identical.
  if (value < 100) return `$${value.toFixed(2)}`;
  return `$${Math.round(value).toLocaleString()}`;
}

function DashboardMetrics({ agents }: { agents: AgentSummary[] }) {
  const total = agents.length;
  const enabled = agents.filter((a) => a.enabled).length;
  const totalSessions = agents.reduce((sum, a) => sum + a.sessionCount, 0);
  const totalMemories = agents.reduce((sum, a) => sum + a.memoryCount, 0);
  // monthCostUsd is null when per-agent cost tracking is disabled; only sum the
  // agents that actually report a figure, and surface whether any did.
  const trackedSpend = agents.filter((a) => a.monthCostUsd !== null);
  const totalSpend = trackedSpend.reduce(
    (sum, a) => sum + (a.monthCostUsd ?? 0),
    0,
  );

  return (
    <div className="grid grid-cols-2 gap-3 sm:gap-4 lg:grid-cols-5">
      <StatCard
        label={t("dash.metric.agents")}
        value={total}
        sublabel={
          total === 0
            ? t("dash.metric.agents.none")
            : `${enabled} ${t("dash.metric.agents.enabled_sub")}`
        }
        icon={<Bot className="h-5 w-5" />}
        tone="neutral"
      />
      <StatCard
        label={t("dash.metric.enabled")}
        value={`${enabled}/${total}`}
        sublabel={t("dash.metric.enabled.sub")}
        icon={<Activity className="h-5 w-5" />}
        tone={enabled > 0 ? "ok" : "neutral"}
      />
      <StatCard
        label={t("dash.metric.sessions")}
        value={totalSessions.toLocaleString()}
        sublabel={t("dash.metric.sessions.sub")}
        icon={<MessageSquare className="h-5 w-5" />}
        tone="neutral"
      />
      <StatCard
        label={t("dash.metric.memories")}
        value={totalMemories.toLocaleString()}
        sublabel={t("dash.metric.memories.sub")}
        icon={<Brain className="h-5 w-5" />}
        tone="neutral"
      />
      <StatCard
        label={t("dash.metric.spend")}
        value={trackedSpend.length === 0 ? "—" : formatMetricUsd(totalSpend)}
        sublabel={
          trackedSpend.length === 0
            ? t("dash.metric.spend.untracked")
            : t("dash.metric.spend.sub")
        }
        icon={<DollarSign className="h-5 w-5" />}
        tone="neutral"
      />
    </div>
  );
}

// ---------------------------------------------------------------------------
// AgentsSection — top-of-dashboard agent grid. Always visible (above the
// global-stats tabs) so the dashboard reads as "many agents + system state"
// rather than "the agent". Same card component used on /agents.
// ---------------------------------------------------------------------------

function AgentsSection() {
  const [agents, setAgents] = useState<AgentSummary[] | null>(null);
  const [quickstartLabel, setQuickstartLabel] = useState(
    t("dashboard.start_quickstart"),
  );
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<Set<string>>(new Set());
  // Selecting a row sets the drawer's agent (by alias); closing clears it.
  // Keying off the alias keeps the open drawer in sync with live toggles.
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);

  useEffect(() => {
    loadAgentSummaries()
      .then(setAgents)
      .catch((err: unknown) =>
        setError(
          err instanceof Error ? err.message : t("dashboard.load_agents_error"),
        ),
      );
  }, []);

  useEffect(() => {
    getQuickstartState()
      .then((state) => {
        if (state.agents.length > 0) {
          setQuickstartLabel(t("dashboard.create_another_agent"));
        } else {
          setQuickstartLabel(t("dashboard.start_quickstart"));
        }
      })
      .catch(() => setQuickstartLabel(t("dashboard.start_quickstart")));
  }, []);

  const handleToggle = useCallback(async (agent: AgentSummary) => {
    setToggling((prev) => new Set(prev).add(agent.alias));
    try {
      await toggleAgentEnabled(agent.alias, !agent.enabled);
      setAgents(
        (prev) =>
          prev?.map((a) =>
            a.alias === agent.alias ? { ...a, enabled: !a.enabled } : a,
          ) ?? null,
      );
    } catch (err) {
      setError(
        err instanceof Error
          ? err.message
          : `${t("dashboard.toggle_agent_error_prefix")} ${agent.alias}`,
      );
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
  const visibleAgents = sortedAgents
    ? sortedAgents.slice(0, AGENT_GLANCE_LIMIT)
    : null;
  const hiddenCount = sortedAgents
    ? Math.max(0, sortedAgents.length - AGENT_GLANCE_LIMIT)
    : 0;

  const selectedAgent =
    selectedAlias === null
      ? null
      : (agents?.find((a) => a.alias === selectedAlias) ?? null);

  return (
    <section className="space-y-6">
      <PageHeader
        title={t("dash.title")}
        description={t("dash.subtitle")}
        actions={
          <Link
            to="/agents"
            className="text-xs flex items-center gap-1 hover:underline"
            style={{ color: "var(--pc-text-muted)" }}
          >
            {hiddenCount > 0
              ? `${t("dash.view_all")} (${sortedAgents!.length})`
              : t("dash.view_all")}
            <ChevronRight className="h-3 w-3" />
          </Link>
        }
      />

      {sortedAgents && sortedAgents.length > 0 && (
        <DashboardMetrics agents={sortedAgents} />
      )}

      <header className="flex items-center gap-2">
        <h2
          className="text-sm font-semibold uppercase tracking-wider"
          style={{ color: "var(--pc-text-secondary)" }}
        >
          {t("dash.agents_heading")}
        </h2>
        {sortedAgents && sortedAgents.length > 0 && (
          <span
            className="text-xs font-mono px-2 py-0.5 rounded-full"
            style={{
              background: "rgba(var(--pc-accent-rgb), 0.1)",
              color: "var(--pc-accent)",
            }}
          >
            {sortedAgents.length}
          </span>
        )}
      </header>

      {error && (
        <div
          className="mb-3 px-3 py-2 rounded-xl border text-xs"
          style={{
            background: "var(--color-status-error-alpha-08)",
            borderColor: "var(--color-status-error-alpha-20)",
            color: "var(--color-status-error)",
          }}
        >
          {error}
        </div>
      )}

      {agents === null ? (
        <div
          className="rounded-2xl border p-6 text-center text-sm"
          style={{
            borderColor: "var(--pc-border)",
            color: "var(--pc-text-muted)",
          }}
        >
          {t("dashboard.loading_agents")}
        </div>
      ) : agents.length === 0 ? (
        <div
          className="rounded-2xl border-2 border-dashed p-6 text-center"
          style={{ borderColor: "var(--pc-border)" }}
        >
          <p
            className="text-sm font-medium mb-2"
            style={{ color: "var(--pc-text-primary)" }}
          >
            {t("dashboard.no_agents_configured")}
          </p>
          <Link
            to="/quickstart"
            className="btn-electric inline-flex items-center gap-2 px-3 py-1.5 rounded-xl text-xs"
          >
            <Plus className="h-3.5 w-3.5" />
            {quickstartLabel}
          </Link>
        </div>
      ) : (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface overflow-hidden">
          {visibleAgents!.map((agent) => (
            <AgentCard
              key={agent.alias}
              agent={agent}
              selected={agent.alias === selectedAlias}
              onSelect={() => setSelectedAlias(agent.alias)}
            />
          ))}
          {hiddenCount > 0 && (
            <Link
              to="/agents"
              className="flex items-center justify-center gap-1.5 px-4 py-3 text-sm font-medium border-t border-pc-border text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text"
            >
              {t("dash.view_all")} · {hiddenCount}{" "}
              {hiddenCount === 1
                ? t("dashboard.more_agent")
                : t("dashboard.more_agents")}
              <ChevronRight className="h-3.5 w-3.5" />
            </Link>
          )}
        </div>
      )}

      <AgentDrawer
        agent={selectedAgent}
        onClose={() => setSelectedAlias(null)}
        onToggle={handleToggle}
        toggling={selectedAgent ? toggling.has(selectedAgent.alias) : false}
      />
    </section>
  );
}
