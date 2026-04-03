import { Brain } from 'lucide-react';
import SectionCard from '../controls/SectionCard';
import FieldRow from '../controls/FieldRow';
import Select from '../controls/Select';
import { t } from '@/lib/i18n';

interface Props {
  config: Record<string, unknown>;
  onUpdate: (field: string, value: unknown) => void;
}

const BACKEND_OPTIONS = [
  { value: 'sqlite', label: 'SQLite' },
  { value: 'markdown', label: 'Markdown' },
  { value: 'embeddings', label: 'Embeddings' },
  { value: 'hybrid', label: 'Hybrid' },
];

export default function MemorySection({ config, onUpdate }: Props) {
  const memory = (config.memory as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<Brain className="h-5 w-5" />}
      title={t('config.section.memory')}
      enabled={(memory.enabled as boolean) ?? true}
      onToggleEnabled={(v) => onUpdate('memory.enabled', v)}
    >
      <FieldRow label={t('config.field.memory_backend')} description={t('config.field.memory_backend.desc')}>
        <Select
          value={(memory.backend as string) ?? 'sqlite'}
          onChange={(v) => onUpdate('memory.backend', v)}
          options={BACKEND_OPTIONS}
        />
      </FieldRow>
    </SectionCard>
  );
}
