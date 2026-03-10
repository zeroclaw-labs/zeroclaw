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
    getCost()
      .then(setCost)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          {t('cost.load_failed')}: {error}
        </div>
      </div>
    );
  }

  if (loading || !cost) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  const models = Object.values(cost.by_model);

  return (
    <div className="p-6 space-y-6">
      {/* Summary Cards */}
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-blue-600/20 rounded-lg">
              <DollarSign className="h-5 w-5 text-blue-400" />
            </div>
            <span className="text-sm text-gray-400">{t('cost.session')}</span>
          </div>
          <p className="text-2xl font-bold text-white">
            {formatUSD(cost.session_cost_usd)}
          </p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-green-600/20 rounded-lg">
              <TrendingUp className="h-5 w-5 text-green-400" />
            </div>
            <span className="text-sm text-gray-400">{t('cost.daily')}</span>
          </div>
          <p className="text-2xl font-bold text-white">
            {formatUSD(cost.daily_cost_usd)}
          </p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-purple-600/20 rounded-lg">
              <Layers className="h-5 w-5 text-purple-400" />
            </div>
            <span className="text-sm text-gray-400">{t('cost.monthly')}</span>
          </div>
          <p className="text-2xl font-bold text-white">
            {formatUSD(cost.monthly_cost_usd)}
          </p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-orange-600/20 rounded-lg">
              <Hash className="h-5 w-5 text-orange-400" />
            </div>
            <span className="text-sm text-gray-400">{t('cost.total_requests')}</span>
          </div>
          <p className="text-2xl font-bold text-white">
            {cost.request_count.toLocaleString()}
          </p>
        </div>
      </div>

      {/* Token Statistics */}
      <div className="bg-gray-900 rounded-xl border border-gray-800 p-5">
        <h3 className="text-base font-semibold text-white mb-4">
          {t('cost.token_statistics')}
        </h3>
        <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
          <div className="bg-gray-800/50 rounded-lg p-4">
            <p className="text-sm text-gray-400">{t('cost.total_tokens')}</p>
            <p className="text-xl font-bold text-white mt-1">
              {cost.total_tokens.toLocaleString()}
            </p>
          </div>
          <div className="bg-gray-800/50 rounded-lg p-4">
            <p className="text-sm text-gray-400">{t('cost.avg_tokens_per_request')}</p>
            <p className="text-xl font-bold text-white mt-1">
              {cost.request_count > 0
                ? Math.round(cost.total_tokens / cost.request_count).toLocaleString()
                : '0'}
            </p>
          </div>
          <div className="bg-gray-800/50 rounded-lg p-4">
            <p className="text-sm text-gray-400">{t('cost.cost_per_1k_tokens')}</p>
            <p className="text-xl font-bold text-white mt-1">
              {cost.total_tokens > 0
                ? formatUSD((cost.monthly_cost_usd / cost.total_tokens) * 1000)
                : '$0.0000'}
            </p>
          </div>
        </div>
      </div>

      {/* Model Breakdown Table */}
      <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
        <div className="px-5 py-4 border-b border-gray-800">
          <h3 className="text-base font-semibold text-white">
            {t('cost.model_breakdown')}
          </h3>
        </div>
        {models.length === 0 ? (
          <div className="p-8 text-center text-gray-500">
            {t('cost.no_model_data')}
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-gray-800">
                  <th className="text-left px-5 py-3 text-gray-400 font-medium">
                    {t('cost.model')}
                  </th>
                  <th className="text-right px-5 py-3 text-gray-400 font-medium">
                    {t('cost.usd')}
                  </th>
                  <th className="text-right px-5 py-3 text-gray-400 font-medium">
                    {t('cost.tokens')}
                  </th>
                  <th className="text-right px-5 py-3 text-gray-400 font-medium">
                    {t('cost.requests')}
                  </th>
                  <th className="text-left px-5 py-3 text-gray-400 font-medium">
                    {t('cost.share')}
                  </th>
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
                      <tr
                        key={m.model}
                        className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors"
                      >
                        <td className="px-5 py-3 text-white font-medium">
                          {m.model}
                        </td>
                        <td className="px-5 py-3 text-gray-300 text-right font-mono">
                          {formatUSD(m.cost_usd)}
                        </td>
                        <td className="px-5 py-3 text-gray-300 text-right">
                          {m.total_tokens.toLocaleString()}
                        </td>
                        <td className="px-5 py-3 text-gray-300 text-right">
                          {m.request_count.toLocaleString()}
                        </td>
                        <td className="px-5 py-3">
                          <div className="flex items-center gap-2">
                            <div className="w-20 h-2 bg-gray-800 rounded-full overflow-hidden">
                              <div
                                className="h-full bg-blue-500 rounded-full"
                                style={{ width: `${Math.max(share, 2)}%` }}
                              />
                            </div>
                            <span className="text-xs text-gray-400 w-10 text-right">
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
