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
  'risk-profile':      { section: 'risk-profiles',    shape: 'alias'  },
  'runtime-profile':   { section: 'runtime-profiles', shape: 'alias'  },
  'skill-bundle':      { section: 'skill-bundles',    shape: 'alias'  },
  'knowledge-bundle':  { section: 'knowledge-bundles',shape: 'alias'  },
  'mcp-bundle':        { section: 'mcp-bundles',      shape: 'alias'  },
  'peer-group':        { section: 'peer-groups',      shape: 'alias'  },
  cron:                { section: 'cron',             shape: 'alias'  },
} as const satisfies Record<string, { section: string; shape: Shape }>;

export type EntityKind = keyof typeof ENTITY_SECTION;

export function entityConfigPath(kind: EntityKind, id: string): string {
  const { section, shape } = ENTITY_SECTION[kind];
  if (shape === 'picker') return `/config/${section}`;
  if (shape === 'typed') {
    const dot = id.indexOf('.');
    if (dot > 0) {
      return `/config/${section}/${encodeURIComponent(id.slice(0, dot))}/${encodeURIComponent(id.slice(dot + 1))}`;
    }
    return `/config/${section}`;
  }
  return `/config/${section}/${encodeURIComponent(id)}`;
}
