import { Search } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Select from '../controls/Select';
import Slider from '../controls/Slider';
import NumberInput from '../controls/NumberInput';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

const PROVIDER_OPTIONS = [
  { value: 'duckduckgo', label: 'DuckDuckGo' },
  { value: 'brave', label: 'Brave' },
  { value: 'searxng', label: 'SearXNG' },
];

export default function WebSearchSection({ config, onUpdate }: Props) {
  const ws = (config.web_search as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<Search className="h-5 w-5" />}
      title={t('config.section.web_search')}
      enabled={(ws.enabled as boolean) ?? true}
      onToggleEnabled={(v) => onUpdate('web_search.enabled', v)}
    >
      <FieldRow label={t('config.field.web_search_provider')} description={t('config.field.web_search_provider.desc')}>
        <Select
          value={(ws.provider as string) ?? 'duckduckgo'}
          onChange={(v) => onUpdate('web_search.provider', v)}
          options={PROVIDER_OPTIONS}
        />
      </FieldRow>
      <FieldRow label={t('config.field.max_results')} description={t('config.field.max_results.desc')}>
        <Slider
          value={(ws.max_results as number) ?? 5}
          onChange={(v) => onUpdate('web_search.max_results', v)}
          min={1}
          max={10}
        />
      </FieldRow>
      <FieldRow label={t('config.field.web_search_timeout')} description={t('config.field.web_search_timeout.desc')}>
        <NumberInput
          value={(ws.timeout_secs as number) ?? 15}
          onChange={(v) => onUpdate('web_search.timeout_secs', v)}
          min={1}
        />
      </FieldRow>
    </SectionCard>
  );
}
