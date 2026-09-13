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

export function entityConfigPath(kind: EntityKind, id: string): string {
  const { section, shape } = ENTITY_SECTION[kind];
  if (shape === 'picker') return `/config/${section}`;
  if (shape === 'typed') {
    // A model-provider ref may be three-segment (`<type>.<alias>.<model>`);
    // the edit surface is always the profile page keyed by `<type>/<alias>`,
    // so route on the first two segments and drop any `<model>`. Other typed
    // sections (channels) are always two-segment `<type>.<alias>`.
    if (kind === 'model-provider') {
      const [type, alias] = id.split('.');
      if (type && alias) {
        return `/config/${section}/${encodeURIComponent(type)}/${encodeURIComponent(alias)}`;
      }
      return `/config/${section}`;
    }
    const dot = id.indexOf('.');
    if (dot > 0) {
      return `/config/${section}/${encodeURIComponent(id.slice(0, dot))}/${encodeURIComponent(id.slice(dot + 1))}`;
    }
    return `/config/${section}`;
  }
  return `/config/${section}/${encodeURIComponent(id)}`;
}
