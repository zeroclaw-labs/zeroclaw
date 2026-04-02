import { HeartPulse } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Toggle from '../controls/Toggle';
import NumberInput from '../controls/NumberInput';
import TextInput from '../controls/TextInput';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

export default function HeartbeatSection({ config, onUpdate }: Props) {
  const hb = (config.heartbeat as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<HeartPulse className="h-5 w-5" />}
      title={t('config.section.heartbeat')}
      enabled={(hb.enabled as boolean) ?? true}
      onToggleEnabled={(v) => onUpdate('heartbeat.enabled', v)}
    >
      <FieldRow label={t('config.field.interval_minutes')} description={t('config.field.interval_minutes.desc')}>
        <NumberInput
          value={(hb.interval_minutes as number) ?? 30}
          onChange={(v) => onUpdate('heartbeat.interval_minutes', v)}
          min={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.two_phase')} description={t('config.field.two_phase.desc')}>
        <Toggle
          value={(hb.two_phase as boolean) ?? true}
          onChange={(v) => onUpdate('heartbeat.two_phase', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.adaptive')} description={t('config.field.adaptive.desc')}>
        <Toggle
          value={(hb.adaptive as boolean) ?? false}
          onChange={(v) => onUpdate('heartbeat.adaptive', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.min_interval_minutes')} description={t('config.field.min_interval_minutes.desc')}>
        <NumberInput
          value={(hb.min_interval_minutes as number) ?? 5}
          onChange={(v) => onUpdate('heartbeat.min_interval_minutes', v)}
          min={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.max_interval_minutes')} description={t('config.field.max_interval_minutes.desc')}>
        <NumberInput
          value={(hb.max_interval_minutes as number) ?? 120}
          onChange={(v) => onUpdate('heartbeat.max_interval_minutes', v)}
          min={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.heartbeat_message')} description={t('config.field.heartbeat_message.desc')}>
        <TextInput
          value={(hb.message as string) ?? ''}
          onChange={(v) => onUpdate('heartbeat.message', v || undefined)}
          placeholder="Optional fallback task"
        />
      </FieldRow>
      <FieldRow label={t('config.field.heartbeat_target')} description={t('config.field.heartbeat_target.desc')}>
        <TextInput
          value={(hb.target as string) ?? ''}
          onChange={(v) => onUpdate('heartbeat.target', v || undefined)}
          placeholder="e.g. telegram"
        />
      </FieldRow>
      <FieldRow label={t('config.field.task_timeout_secs')} description={t('config.field.task_timeout_secs.desc')}>
        <NumberInput
          value={(hb.task_timeout_secs as number) ?? 600}
          onChange={(v) => onUpdate('heartbeat.task_timeout_secs', v)}
          min={0}
        />
      </FieldRow>
    </SectionCard>
  );
}
