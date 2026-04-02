import { Server } from 'lucide-react';
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

export default function GatewaySection({ config, onUpdate }: Props) {
  const gw = (config.gateway as Record<string, unknown>) ?? {};

  return (
    <SectionCard
      icon={<Server className="h-5 w-5" />}
      title={t('config.section.gateway')}
    >
      <FieldRow label={t('config.field.gateway_port')} description={t('config.field.gateway_port.desc')}>
        <NumberInput
          value={(gw.port as number) ?? 42617}
          onChange={(v) => onUpdate('gateway.port', v)}
          min={1}
          max={65535}
        />
      </FieldRow>
      <FieldRow label={t('config.field.gateway_host')} description={t('config.field.gateway_host.desc')}>
        <TextInput
          value={(gw.host as string) ?? '127.0.0.1'}
          onChange={(v) => onUpdate('gateway.host', v)}
          placeholder="127.0.0.1"
        />
      </FieldRow>
      <FieldRow label={t('config.field.require_pairing')} description={t('config.field.require_pairing.desc')}>
        <Toggle
          value={(gw.require_pairing as boolean) ?? true}
          onChange={(v) => onUpdate('gateway.require_pairing', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.session_persistence')} description={t('config.field.session_persistence.desc')}>
        <Toggle
          value={(gw.session_persistence as boolean) ?? true}
          onChange={(v) => onUpdate('gateway.session_persistence', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.session_ttl_hours')} description={t('config.field.session_ttl_hours.desc')}>
        <NumberInput
          value={(gw.session_ttl_hours as number) ?? 0}
          onChange={(v) => onUpdate('gateway.session_ttl_hours', v)}
          min={0}
        />
      </FieldRow>
      <FieldRow label={t('config.field.allow_public_bind')} description={t('config.field.allow_public_bind.desc')}>
        <Toggle
          value={(gw.allow_public_bind as boolean) ?? false}
          onChange={(v) => onUpdate('gateway.allow_public_bind', v)}
        />
      </FieldRow>
    </SectionCard>
  );
}
