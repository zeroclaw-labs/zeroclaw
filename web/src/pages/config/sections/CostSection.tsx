import { DollarSign } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Toggle from '../controls/Toggle';
import NumberInput from '../controls/NumberInput';
import Slider from '../controls/Slider';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

export default function CostSection({ config, onUpdate }: Props) {
  const cost = (config.cost as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<DollarSign className="h-5 w-5" />}
      title={t('config.section.cost')}
      enabled={(cost.enabled as boolean) ?? true}
      onToggleEnabled={(v) => onUpdate('cost.enabled', v)}
    >
      <FieldRow label={t('config.field.daily_limit_usd')} description={t('config.field.daily_limit_usd.desc')}>
        <NumberInput
          value={(cost.daily_limit_usd as number) ?? 10.0}
          onChange={(v) => onUpdate('cost.daily_limit_usd', v)}
          min={0}
          step={1}
        />
      </FieldRow>
      <FieldRow label={t('config.field.monthly_limit_usd')} description={t('config.field.monthly_limit_usd.desc')}>
        <NumberInput
          value={(cost.monthly_limit_usd as number) ?? 100.0}
          onChange={(v) => onUpdate('cost.monthly_limit_usd', v)}
          min={0}
          step={5}
        />
      </FieldRow>
      <FieldRow label={t('config.field.warn_at_percent')} description={t('config.field.warn_at_percent.desc')}>
        <Slider
          value={(cost.warn_at_percent as number) ?? 80}
          onChange={(v) => onUpdate('cost.warn_at_percent', v)}
          min={0}
          max={100}
          step={5}
        />
      </FieldRow>
      <FieldRow label={t('config.field.allow_override')} description={t('config.field.allow_override.desc')}>
        <Toggle
          value={(cost.allow_override as boolean) ?? false}
          onChange={(v) => onUpdate('cost.allow_override', v)}
        />
      </FieldRow>
    </SectionCard>
  );
}
