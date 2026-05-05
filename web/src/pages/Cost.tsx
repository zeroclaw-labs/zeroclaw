import { useState, useEffect } from 'react';
import {
  DollarSign,
  TrendingUp,
  Hash,
  Layers,
} from 'lucide-react';
import type { CostSummary } from '@/types/api';
import { getCost } from '@/lib/api';
import { t } from '@/lib/i18n';

function formatUSD(value: number): string {
  return `$${value.toFixed(4)}`;
}

export default function Cost() {
  const [cost, setCost] = useState<CostSummary | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getCost().then(setCost).catch((err) => setError(err.message)).finally(() => setLoading(false));
  }, []);

  if (error) {
    return (
      <div className="p-6 animate-fade-in">
        <div className="rounded-2xl border p-4" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
          {t('cost.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin" style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  const models = Object.values(cost.by_model);

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      {/* Summary Cards */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4 stagger-children">
        {[
          { icon: DollarSign, accent: 'var(--pc-accent)', bg: 'rgba(var(--pc-accent-rgb), 0.08)', label: t('cost.session_cost'), value: formatUSD(cost.session_cost_usd) },
          { icon: TrendingUp, accent: 'var(--color-status-success)', bg: 'rgba(0, 230, 138, 0.08)', label: t('cost.daily_cost'), value: formatUSD(cost.daily_cost_usd) },
          { icon: Layers, accent: '#a78bfa', bg: 'rgba(167, 139, 250, 0.08)', label: t('cost.monthly_cost'), value: formatUSD(cost.monthly_cost_usd) },
          { icon: Hash, accent: 'var(--color-status-warning)', bg: 'rgba(255, 170, 0, 0.08)', label: t('cost.total_requests'), value: cost.request_count.toLocaleString() },
        ].map(({ icon: Icon, accent, bg, label, value }) => (
          <div key={label} className="card p-5 animate-slide-in-up">
            <div className="flex items-center gap-3 mb-3">
              <div className="p-2 rounded-2xl" style={{ background: bg, color: accent }}>
                <Icon className="h-5 w-5" />
              </div>
              <span className="text-xs uppercase tracking-wider font-medium" style={{ color: 'var(--pc-text-muted)' }}>{label}</span>
            </div>
            <p className="text-2xl font-bold font-mono" style={{ color: 'var(--pc-text-primary)' }}>{value}</p>
          </div>
        ))}
      </div>

      {/* Token Statistics */}
      <div className="card p-5 animate-slide-in-up" style={{ animationDelay: '200ms' }}>
        <h3 className="text-sm font-semibold mb-4 uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
          {t('cost.token_statistics')}
        </h3>
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
          {[
            { label: t('cost.total_tokens'), value: cost.total_tokens.toLocaleString() },
            { label: t('cost.avg_tokens_per_request'), value: cost.request_count > 0 ? Math.round(cost.total_tokens / cost.request_count).toLocaleString() : '0' },
            { label: t('cost.cost_per_1k_tokens'), value: cost.total_tokens > 0 ? formatUSD((cost.monthly_cost_usd / cost.total_tokens) * 1000) : '$0.0000' },
          ].map(({ label, value }) => (
            <div key={label} className="rounded-2xl p-4 border" style={{ background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-border)' }}>
              <p className="text-xs uppercase tracking-wider" style={{ color: 'var(--pc-text-muted)' }}>{label}</p>
              <p className="text-xl font-bold mt-1 font-mono" style={{ color: 'var(--pc-text-primary)' }}>{value}</p>
            </div>
          ))}
        </div>
      </div>

      {/* Model Breakdown Table */}
      <div className="card overflow-hidden animate-slide-in-up rounded-2xl" style={{ animationDelay: '300ms' }}>
        <div className="px-5 py-4 border-b" style={{ borderColor: 'var(--pc-border)' }}>
          <h3 className="text-sm font-semibold uppercase tracking-wider" style={{ color: 'var(--pc-text-primary)' }}>
            {t('cost.model_breakdown')}
          </h3>
        </div>
        {models.length === 0 ? (
          <div className="p-8 text-center" style={{ color: 'var(--pc-text-faint)' }}>
            {t('cost.no_model_data')}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="table-electric">
              <thead>
                <tr>
                  <th>{t('cost.model')}</th>
                  <th className="text-right">{t('cost.cost')}</th>
                  <th className="text-right">{t('cost.tokens')}</th>
                  <th className="text-right">{t('cost.requests')}</th>
                  <th>{t('cost.share')}</th>
                </tr>
              </thead>
              <tbody>
                {models.sort((a, b) => b.cost_usd - a.cost_usd).map((m) => {
                  const share = cost.monthly_cost_usd > 0 ? (m.cost_usd / cost.monthly_cost_usd) * 100 : 0;
                  return (
                    <tr key={m.model}>
                      <td className="font-medium text-sm" style={{ color: 'var(--pc-text-primary)' }}>
                        {m.model}
                      </td>
                      <td className="text-right font-mono text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
                        {formatUSD(m.cost_usd)}
                      </td>
                      <td className="text-right text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
                        {m.total_tokens.toLocaleString()}
                      </td>
                      <td className="text-right text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
                        {m.request_count.toLocaleString()}
                      </td>
                      <td>
                        <div className="flex items-center gap-2">
                          <div className="w-20 h-1.5 rounded-full overflow-hidden" style={{ background: 'var(--pc-hover)' }}>
                            <div
                              className="h-full rounded-full progress-bar-animated transition-all duration-700"
                              style={{ width: `${Math.max(share, 2)}%`, background: 'var(--pc-accent)' }}
                            />
                          </div>
                          <span className="text-xs font-mono w-10 text-right" style={{ color: 'var(--pc-text-muted)' }}>
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
