import { useState, useEffect, useCallback } from 'react';
import { Link } from 'react-router-dom';
import {
  Wrench,
  Search,
  ChevronDown,
  ChevronRight,
  Terminal,
  Package,
  ArrowRight,
  ShieldCheck,
  ShieldX,
  ExternalLink,
} from 'lucide-react';
import type { ToolSpec, CliTool } from '@/types/api';
import {
  getTools,
  getCliTools,
  getMapKeys,
  listProps,
  patchConfig,
  ApiError,
} from '@/lib/api';
import { loadAgentPickerSummaries, type AgentPickerSummary } from '@/lib/agents';
import { t } from '@/lib/i18n';
import { Badge, Card, PageHeader } from '@/components/ui';

// ── Risk-profile tool access ────────────────────────────────────────────
// Per-profile allow/exclude state for the tool-access matrix in each expanded
// tool card. zeroclaw's gate (crates/zeroclaw-config policy + runtime):
//   • allowed_tools EMPTY  → unrestricted (every tool allowed)
//   • allowed_tools [list] → only those tools allowed
//   • excluded_tools       → denylist, wins over allow
// So we never silently convert an unrestricted profile into an allowlist:
// BLOCK adds to excluded_tools (no side effects on other tools); ALLOW clears
// the exclusion and, only when the profile is already an allowlist, adds the
// tool to it.
interface ProfileAccess {
  allowed: string[];
  excluded: string[];
}

function parseStrArray(raw: unknown): string[] {
  if (Array.isArray(raw)) return raw.map(String);
  if (typeof raw !== 'string' || raw.length === 0 || raw === '<unset>') return [];
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.map(String);
  } catch {
    // fall through to lenient parse
  }
  return raw
    .replace(/^\[|\]$/g, '')
    .split(/[,\n]/)
    .map((s) => s.trim().replace(/^"|"$/g, ''))
    .filter(Boolean);
}

function isToolAllowed(tool: string, a: ProfileAccess): boolean {
  if (a.excluded.includes(tool)) return false;
  if (a.allowed.length === 0) return true; // unrestricted
  return a.allowed.includes(tool);
}

function accessReason(tool: string, a: ProfileAccess): string {
  if (a.excluded.includes(tool)) return t('tools.reason_excluded');
  if (a.allowed.length === 0) return t('tools.reason_all_allowed');
  return a.allowed.includes(tool) ? t('tools.reason_in_allowlist') : t('tools.reason_not_in_allowlist');
}

export default function Tools() {
  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [cliTools, setCliTools] = useState<CliTool[]>([]);
  const [search, setSearch] = useState('');
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [agentSectionOpen, setAgentSectionOpen] = useState(true);
  const [cliSectionOpen, setCliSectionOpen] = useState(true);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Agent selector. Empty string = the gateway's default agent listing
  // (no `?agent=`). The agent-tools list re-fetches scoped to the pick so
  // each agent's own tools (built-ins + its `mcp_bundles` MCP tools) show,
  // instead of one arbitrary agent's.
  const [agents, setAgents] = useState<AgentPickerSummary[]>([]);
  const [selectedAgent, setSelectedAgent] = useState('');

  // Risk-profile access, keyed by profile name. `null` until loaded.
  const [access, setAccess] = useState<Record<string, ProfileAccess> | null>(null);
  const [accessError, setAccessError] = useState<string | null>(null);

  // Agent list for the selector (non-fatal: the page still works as the
  // default listing if this fails).
  useEffect(() => {
    loadAgentPickerSummaries()
      .then(setAgents)
      .catch(() => setAgents([]));
  }, []);

  // CLI tools are not agent-scoped, so load them once.
  useEffect(() => {
    getCliTools()
      .then(setCliTools)
      .catch((err) => setError(err.message));
  }, []);

  // Agent tools re-fetch whenever the selected agent changes.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getTools(selectedAgent || undefined)
      .then((toolList) => { if (!cancelled) setTools(toolList); })
      .catch((err) => { if (!cancelled) setError(err.message); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [selectedAgent]);

  // Load every risk profile's allowed/excluded tool lists for the matrix.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const { keys } = await getMapKeys('risk_profiles');
        const entriesPerProfile = await Promise.all(
          keys.map(async (name) => {
            const { entries } = await listProps(`risk_profiles.${name}`);
            const allowed = parseStrArray(
              entries.find((e) => e.path === `risk_profiles.${name}.allowed_tools`)?.value,
            );
            const excluded = parseStrArray(
              entries.find((e) => e.path === `risk_profiles.${name}.excluded_tools`)?.value,
            );
            return [name, { allowed, excluded }] as const;
          }),
        );
        if (!cancelled) setAccess(Object.fromEntries(entriesPerProfile));
      } catch (e) {
        if (!cancelled) {
          setAccessError(e instanceof Error ? e.message : String(e));
        }
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // Flip a tool's effective allow state in one profile, writing only the
  // fields that actually change. Optimistic; reverts on failure.
  const toggleAccess = useCallback(
    async (profile: string, tool: string, makeAllowed: boolean) => {
      const current = access?.[profile];
      if (!current) return;
      const allowed = [...current.allowed];
      let excluded = [...current.excluded];
      if (makeAllowed) {
        excluded = excluded.filter((x) => x !== tool);
        if (allowed.length > 0 && !allowed.includes(tool)) allowed.push(tool);
      } else if (!excluded.includes(tool)) {
        excluded.push(tool);
      }
      const ops: Parameters<typeof patchConfig>[0] = [];
      if (JSON.stringify(allowed) !== JSON.stringify(current.allowed)) {
        ops.push({ op: 'replace', path: `risk_profiles.${profile}.allowed_tools`, value: allowed });
      }
      if (JSON.stringify(excluded) !== JSON.stringify(current.excluded)) {
        ops.push({
          op: 'replace',
          path: `risk_profiles.${profile}.excluded_tools`,
          value: excluded.length > 0 ? excluded : null,
        });
      }
      if (ops.length === 0) return;
      const next = { allowed, excluded };
      setAccess((prev) => (prev ? { ...prev, [profile]: next } : prev));
      setAccessError(null);
      try {
        await patchConfig(ops);
      } catch (e) {
        // Revert on failure.
        setAccess((prev) => (prev ? { ...prev, [profile]: current } : prev));
        setAccessError(
          e instanceof ApiError
            ? `[${e.envelope.code}] ${e.envelope.message}`
            : e instanceof Error
              ? e.message
              : String(e),
        );
      }
    },
    [access],
  );

  const filtered = tools.filter((t) =>
    t.name.toLowerCase().includes(search.toLowerCase()) ||
    t.description.toLowerCase().includes(search.toLowerCase()),
  );

  const filteredCli = cliTools.filter((t) =>
    t.name.toLowerCase().includes(search.toLowerCase()) ||
    t.category.toLowerCase().includes(search.toLowerCase()),
  );

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error">
          {t('tools.load_error')}: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="h-8 w-8 border-2 rounded-full animate-spin border-pc-border" style={{ borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <PageHeader
        title={t('tools.title')}
        description={
          <>
            {t('tools.description_prefix')}{' '}
            <code className="rounded-[var(--radius-sm)] px-1 py-0.5 text-[0.85em] font-mono bg-pc-code text-pc-text-secondary">
              risk_profiles.&lt;name&gt;.allowed_tools
            </code>
            {t('tools.description_suffix')}
          </>
        }
        actions={
          <div className="flex items-center gap-2 flex-wrap justify-end">
            {agents.length > 0 && (
              <select
                value={selectedAgent}
                onChange={(e) => setSelectedAgent(e.target.value)}
                className="h-9 min-w-0 max-w-full rounded-[var(--radius-md)] border border-pc-border bg-pc-input px-3 text-sm font-medium text-pc-text-secondary transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30"
                aria-label={t('tools.agent_select_label')}
                title={t('tools.agent_select_label')}
              >
                <option value="">{t('tools.agent_select_default')}</option>
                {agents.map((a) => (
                  <option key={a.alias} value={a.alias} disabled={!a.enabled}>
                    {a.alias}{a.enabled ? '' : ` (${t('tools.agent_disabled')})`}
                  </option>
                ))}
              </select>
            )}
            <div className="relative w-64 max-w-full">
              <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-pc-text-faint pointer-events-none" />
              <input
                type="text"
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t('tools.search')}
                className="w-full h-9 pl-9 pr-3 text-sm rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30"
              />
            </div>
            {/* Exit path: tool access is configured per risk profile, so send
                the operator to the risk-profiles config section. */}
            <Link
              to="/config/risk_profiles"
              className="inline-flex items-center justify-center gap-1.5 h-9 px-3.5 text-sm font-medium whitespace-nowrap rounded-[var(--radius-md)] border border-pc-border bg-transparent text-pc-text-secondary transition-colors duration-150 hover:bg-[var(--pc-hover)] hover:text-pc-text hover:border-pc-border-strong focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
            >
              {t('tools.configure_access')}
              <ArrowRight className="h-3.5 w-3.5" />
            </Link>
          </div>
        }
      />

      {/* Agent Tools Grid */}
      <section>
        <button
          onClick={() => setAgentSectionOpen((v) => !v)}
          type="button"
          className="flex items-center gap-2 mb-4 w-full text-left group cursor-pointer"
          aria-expanded={agentSectionOpen}
          aria-controls="agent-tools-section"
        >
          <Wrench className="h-4 w-4 text-pc-accent" />
          <span className="text-xs font-semibold uppercase tracking-wider flex-1 text-pc-text-secondary" role="heading" aria-level={2}>
            {t('tools.agent_tools')}
          </span>
          <Badge tone="neutral">{filtered.length}</Badge>
          <ChevronDown
            className="h-4 w-4 text-pc-text-muted transition-transform"
            style={{ transform: agentSectionOpen ? 'rotate(0deg)' : 'rotate(-90deg)' }}
          />
        </button>

        <div id="agent-tools-section">
          {agentSectionOpen && (filtered.length === 0 ? (
            <p className="text-sm text-pc-text-muted">{t('tools.empty')}</p>
          ) : (
            <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
              {filtered.map((tool) => {
                const isExpanded = expandedTool === tool.name;
                return (
                  <Card key={tool.name} padded={false} className="overflow-hidden">
                    <button
                      onClick={() => setExpandedTool(isExpanded ? null : tool.name)}
                      type="button"
                      className="w-full text-left p-4 transition-colors hover:bg-pc-elevated/50 cursor-pointer"
                    >
                      <div className="flex items-start justify-between gap-2">
                        <div className="flex items-center gap-2 min-w-0">
                          <Package className="h-4 w-4 flex-shrink-0 text-pc-text-muted" />
                          <h3 className="text-sm font-medium truncate text-pc-text">{tool.name}</h3>
                        </div>
                        {isExpanded
                          ? <ChevronDown className="h-4 w-4 flex-shrink-0 text-pc-text-muted" />
                          : <ChevronRight className="h-4 w-4 flex-shrink-0 text-pc-text-faint" />
                        }
                      </div>
                      <p className="text-sm mt-2 line-clamp-2 text-pc-text-muted">
                        {tool.description}
                      </p>
                    </button>

                    {isExpanded && (
                      <div className="border-t border-pc-border p-4 space-y-4">
                        <ToolAccessMatrix
                          tool={tool.name}
                          access={access}
                          accessError={accessError}
                          onToggle={toggleAccess}
                        />
                        {tool.parameters && (
                          <details className="group/schema">
                            <summary className="cursor-pointer list-none text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint hover:text-pc-text-muted flex items-center gap-1">
                              <ChevronRight className="h-3 w-3 transition-transform group-open/schema:rotate-90" />
                              {t('tools.parameter_schema')}
                            </summary>
                            <pre className="mt-2 text-xs rounded-[var(--radius-md)] p-3 overflow-x-auto max-h-64 overflow-y-auto font-mono bg-pc-code text-pc-text-secondary">
                              {JSON.stringify(tool.parameters, null, 2)}
                            </pre>
                          </details>
                        )}
                      </div>
                    )}
                  </Card>
                );
              })}
            </div>
          ))}
        </div>
      </section>

      {/* CLI Tools Section */}
      {filteredCli.length > 0 && (
        <section>
          <button
            onClick={() => setCliSectionOpen((v) => !v)}
            type="button"
            className="flex items-center gap-2 mb-4 w-full text-left group cursor-pointer"
            aria-expanded={cliSectionOpen}
            aria-controls="cli-tools-section"
          >
            <Terminal className="h-4 w-4 text-pc-text-muted" />
            <span className="text-xs font-semibold uppercase tracking-wider flex-1 text-pc-text-secondary" role="heading" aria-level={2}>
              {t('tools.cli_tools')}
            </span>
            <Badge tone="neutral">{filteredCli.length}</Badge>
            <ChevronDown
              className="h-4 w-4 text-pc-text-muted transition-transform"
              style={{ transform: cliSectionOpen ? 'rotate(0deg)' : 'rotate(-90deg)' }}
            />
          </button>

          <div id="cli-tools-section">
            {cliSectionOpen && (
              <Card padded={false} className="overflow-hidden">
                <div className="overflow-x-auto">
                  <table className="w-full text-sm border-collapse">
                    <thead>
                      <tr className="border-b border-pc-border text-left text-[11px] font-medium uppercase tracking-wider text-pc-text-faint">
                        <th className="px-4 py-2.5 font-medium">{t('tools.name')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.path')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.version')}</th>
                        <th className="px-4 py-2.5 font-medium">{t('tools.category')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {filteredCli.map((tool) => (
                        <tr key={tool.name} className="border-b border-pc-border/60 last:border-0">
                          <td className="px-4 py-2.5 font-medium text-pc-text">
                            {tool.name}
                          </td>
                          <td className="px-4 py-2.5 font-mono text-xs truncate max-w-[200px] text-pc-text-muted">
                            {tool.path}
                          </td>
                          <td className="px-4 py-2.5 text-pc-text-muted">
                            {tool.version ?? '-'}
                          </td>
                          <td className="px-4 py-2.5">
                            <Badge tone="neutral" className="capitalize">{tool.category}</Badge>
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </Card>
            )}
          </div>
        </section>
      )}
    </div>
  );
}

// Per-tool risk-profile access matrix shown inside an expanded tool card.
// One row per profile: effective Allowed/Blocked state (+ the reason), a
// toggle that flips it, and a jump-link to the profile's full config.
function ToolAccessMatrix({
  tool,
  access,
  accessError,
  onToggle,
}: {
  tool: string;
  access: Record<string, ProfileAccess> | null;
  accessError: string | null;
  onToggle: (profile: string, tool: string, makeAllowed: boolean) => void;
}) {
  if (access === null && accessError === null) {
    return (
      <p className="text-xs text-pc-text-faint">{t('tools.loading_profiles')}</p>
    );
  }
  if (accessError && !access) {
    return (
      <p className="text-xs text-status-error">
        {t('tools.load_profiles_error')}: {accessError}
      </p>
    );
  }
  const profiles = Object.keys(access ?? {}).sort();
  return (
    <div className="space-y-2">
      <p className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
        {t('tools.access_by_profile')}
      </p>
      {accessError && (
        <p className="text-xs text-status-error">{accessError}</p>
      )}
      <ul className="space-y-1">
        {profiles.map((profile) => {
          const a = access![profile]!;
          const allowed = isToolAllowed(tool, a);
          return (
            <li
              key={profile}
              className="flex items-center justify-between gap-2 rounded-[var(--radius-sm)] px-2 py-1.5 hover:bg-pc-elevated/40"
            >
              <div className="min-w-0 flex items-center gap-2">
                <Link
                  to={`/config/risk_profiles/${encodeURIComponent(profile)}`}
                  className="text-sm font-mono text-pc-text-secondary hover:text-pc-accent truncate inline-flex items-center gap-1"
                  title={`${t('tools.open_profile_prefix')}${profile}${t('tools.open_profile_suffix')}`}
                >
                  {profile}
                  <ExternalLink className="h-3 w-3 flex-shrink-0 opacity-60" />
                </Link>
                <span className="text-[11px] text-pc-text-faint truncate">
                  {accessReason(tool, a)}
                </span>
              </div>
              <button
                type="button"
                onClick={() => onToggle(profile, tool, !allowed)}
                aria-pressed={allowed}
                title={
                  allowed
                    ? `${t('tools.block_prefix')}${tool}${t('tools.in_profile_mid')}${profile}`
                    : `${t('tools.allow_prefix')}${tool}${t('tools.in_profile_mid')}${profile}`
                }
                className={[
                  'flex-shrink-0 inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition-colors',
                  allowed
                    ? 'bg-status-success/10 text-status-success hover:bg-status-success/20'
                    : 'bg-pc-elevated text-pc-text-muted hover:bg-pc-elevated/70',
                ].join(' ')}
              >
                {allowed ? (
                  <ShieldCheck className="h-3.5 w-3.5" />
                ) : (
                  <ShieldX className="h-3.5 w-3.5" />
                )}
                {allowed ? t('tools.allowed') : t('tools.blocked')}
              </button>
            </li>
          );
        })}
      </ul>
      <p className="text-[11px] text-pc-text-faint">
        {t('tools.changes_edit_prefix')} <code className="font-mono">allowed_tools</code> /{' '}
        <code className="font-mono">excluded_tools</code> {t('tools.changes_edit_suffix')}
      </p>
    </div>
  );
}
