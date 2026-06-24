import { getCost, getMapKeys, getMemory, getSessions, listProps, patchConfig } from './api';

export interface AgentSummary {
  alias: string;
  enabled: boolean;
  modelProvider: string;
  channels: string[];
  riskProfile: string;
  runtimeProfile: string;
  /** Memory backend kind from `[agents.<alias>.memory].backend`. Empty
   * string when unset (the agent inherits the default — sqlite). */
  memoryBackend: string;
  skillBundles: string[];
  knowledgeBundles: string[];
  mcpBundles: string[];
  /** Cron alias list from `[agents.<alias>].cron_jobs`. */
  cronJobs: string[];
  /** Peer-group aliases this agent appears in (reverse-resolved by
   * walking `[peer-groups.<alias>].agents`). */
  peerGroups: string[];
  sessionCount: number;
  lastActivity: string | null;
  monthCostUsd: number | null;
  /** Persisted memory rows attributed to this agent via `agent_alias`. */
  memoryCount: number;
}

function entryValue(entry: { populated?: boolean; value?: unknown }): unknown {
  if (!entry.populated) return undefined;
  return entry.value;
}

// `listProps` returns array values as a JSON-encoded string (the macro's
// display_value), not a parsed array. Decode here so callers can `Array.isArray`.
function entryAsStringArray(entry: { populated?: boolean; value?: unknown } | undefined): string[] {
  if (!entry || !entry.populated) return [];
  const raw = entry.value;
  if (Array.isArray(raw)) return raw.map((v) => String(v));
  if (typeof raw !== 'string' || raw.length === 0) return [];
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.map((v) => String(v));
  } catch {
    // fall through to comma/newline split for hand-typed display formats
  }
  return raw
    .replace(/^\[|\]$/g, '')
    .split(/[,\n]/)
    .map((s) => s.trim().replace(/^"|"$/g, ''))
    .filter((s) => s.length > 0);
}

/**
 * Load summaries for every configured agent. One round-trip to fetch the
 * alias list, one per alias for its fields. Suitable for dashboards and
 * pickers; not suitable for the highest-traffic page in the app.
 */
export async function loadAgentSummaries(): Promise<AgentSummary[]> {
  const { keys } = await getMapKeys('agents');
  if (keys.length === 0) return [];

  // Fetch sessions + cost + memories in parallel with per-agent prop
  // lookups. Falls back to empty/null if any endpoint errors so a partial
  // outage doesn't blank the agents page.
  const sessionsPromise = getSessions().catch(() => []);
  const costPromise = getCost().catch(() => null);
  const memoriesPromise = getMemory().catch(() => []);

  // Reverse-build agent → peer-groups in parallel with the per-agent walks.
  // listProps('peer-groups.<alias>.agents') is the field that names members.
  const peerGroupsPromise = getMapKeys('peer-groups')
    .then(async ({ keys: pgKeys }) => {
      const memberships: Record<string, string[]> = {};
      await Promise.all(
        pgKeys.map(async (pg) => {
          const { entries } = await listProps(`peer-groups.${pg}`);
          const agentsEntry = entries.find(
            (e) => e.path === `peer-groups.${pg}.agents`,
          );
          for (const a of entryAsStringArray(agentsEntry)) {
            (memberships[a] ||= []).push(pg);
          }
        }),
      );
      return memberships;
    })
    .catch(() => ({}) as Record<string, string[]>);

  const summaries = await Promise.all(
    keys.map(async (alias): Promise<AgentSummary> => {
      const { entries } = await listProps(`agents.${alias}`);
      // Configurable-macro paths are kebab-case (snake field names
      // converted via snake_to_kebab in zeroclaw-macros).
      const lookup = (suffixKebab: string) =>
        entries.find((e) => e.path === `agents.${alias}.${suffixKebab}`);
      const stringField = (suffixKebab: string): string => {
        const raw = entryValue(lookup(suffixKebab) ?? { populated: false });
        return typeof raw === 'string' ? raw : '';
      };
      return {
        alias,
        enabled: entryValue(lookup('enabled') ?? { populated: false }) === 'true',
        modelProvider: stringField('model-provider'),
        channels: entryAsStringArray(lookup('channels')),
        riskProfile: stringField('risk-profile'),
        runtimeProfile: stringField('runtime-profile'),
        memoryBackend: stringField('memory.backend'),
        skillBundles: entryAsStringArray(lookup('skill-bundles')),
        knowledgeBundles: entryAsStringArray(lookup('knowledge-bundles')),
        mcpBundles: entryAsStringArray(lookup('mcp-bundles')),
        cronJobs: entryAsStringArray(lookup('cron-jobs')),
        peerGroups: [],
        sessionCount: 0,
        lastActivity: null,
        monthCostUsd: null,
        memoryCount: 0,
      };
    }),
  );

  const [sessions, cost, peerGroups, memories] = await Promise.all([
    sessionsPromise,
    costPromise,
    peerGroupsPromise,
    memoriesPromise,
  ]);
  const memoriesByAgent = memories.reduce<Record<string, number>>((acc, m) => {
    if (m.agent_alias) {
      acc[m.agent_alias] = (acc[m.agent_alias] ?? 0) + 1;
    }
    return acc;
  }, {});
  for (const summary of summaries) {
    const owned = sessions.filter((s) => s.agent_alias === summary.alias);
    summary.sessionCount = owned.length;
    summary.lastActivity = owned.reduce<string | null>((acc, s) => {
      if (!acc) return s.last_activity;
      return s.last_activity > acc ? s.last_activity : acc;
    }, null);

    const agentCost = cost?.by_agent?.[summary.alias];
    summary.monthCostUsd = agentCost ? agentCost.cost_usd : null;
    summary.peerGroups = peerGroups[summary.alias] ?? [];
    summary.memoryCount = memoriesByAgent[summary.alias] ?? 0;
  }

  return summaries;
}

/** Flip the `enabled` flag for one agent via a JSON-Patch replace. */
export function toggleAgentEnabled(alias: string, next: boolean): Promise<unknown> {
  return patchConfig([
    {
      op: 'replace',
      path: `/agents/${alias}/enabled`,
      value: next,
    },
  ]);
}
