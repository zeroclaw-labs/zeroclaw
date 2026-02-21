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
      return 'bg-green-500';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'bg-yellow-500';
    default:
      return 'bg-red-500';
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'border-green-500/30';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'border-yellow-500/30';
    default:
      return 'border-red-500/30';
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
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load dashboard: {error}
        </div>
      </div>
    );
  }

  if (!status || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  const maxCost = Math.max(cost.session_cost_usd, cost.daily_cost_usd, cost.monthly_cost_usd, 0.001);

  return (
    <div className="p-6 space-y-6">
      {/* Status Cards Grid */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-blue-600/20 rounded-lg">
              <Cpu className="h-5 w-5 text-blue-400" />
            </div>
            <span className="text-sm text-gray-400">Provider / Model</span>
          </div>
          <p className="text-lg font-semibold text-white truncate">
            {status.provider ?? 'Unknown'}
          </p>
          <p className="text-sm text-gray-400 truncate">{status.model}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-green-600/20 rounded-lg">
              <Clock className="h-5 w-5 text-green-400" />
            </div>
            <span className="text-sm text-gray-400">Uptime</span>
          </div>
          <p className="text-lg font-semibold text-white">
            {formatUptime(status.uptime_seconds)}
          </p>
          <p className="text-sm text-gray-400">Since last restart</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-purple-600/20 rounded-lg">
              <Globe className="h-5 w-5 text-purple-400" />
            </div>
            <span className="text-sm text-gray-400">Gateway Port</span>
          </div>
          <p className="text-lg font-semibold text-white">
            :{status.gateway_port}
          </p>
          <p className="text-sm text-gray-400">Locale: {status.locale}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-orange-600/20 rounded-lg">
              <Database className="h-5 w-5 text-orange-400" />
            </div>
            <span className="text-sm text-gray-400">Memory Backend</span>
          </div>
          <p className="text-lg font-semibold text-white capitalize">
            {status.memory_backend}
          </p>
          <p className="text-sm text-gray-400">
            Paired: {status.paired ? 'Yes' : 'No'}
          </p>
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        {/* Cost Widget */}
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <DollarSign className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Cost Overview</h2>
          </div>
          <div className="space-y-4">
            {[
              { label: 'Session', value: cost.session_cost_usd, color: 'bg-blue-500' },
              { label: 'Daily', value: cost.daily_cost_usd, color: 'bg-green-500' },
              { label: 'Monthly', value: cost.monthly_cost_usd, color: 'bg-purple-500' },
            ].map(({ label, value, color }) => (
              <div key={label}>
                <div className="flex justify-between text-sm mb-1">
                  <span className="text-gray-400">{label}</span>
                  <span className="text-white font-medium">{formatUSD(value)}</span>
                </div>
                <div className="w-full h-2 bg-gray-800 rounded-full overflow-hidden">
                  <div
                    className={`h-full rounded-full ${color}`}
                    style={{ width: `${Math.max((value / maxCost) * 100, 2)}%` }}
                  />
                </div>
              </div>
            ))}
          </div>
          <div className="mt-4 pt-3 border-t border-gray-800 flex justify-between text-sm">
            <span className="text-gray-400">Total Tokens</span>
            <span className="text-white">{cost.total_tokens.toLocaleString()}</span>
          </div>
          <div className="flex justify-between text-sm mt-1">
            <span className="text-gray-400">Requests</span>
            <span className="text-white">{cost.request_count.toLocaleString()}</span>
          </div>
        </div>

        {/* Active Channels */}
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <Radio className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Active Channels</h2>
          </div>
          <div className="space-y-2">
            {Object.entries(status.channels).length === 0 ? (
              <p className="text-sm text-gray-500">No channels configured</p>
            ) : (
              Object.entries(status.channels).map(([name, active]) => (
                <div
                  key={name}
                  className="flex items-center justify-between py-2 px-3 rounded-lg bg-gray-800/50"
                >
                  <span className="text-sm text-white capitalize">{name}</span>
                  <div className="flex items-center gap-2">
                    <span
                      className={`inline-block h-2.5 w-2.5 rounded-full ${
                        active ? 'bg-green-500' : 'bg-gray-500'
                      }`}
                    />
                    <span className="text-xs text-gray-400">
                      {active ? 'Active' : 'Inactive'}
                    </span>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>

        {/* Health Grid */}
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <Activity className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Component Health</h2>
          </div>
          <div className="grid grid-cols-2 gap-3">
            {Object.entries(status.health.components).length === 0 ? (
              <p className="text-sm text-gray-500 col-span-2">No components reporting</p>
            ) : (
              Object.entries(status.health.components).map(([name, comp]) => (
                <div
                  key={name}
                  className={`rounded-lg p-3 border ${healthBorder(comp.status)} bg-gray-800/50`}
                >
                  <div className="flex items-center gap-2 mb-1">
                    <span className={`inline-block h-2 w-2 rounded-full ${healthColor(comp.status)}`} />
                    <span className="text-sm font-medium text-white capitalize truncate">
                      {name}
                    </span>
                  </div>
                  <p className="text-xs text-gray-400 capitalize">{comp.status}</p>
                  {comp.restart_count > 0 && (
                    <p className="text-xs text-yellow-400 mt-1">
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
