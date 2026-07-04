import { getMapKeys, getProp, getTemplates } from './api';

export type ConfiguredModelCategory = 'models' | 'tts' | 'transcription';

export interface ConfiguredModelBinding {
  type: string;
  alias: string;
  resource: string;
}

export async function walkConfiguredModelBindings(
  category: ConfiguredModelCategory,
): Promise<ConfiguredModelBinding[]> {
  const root = `providers.${category}`;
  const out: ConfiguredModelBinding[] = [];
  // providers.<category> isn't a map-keyed section; the typed wrapper
  // exposes one HashMap per provider type. Read the slot list from
  // map_key_sections via the templates endpoint.
  let types: string[];
  try {
    const { templates } = await getTemplates();
    const prefix = `${root}.`;
    types = templates
      .filter((t) => t.kind === 'map' && t.path.startsWith(prefix))
      .map((t) => t.path.slice(prefix.length))
      .filter((t) => t && !t.includes('.'));
  } catch {
    return out;
  }
  for (const type of types) {
    let aliases: string[];
    try {
      aliases = (await getMapKeys(`${root}.${type}`)).keys;
    } catch {
      continue;
    }
    const results = await Promise.all(
      aliases.map((alias) =>
        getProp(`${root}.${type}.${alias}.model`).catch(() => null),
      ),
    );
    aliases.forEach((alias, i) => {
      const r = results[i];
      const v = r && typeof r.value === 'string' ? r.value : '';
      if (v && v !== '<unset>') {
        out.push({ type, alias, resource: v });
      }
    });
  }
  return out;
}

export async function resolveModelToProviderType(
  category: ConfiguredModelCategory,
): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  for (const b of await walkConfiguredModelBindings(category)) {
    if (!(b.resource in out)) out[b.resource] = b.type;
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
