import { MessageSquareText } from 'lucide-react';
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

function asRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' ? value as Record<string, unknown> : {};
}

function formatList(value: unknown): string {
  return Array.isArray(value) ? value.join(', ') : '';
}

function parseList(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

export default function ChannelsSection({ config, onUpdate }: Props) {
  const channels = asRecord(config.channels);
  const rocketchat = asRecord(channels.rocketchat);
  const forceReply = (channels.force_reply as boolean) ?? false;

  return (
    <SectionCard
      icon={<MessageSquareText className="h-5 w-5" />}
      title={t('config.section.channels')}
      defaultOpen
    >
      <FieldRow label={t('config.field.rocketchat_enabled')} description={t('config.field.rocketchat_enabled.desc')}>
        <Toggle
          value={(rocketchat.enabled as boolean) ?? false}
          onChange={(v) => onUpdate('channels.rocketchat.enabled', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_server_url')} description={t('config.field.rocketchat_server_url.desc')}>
        <TextInput
          value={(rocketchat.server_url as string) ?? ''}
          onChange={(v) => onUpdate('channels.rocketchat.server_url', v)}
          placeholder="https://chat.example.com"
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_user_id')} description={t('config.field.rocketchat_user_id.desc')}>
        <TextInput
          value={(rocketchat.user_id as string) ?? ''}
          onChange={(v) => onUpdate('channels.rocketchat.user_id', v)}
          placeholder="RocketChat_USER_ID"
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_auth_token')} description={t('config.field.rocketchat_auth_token.desc')}>
        <TextInput
          value={(rocketchat.auth_token as string) ?? ''}
          onChange={(v) => onUpdate('channels.rocketchat.auth_token', v)}
          placeholder="RocketChat_TOKEN"
          masked
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_allowed_rooms')} description={t('config.field.rocketchat_allowed_rooms.desc')}>
        <TextInput
          value={formatList(rocketchat.allowed_rooms)}
          onChange={(v) => onUpdate('channels.rocketchat.allowed_rooms', parseList(v))}
          placeholder="GENERAL, support"
          commitOnBlur
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_allowed_users')} description={t('config.field.rocketchat_allowed_users.desc')}>
        <TextInput
          value={formatList(rocketchat.allowed_users)}
          onChange={(v) => onUpdate('channels.rocketchat.allowed_users', parseList(v))}
          placeholder="*, alice, bob"
          commitOnBlur
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_dm_replies')} description={t('config.field.rocketchat_dm_replies.desc')}>
        <Toggle
          value={(rocketchat.dm_replies as boolean) ?? false}
          onChange={(v) => onUpdate('channels.rocketchat.dm_replies', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_mention_only')} description={t('config.field.rocketchat_mention_only.desc')}>
        <Toggle
          value={(rocketchat.mention_only as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.mention_only', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_thread_replies')} description={t('config.field.rocketchat_thread_replies.desc')}>
        <Toggle
          value={(rocketchat.thread_replies as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.thread_replies', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_discussion_replies')} description={t('config.field.rocketchat_discussion_replies.desc')}>
        <Toggle
          value={(rocketchat.discussion_replies as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.discussion_replies', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_typing_indicator')} description={t('config.field.rocketchat_typing_indicator.desc')}>
        <Toggle
          value={(rocketchat.typing_indicator as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.typing_indicator', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_thinking_placeholder')} description={t('config.field.rocketchat_thinking_placeholder.desc')}>
        <Toggle
          value={(rocketchat.thinking_placeholder as boolean) ?? false}
          onChange={(v) => onUpdate('channels.rocketchat.thinking_placeholder', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_ack_reactions')} description={t('config.field.rocketchat_ack_reactions.desc')}>
        <Toggle
          value={(rocketchat.ack_reactions as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.ack_reactions', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_poll_interval_ms')} description={t('config.field.rocketchat_poll_interval_ms.desc')}>
        <NumberInput
          value={(rocketchat.poll_interval_ms as number) ?? 1500}
          onChange={(v) => onUpdate('channels.rocketchat.poll_interval_ms', v)}
          min={250}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_download_media')} description={t('config.field.rocketchat_download_media.desc')}>
        <Toggle
          value={(rocketchat.download_media as boolean) ?? true}
          onChange={(v) => onUpdate('channels.rocketchat.download_media', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.rocketchat_max_media_bytes')} description={t('config.field.rocketchat_max_media_bytes.desc')}>
        <NumberInput
          value={(rocketchat.max_media_bytes as number) ?? 26214400}
          onChange={(v) => onUpdate('channels.rocketchat.max_media_bytes', v)}
          min={0}
        />
      </FieldRow>
      <FieldRow label={t('config.field.channels_ack_reactions')} description={t('config.field.channels_ack_reactions.desc')}>
        <Toggle
          value={(channels.ack_reactions as boolean) ?? true}
          onChange={(v) => onUpdate('channels.ack_reactions', v)}
        />
      </FieldRow>
      <FieldRow label={t('config.field.channels_force_reply')} description={t('config.field.channels_force_reply.desc')}>
        <Toggle
          value={forceReply}
          onChange={(v) => onUpdate('channels.force_reply', v)}
        />
      </FieldRow>
    </SectionCard>
  );
}
