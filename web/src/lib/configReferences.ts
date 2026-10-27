import type { ListResponseEntry } from './api';

const ALIAS_REF_TYPE_TO_SOURCE: Record<string, string> = {
  ModelProviderRef: 'model_providers',
  TtsProviderRef: 'tts_providers',
  TranscriptionProviderRef: 'transcription_providers',
  RiskProfileRef: 'risk_profiles',
  RuntimeProfileRef: 'runtime_profiles',
  ChannelRef: 'channels',
};

// The `resolve-alias-source` query value for an alias-ref entry: prefer the
// daemon-declared `alias_source`; else derive it from the `<Type>Ref` type_hint.
// Returns null for non-alias-ref entries or unmapped ref types.
export function aliasRefSource(entry: ListResponseEntry): string | null {
  if (entry.kind !== 'alias-ref') return null;
  if (entry.alias_source) return entry.alias_source;
  const m = entry.type_hint?.match(/(\w+Ref)\b/);
  return (m && ALIAS_REF_TYPE_TO_SOURCE[m[1] ?? '']) ?? null;
}

/** Route namespace of a schema-declared reference, shared by contextual search.
 * Only populated, non-secret reference values can identify an owning form.
 */
export function referenceConfigPrefix(entry: ListResponseEntry): string | null {
  const source = aliasRefSource(entry);
  if (
    !source ||
    !entry.populated ||
    entry.is_secret ||
    typeof entry.value !== 'string' ||
    !entry.value ||
    entry.value === '<unset>'
  )
    return null;
  const namespaces: Record<string, string> = {
    model_providers: 'providers.models',
    tts_providers: 'providers.tts',
    transcription_providers: 'providers.transcription',
    risk_profiles: 'risk_profiles',
    runtime_profiles: 'runtime_profiles',
    channels: 'channels',
  };
  return namespaces[source] ? `${namespaces[source]}.${entry.value}` : null;
}
