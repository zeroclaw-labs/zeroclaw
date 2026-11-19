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
      out.push({ type: rateProviderFamily(connection.kind), alias, resource: model.model });
    }
  }
  return out;
}

/** Rate sheets use `custom` for every OpenAI-compatible connection. */
function rateProviderFamily(kind: string): string {
  return kind === 'openai-compatible' ? 'custom' : kind;
}

/**
 * Resolve the canonical model resource to its connection kind.
 *
 * The registry is the source of truth: model aliases point at a connection,
 * and the connection owns the protocol/provider kind used by the dashboard.
 * Keep the small map-shaped helper for callers that only need navigation
 * metadata; it must not reintroduce the retired providers.<category> lookup.
 */
export async function resolveModelToProviderType(
  category: ConfiguredModelCategory,
): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  for (const binding of await walkConfiguredModelBindings(category)) {
    if (!(binding.resource in out)) out[binding.resource] = binding.type;
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
