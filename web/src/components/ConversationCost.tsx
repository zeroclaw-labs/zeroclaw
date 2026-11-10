import type { ConversationCostSummary } from '@/lib/api';
import { conversationCostAmount } from '@/lib/conversationCost';
import { t } from '@/lib/i18n';

export function ConversationCost({ summary }: { summary: ConversationCostSummary | null }) {
  const amount = summary?.estimated_cost_usd;
  const available = summary?.status !== 'unavailable' && amount != null && Number.isFinite(amount) && amount >= 0;
  const label = available
    ? `${t('agent.cost_estimate')} ${conversationCostAmount(amount)}${summary?.status === 'partial' ? ` · ${t('agent.cost_incomplete')}` : ''}`
    : t('agent.cost_unavailable');
  const detail = [t('agent.cost_detail'), ...(summary?.reasons ?? []).map((reason) => t(`agent.cost_reason_${reason}`))].join(' ');
  return <span className="text-[10px] whitespace-nowrap" style={{ color: 'var(--pc-text-faint)' }} title={detail} aria-label={`${label}. ${detail}`}>{label}</span>;
}
