import {
  Bot,
  Brain,
  ChevronRight,
  DollarSign,
  MessageSquare,
  Power,
  Wifi,
} from 'lucide-react';
import type { AgentSummary } from '@/lib/agents';
import { Badge } from '@/components/ui';
import { t } from '@/lib/i18n';

export interface AgentCardProps {
  agent: AgentSummary;
  /** Open the detail drawer for this agent. */
  onSelect: () => void;
  /** Highlight when this row's agent is the one shown in the drawer. */
  selected?: boolean;
}

function formatUsd(value: number | null): string {
  if (value === null) return '—';
  if (value < 0.01) return '<$0.01';
  return `$${value.toFixed(2)}`;
}

// A compact inline fact: icon + value, with a muted caption that collapses on
// the narrowest rows. Keeps the row dense but scannable.
function RowFact({
  icon: Icon,
  value,
  label,
  title,
}: {
  icon: typeof Wifi;
  value: string | number;
  label: string;
  title?: string;
}) {
  return (
    <span
      className="flex items-center gap-1 text-xs text-pc-text-secondary tabular-nums"
      title={title}
    >
      <Icon className="h-3.5 w-3.5 flex-shrink-0 text-pc-text-faint" />
      <span className="font-medium text-pc-text">{value}</span>
      <span className="hidden lg:inline text-pc-text-muted">{label}</span>
    </span>
  );
}

/**
 * Dense, scannable list row for one agent. A keyboard-focusable button: the
 * whole row opens the detail drawer. Shows identity, enabled state, and a few
 * inline facts (channels / sessions / memories / spend). Full detail + actions
 * (Open chat, Edit, the per-entity config links) live in the drawer.
 */
export default function AgentCard({ agent, onSelect, selected = false }: AgentCardProps) {
  const channelCount = agent.channels.length;

  return (
    <button
      type="button"
      onClick={onSelect}
      aria-haspopup="dialog"
      aria-label={`${t('agentcard.open_detail_prefix')} ${agent.alias} ${t('agentcard.open_detail_suffix')}`}
      className={[
        'group w-full flex items-center gap-3 px-4 py-3 text-left',
        'border-b border-pc-border last:border-b-0',
        'transition-colors duration-150 cursor-pointer',
        'hover:bg-[var(--pc-hover)]',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]',
        selected ? 'bg-pc-elevated' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      {/* Identity */}
      <div className="h-8 w-8 rounded-[var(--radius-md)] flex-shrink-0 flex items-center justify-center bg-pc-accent/10">
        <Bot className="h-4 w-4 text-pc-accent" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 min-w-0">
          <span className="text-sm font-semibold truncate text-pc-text">
            {agent.alias}
          </span>
          <Badge tone={agent.enabled ? 'ok' : 'neutral'} className="flex-shrink-0">
            <Power className="h-3 w-3" />
            {agent.enabled ? t('agent.enabled') : t('agent.disabled')}
          </Badge>
        </div>
        <span className="block text-xs truncate font-mono text-pc-text-muted">
          {agent.modelProvider || t('agent.no_model_provider')}
        </span>
      </div>

      {/* Inline facts — hidden on the narrowest viewports to keep the row clean */}
      <div className="hidden sm:flex items-center gap-4 flex-shrink-0">
        <RowFact
          icon={Wifi}
          value={channelCount}
          label={channelCount === 1 ? t('agentcard.channel') : t('agentcard.channels')}
          title={
            channelCount === 0
              ? t('agentcard.no_channels_bound')
              : `${t('agentcard.channels_title')}: ${agent.channels.join(', ')}`
          }
        />
        <RowFact
          icon={MessageSquare}
          value={agent.sessionCount}
          label={agent.sessionCount === 1 ? t('agentcard.session') : t('agentcard.sessions')}
          title={t('agentcard.active_sessions')}
        />
        <RowFact
          icon={Brain}
          value={agent.memoryCount}
          label={agent.memoryCount === 1 ? t('agentcard.memory') : t('agentcard.memories')}
          title={t('agentcard.stored_memories')}
        />
        <RowFact
          icon={DollarSign}
          value={formatUsd(agent.monthCostUsd)}
          label={t('agentcard.this_month')}
          title={
            agent.monthCostUsd === null
              ? t('agent.cost_untracked_title')
              : t('agent.cost_tracked_title')
          }
        />
      </div>

      <ChevronRight className="h-4 w-4 flex-shrink-0 text-pc-text-faint transition-colors group-hover:text-pc-text-muted" />
    </button>
  );
}
