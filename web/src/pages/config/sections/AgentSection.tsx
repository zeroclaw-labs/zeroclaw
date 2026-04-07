import { Bot } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Toggle from '../controls/Toggle';
import NumberInput from '../controls/NumberInput';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

export default function AgentSection({ config, onUpdate }: Props) {
  const agent = (config.agent as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<Bot className="h-5 w-5" />}
      title={t('config.section.agent')}
    >
      <FieldRow label={t('config.field.compact_context')} description={t('config.field.compact_context.desc')}>
        <Toggle
          value={(agent.compact_context as boolean) ?? true}
          onChange={(v) => onUpdate('agent.compact_context', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.max_tool_iterations')} description={t('config.field.max_tool_iterations.desc')}>
        <NumberInput
          value={(agent.max_tool_iterations as number) ?? 10}
          onChange={(v) => onUpdate('agent.max_tool_iterations', v)}
          min={1}
          max={100}
        />
      </FieldRow>
      <FieldRow label={t('config.field.max_history_messages')} description={t('config.field.max_history_messages.desc')}>
        <NumberInput
          value={(agent.max_history_messages as number) ?? 50}
          onChange={(v) => onUpdate('agent.max_history_messages', v)}
          min={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.max_context_tokens')} description={t('config.field.max_context_tokens.desc')}>
        <NumberInput
          value={(agent.max_context_tokens as number) ?? 32000}
          onChange={(v) => onUpdate('agent.max_context_tokens', v)}
          min={1000}
          step={1000}
        />
      </FieldRow>
      <FieldRow label={t('config.field.parallel_tools')} description={t('config.field.parallel_tools.desc')}>
        <Toggle
          value={(agent.parallel_tools as boolean) ?? false}
          onChange={(v) => onUpdate('agent.parallel_tools', v)}
        />
      </FieldRow>
    </SectionCard>
  );
}
