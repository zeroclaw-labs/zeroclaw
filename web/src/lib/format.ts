import { t } from '@/lib/i18n';

export function formatUsd(value: number | null): string {
  if (value === null) return '—';
  if (value < 0.01) return '<$0.01';
  return `$${value.toFixed(2)}`;
}

export function formatRelative(iso: string | null): string {
  if (!iso) return t('agent.no_sessions_yet');
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return t('agent.no_sessions_yet');
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 60) return t('agent.just_now');
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}${t('agent.rel_minutes')}`;
  if (diffSec < 86_400) return `${Math.floor(diffSec / 3600)}${t('agent.rel_hours')}`;
  return `${Math.floor(diffSec / 86_400)}${t('agent.rel_days')}`;
}
