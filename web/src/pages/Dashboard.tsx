import { useState, useEffect } from "react";
import {
  Cpu,
  Clock,
  Globe,
  Database,
  Activity,
  DollarSign,
  Radio,
} from "lucide-react";
import type { StatusResponse, CostSummary } from "@/types/api";
import { getStatus, getCost } from "@/lib/api";

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
    case "ok":
    case "healthy":
      return "bg-[#00e68a]";
    case "warn":
    case "warning":
    case "degraded":
      return "bg-[#ffaa00]";
    default:
      return "bg-[#ff4466]";
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case "ok":
    case "healthy":
      return "border-[#00e68a30]";
    case "warn":
    case "warning":
    case "degraded":
      return "border-[#ffaa0030]";
    default:
      return "border-[#ff446630]";
  }
}

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

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
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680]">
          Failed to load dashboard: {error}
        </div>
      </div>
    );
  }

  if (!status || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
      </div>
    );
  }

  const maxCost = Math.max(
    cost.session_cost_usd,
    cost.daily_cost_usd,
    cost.monthly_cost_usd,
    0.001,
  );

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Status Cards Grid */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {[
          {
            icon: Cpu,
            color: "#0080ff",
            bg: "#0080ff15",
            label: "Provider / Model",
            value: status.provider ?? "Unknown",
            sub: status.model,
          },
          {
            icon: Clock,
            color: "#00e68a",
            bg: "#00e68a15",
            label: "Uptime",
            value: formatUptime(status.uptime_seconds),
            sub: "Since last restart",
          },
          {
            icon: Globe,
            color: "#a855f7",
            bg: "#a855f715",
            label: "Gateway Port",
            value: `:${status.gateway_port}`,
            sub: `Locale: ${status.locale}`,
          },
          {
            icon: Database,
            color: "#ff8800",
            bg: "#ff880015",
            label: "Memory Backend",
            value: status.memory_backend,
            sub: `Paired: ${status.paired ? "Yes" : "No"}`,
          },
        ].map(({ icon: Icon, color, bg, label, value, sub }) => (
          <div key={label} className="glass-card p-5 animate-slide-in-up">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 rounded-xl" style={{ background: bg }}>
                <Icon className="h-5 w-5" style={{ color }} />
              </div>
              <span className="text-xs text-[#556080] uppercase tracking-wider font-medium">
                {label}
              </span>
            </div>
            <p className="text-lg font-semibold text-white truncate capitalize">
              {value}
            </p>
            <p className="text-sm text-[#556080] truncate">{sub}</p>
          </div>
        ))}
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6 stagger-children">
        {/* Cost Widget */}
        <div className="glass-card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <DollarSign className="h-5 w-5 text-[#0080ff]" />
            <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
              Cost Overview
            </h2>
          </div>
          <div className="space-y-4">
            {[
              {
                label: "Session",
                value: cost.session_cost_usd,
                color: "#0080ff",
              },
              { label: "Daily", value: cost.daily_cost_usd, color: "#00e68a" },
              {
                label: "Monthly",
                value: cost.monthly_cost_usd,
                color: "#a855f7",
              },
            ].map(({ label, value, color }) => (
              <div key={label}>
                <div className="flex justify-between text-sm mb-1.5">
                  <span className="text-[#556080]">{label}</span>
                  <span className="text-white font-medium font-mono">
                    {formatUSD(value)}
                  </span>
                </div>
                <div className="w-full h-1.5 bg-[#0a0a18] rounded-full overflow-hidden">
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
          <div className="mt-5 pt-4 border-t border-[#1a1a3e]/50 flex justify-between text-sm">
            <span className="text-[#556080]">Total Tokens</span>
            <span className="text-white font-mono">
              {cost.total_tokens.toLocaleString()}
            </span>
          </div>
          <div className="flex justify-between text-sm mt-1">
            <span className="text-[#556080]">Requests</span>
            <span className="text-white font-mono">
              {cost.request_count.toLocaleString()}
            </span>
          </div>
        </div>

        {/* Active Channels */}
        <div className="glass-card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Radio className="h-5 w-5 text-[#0080ff]" />
            <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
              Active Channels
            </h2>
          </div>
          <div className="space-y-2">
            {Object.entries(status.channels).length === 0 ? (
              <p className="text-sm text-[#334060]">No channels configured</p>
            ) : (
              Object.entries(status.channels).map(([name, active]) => (
                <div
                  key={name}
                  className="flex items-center justify-between py-2.5 px-3 rounded-xl transition-all duration-300 hover:bg-[#0080ff08]"
                  style={{ background: "rgba(10, 10, 26, 0.5)" }}
                >
                  <span className="text-sm text-white capitalize font-medium">
                    {name}
                  </span>
                  <div className="flex items-center gap-2">
                    <span
                      className={`inline-block h-2 w-2 rounded-full glow-dot ${
                        active
                          ? "text-[#00e68a] bg-[#00e68a]"
                          : "text-[#334060] bg-[#334060]"
                      }`}
                    />
                    <span className="text-xs text-[#556080]">
                      {active ? "Active" : "Inactive"}
                    </span>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Health Grid */}
        <div className="glass-card p-5 animate-slide-in-up">
          <div className="flex items-center gap-2 mb-5">
            <Activity className="h-5 w-5 text-[#0080ff]" />
            <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
              Component Health
            </h2>
          </div>
          <div className="grid grid-cols-2 gap-3">
            {Object.entries(status.health.components).length === 0 ? (
              <p className="text-sm text-[#334060] col-span-2">
                No components reporting
              </p>
            ) : (
              Object.entries(status.health.components).map(([name, comp]) => (
                <div
                  key={name}
                  className={`rounded-xl p-3 border ${healthBorder(comp.status)} transition-all duration-300 hover:scale-[1.02]`}
                  style={{ background: "rgba(10, 10, 26, 0.5)" }}
                >
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className={`inline-block h-2 w-2 rounded-full ${healthColor(comp.status)} glow-dot`}
                    />
                    <span className="text-sm font-medium text-white capitalize truncate">
                      {name}
                    </span>
                  </div>
                  <p className="text-xs text-[#556080] capitalize">
                    {comp.status}
                  </p>
                  {comp.restart_count > 0 && (
                    <p className="text-xs text-[#ffaa00] mt-1">
                      Restarts: {comp.restart_count}
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
