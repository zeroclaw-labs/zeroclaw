import { getProviderRegistry } from './api';

export type ConfiguredModelCategory = 'models' | 'tts' | 'transcription';

export interface ConfiguredModelBinding {
  type: string;
  alias: string;
  resource: string;
}

export async function walkConfiguredModelBindings(
  category: ConfiguredModelCategory,
): Promise<ConfiguredModelBinding[]> {
  const out: ConfiguredModelBinding[] = [];
  const registry = await getProviderRegistry();
  const capability = { models: 'chat', tts: 'tts', transcription: 'asr' }[category];
  for (const [alias, model] of Object.entries(registry.models)) {
    const connection = registry.connections[model.provider];
    if (model.capability === capability && model.model && connection) {
      out.push({ type: connection.kind, alias, resource: model.model });
    }
  }
  return out;
}

export async function configuredResourceIds(
  category: ConfiguredModelCategory,
  type: string,
): Promise<string[]> {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const b of await walkConfiguredModelBindings(category)) {
    if (b.type !== type) continue;
    if (seen.has(b.resource)) continue;
    seen.add(b.resource);
    out.push(b.resource);
  }
  return out;
}
