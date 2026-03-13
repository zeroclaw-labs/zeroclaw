import { useState, useEffect } from 'react';
import {
  DollarSign,
  TrendingUp,
  Hash,
  Layers,
} from 'lucide-react';
import type { CostSummary } from '@/types/api';
import { getCost } from '@/lib/api';

function formatUSD(value: number): string {
  return `$${value.toFixed(4)}`;
}

export default function Cost() {
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getCost()
      .then(setCost)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-xl bg-[#ff446615] border border-[#ff446630] p-4 text-[#ff6680]">
          Failed to load cost data: {error}
        </div>
      </div>
    );
  }

  if (loading || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 border-[#0080ff30] border-t-[#0080ff] rounded-full animate-spin" />
      </div>
    );
  }

  const models = Object.values(cost.by_model);

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Summary Cards */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {[
          { icon: DollarSign, color: '#0080ff', bg: '#0080ff15', label: 'Session Cost', value: formatUSD(cost.session_cost_usd) },
          { icon: TrendingUp, color: '#00e68a', bg: '#00e68a15', label: 'Daily Cost', value: formatUSD(cost.daily_cost_usd) },
          { icon: Layers, color: '#a855f7', bg: '#a855f715', label: 'Monthly Cost', value: formatUSD(cost.monthly_cost_usd) },
          { icon: Hash, color: '#ff8800', bg: '#ff880015', label: 'Total Requests', value: cost.request_count.toLocaleString() },
        ].map(({ icon: Icon, color, bg, label, value }) => (
          <div key={label} className="glass-card p-5 animate-slide-in-up">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 rounded-xl" style={{ background: bg }}>
                <Icon className="h-5 w-5" style={{ color }} />
              </div>
              <span className="text-xs text-[#556080] uppercase tracking-wider font-medium">{label}</span>
            </div>
            <p className="text-2xl font-bold text-white font-mono">{value}</p>
          </div>
        ))}
      </div>

      {/* Token Statistics */}
      <div className="glass-card p-5 animate-slide-in-up" style={{ animationDelay: '200ms' }}>
        <h3 className="text-sm font-semibold text-white mb-4 uppercase tracking-wider">
          Token Statistics
        </h3>
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
          {[
            { label: 'Total Tokens', value: cost.total_tokens.toLocaleString() },
            { label: 'Avg Tokens / Request', value: cost.request_count > 0 ? Math.round(cost.total_tokens / cost.request_count).toLocaleString() : '0' },
            { label: 'Cost per 1K Tokens', value: cost.total_tokens > 0 ? formatUSD((cost.monthly_cost_usd / cost.total_tokens) * 1000) : '$0.0000' },
          ].map(({ label, value }) => (
            <div key={label} className="rounded-xl p-4" style={{ background: 'rgba(0,128,255,0.04)', border: '1px solid rgba(0,128,255,0.08)' }}>
              <p className="text-xs text-[#556080] uppercase tracking-wider">{label}</p>
              <p className="text-xl font-bold text-white mt-1 font-mono">{value}</p>
            </div>
          ))}
        </div>
      </div>

      {/* Model Breakdown Table */}
      <div className="glass-card overflow-hidden animate-slide-in-up" style={{ animationDelay: '300ms' }}>
        <div className="px-5 py-4 border-b border-[#1a1a3e]">
          <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
            Model Breakdown
          </h3>
        </div>
        {models.length === 0 ? (
          <div className="p-8 text-center text-[#334060]">
            No model data available.
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="table-electric">
              <thead>
                <tr>
                  <th className="text-left">Model</th>
                  <th className="text-right">Cost</th>
                  <th className="text-right">Tokens</th>
                  <th className="text-right">Requests</th>
                  <th className="text-left">Share</th>
                </tr>
              </thead>
              <tbody>
                {models
                  .sort((a, b) => b.cost_usd - a.cost_usd)
                  .map((m) => {
                    const share =
                      cost.monthly_cost_usd > 0
                        ? (m.cost_usd / cost.monthly_cost_usd) * 100
                        : 0;
                    return (
                      <tr key={m.model}>
                        <td className="px-5 py-3 text-white font-medium text-sm">
                          {m.model}
                        </td>
                        <td className="px-5 py-3 text-[#8892a8] text-right font-mono text-sm">
                          {formatUSD(m.cost_usd)}
                        </td>
                        <td className="px-5 py-3 text-[#8892a8] text-right text-sm">
                          {m.total_tokens.toLocaleString()}
                        </td>
                        <td className="px-5 py-3 text-[#8892a8] text-right text-sm">
                          {m.request_count.toLocaleString()}
                        </td>
                        <td className="px-5 py-3">
                          <div className="flex items-center gap-2">
                            <div className="w-20 h-1.5 bg-[#0a0a18] rounded-full overflow-hidden">
                              <div
                                className="h-full rounded-full progress-bar-animated transition-all duration-700"
                                style={{ width: `${Math.max(share, 2)}%`, background: '#0080ff' }}
                              />
                            </div>
                            <span className="text-xs text-[#556080] w-10 text-right font-mono">
                              {share.toFixed(1)}%
                            </span>
                          </div>
                        </td>
                      </tr>
                    );
                  })}
              </tbody>
            </table>
          </div>
        )}
      </div>
    </div>
  );
}
