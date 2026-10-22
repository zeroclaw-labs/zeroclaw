// Resolves an entity reference (agent alias, channel composite, model
// provider composite, profile / bundle alias) to its `/config/...` edit
// surface. The route shape follows `Section::shape()` in
// `crates/zeroclaw-config/src/sections.rs`:
//
//   alias    → /config/<section>/<id>
//   typed    → /config/<section>/<type>/<alias>   (id = `<type>.<alias>`)
//   picker   → /config/<section>                  (no per-alias detail)
//
// Adding a new entity kind is one row in `ENTITY_SECTION`.

type Shape = 'alias' | 'typed' | 'picker';

const ENTITY_SECTION = {
  agent:               { section: 'agents',           shape: 'alias'  },
  channel:             { section: 'channels',         shape: 'typed'  },
  'model-provider':    { section: 'providers.models', shape: 'typed'  },
  'memory-backend':    { section: 'memory',           shape: 'picker' },
  'risk-profile':      { section: 'risk_profiles',    shape: 'alias'  },
  'runtime-profile':   { section: 'runtime_profiles', shape: 'alias'  },
  'skill-bundle':      { section: 'skill_bundles',    shape: 'alias'  },
  'knowledge-bundle':  { section: 'knowledge_bundles',shape: 'alias'  },
  'mcp-bundle':        { section: 'mcp_bundles',      shape: 'alias'  },
  'peer-group':        { section: 'peer_groups',      shape: 'alias'  },
  cron:                { section: 'cron',             shape: 'alias'  },
} as const satisfies Record<string, { section: string; shape: Shape }>;

export type EntityKind = keyof typeof ENTITY_SECTION;

export function modelProfileRef(id: string, aliases: readonly string[]): string | undefined {
  const dot = id.indexOf('.');
  if (dot < 1) return undefined;
  const tail = id.slice(dot + 1);
  const alias = aliases.filter((candidate) => tail === candidate || tail.startsWith(`${candidate}.`))
    .sort((a, b) => b.length - a.length)[0];
  return alias ? `${id.slice(0, dot)}.${alias}` : undefined;
}

/** Property paths are flattened config paths, not TOML source to tokenize.
 * Resolve the dynamic key from configured aliases before examining its fields.
 */
export function modelCatalogTarget(path: string, aliases: readonly string[]): { family: string; alias: string; array: boolean } | undefined {
  const reference = path.startsWith('providers.models.') ? path.slice('providers.models.'.length) : '';
  const profile = modelProfileRef(reference, aliases);
  if (!profile) return undefined;
  const field = reference.slice(profile.length);
  if (field !== '.model' && field !== '.fallback_models' && !/^\.models\..+\.id$/.test(field)) return undefined;
  const dot = profile.indexOf('.');
  return { family: profile.slice(0, dot), alias: profile.slice(dot + 1), array: field === '.fallback_models' };
}

export function entityConfigPath(kind: EntityKind, id: string, modelAliases: readonly string[] = []): string {
  const { section, shape } = ENTITY_SECTION[kind];
  if (shape === 'picker') return `/config/${section}`;
  if (shape === 'typed') {
    if (kind === 'model-provider') {
      const profile = modelProfileRef(id, modelAliases);
      if (!profile) return `/config/${section}`;
      const dot = profile.indexOf('.');
      return `/config/${section}/${encodeURIComponent(profile.slice(0, dot))}/${encodeURIComponent(profile.slice(dot + 1))}`;
    }
    const dot = id.indexOf('.');
    if (dot > 0) {
      return `/config/${section}/${encodeURIComponent(id.slice(0, dot))}/${encodeURIComponent(id.slice(dot + 1))}`;
    }
    return `/config/${section}`;
  }
  return `/config/${section}/${encodeURIComponent(id)}`;
}
