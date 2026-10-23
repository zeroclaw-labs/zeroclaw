import { useEffect, useState, useCallback } from 'react';
import { Link } from 'react-router-dom';
import { Bot, Plus, AlertCircle, MessageSquare, Power } from 'lucide-react';
import AgentCard from '@/components/AgentCard';
import AgentDrawer from '@/components/AgentDrawer';
import { Badge, Button, PageHeader } from '@/components/ui';
import { useAvailableAgents, useNavAccess } from '@/hooks/useNavAccess';
import { t } from '@/lib/i18n';
import { loadAgentSummaries, toggleAgentEnabled, type AgentSummary } from '@/lib/agents';

interface AgentSummariesState {
  loading: boolean;
  error: string | null;
  agents: AgentSummary[];
}

export default function AgentsList() {
  return useNavAccess().operatorPages ? <OperatorAgentsList /> : <AvailableAgentsList />;
}

/**
 * The agents a signed-in account without the operator dashboard may chat with
 * (`/api/agents/available`). Configuration, cost and memory belong to the
 * operator pages, so a row here only opens the chat.
 */
function AvailableAgentsList() {
  const { loading, agents, error } = useAvailableAgents(true);
  return (
    <div className="p-6 max-w-6xl mx-auto">
      <PageHeader
        className="mb-6"
        title={t('nav.agents')}
        description={t('agents_list.available_description')}
      />

      {error && (
        <div className="mb-4 px-4 py-3 rounded-[var(--radius-md)] border border-status-error/20 bg-status-error/10 text-status-error flex items-start gap-2 text-sm">
          <AlertCircle className="h-4 w-4 flex-shrink-0 mt-0.5" />
          <span>{error}</span>
        </div>
      )}

      {loading ? (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-8 text-center text-sm text-pc-text-muted">
          {t('common.loading')}
        </div>
      ) : agents.length === 0 ? (
        <div className="rounded-[var(--radius-lg)] border border-dashed border-pc-border bg-pc-surface p-12 text-center">
          <div className="h-12 w-12 rounded-[var(--radius-lg)] mx-auto mb-4 flex items-center justify-center bg-pc-accent/10">
            <Bot className="h-6 w-6 text-pc-accent" />
          </div>
          <p className="text-sm text-pc-text-muted">{t('agents_list.available_empty')}</p>
        </div>
      ) : (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface overflow-hidden">
          {agents.map((agent) => {
            const identity = (
              <>
                <div className="h-8 w-8 rounded-[var(--radius-md)] flex-shrink-0 flex items-center justify-center bg-pc-accent/10">
                  <Bot className="h-4 w-4 text-pc-accent" />
                </div>
                <span className="min-w-0 flex-1 text-sm font-semibold truncate text-pc-text">
                  {agent.alias}
                </span>
                <Badge tone={agent.enabled ? 'ok' : 'neutral'} className="flex-shrink-0">
                  <Power className="h-3 w-3" />
                  {agent.enabled ? t('agent.enabled') : t('agent.disabled')}
                </Badge>
              </>
            );
            const rowClass =
              'w-full flex items-center gap-3 px-4 py-3 border-b border-pc-border last:border-b-0';
            // A disabled agent can't take a chat; list it without a link.
            return agent.enabled ? (
              <Link
                key={agent.alias}
                to={`/agent/${encodeURIComponent(agent.alias)}`}
                data-available-agent={agent.alias}
                className={`${rowClass} transition-colors hover:bg-[var(--pc-hover)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]`}
              >
                {identity}
                <span className="inline-flex items-center gap-1.5 h-7 px-2.5 rounded-[var(--radius-md)] flex-shrink-0 text-xs font-medium bg-pc-accent/10 text-pc-accent">
                  <MessageSquare className="h-3.5 w-3.5" />
                  <span className="hidden sm:inline">{t('agent.open_chat')}</span>
                </span>
              </Link>
            ) : (
              <div key={agent.alias} data-available-agent={agent.alias} className={`${rowClass} opacity-60`}>
                {identity}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function OperatorAgentsList() {
  const [state, setState] = useState<AgentSummariesState>({
    loading: true,
    error: null,
    agents: [],
  });
  const [toggling, setToggling] = useState<Set<string>>(new Set());
  // Selecting a row sets the drawer's agent (by alias); closing clears it. We
  // key off the alias so the open drawer reflects live toggle updates.
  const [selectedAlias, setSelectedAlias] = useState<string | null>(null);

  const refresh = useCallback(() => {
    setState((s) => ({ ...s, loading: true, error: null }));
    loadAgentSummaries()
      .then((agents) => setState({ loading: false, error: null, agents }))
      .catch((err: unknown) =>
        setState({
          loading: false,
          error: err instanceof Error ? err.message : t('agents_list.load_failed'),
          agents: [],
        }),
      );
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const toggleEnabled = useCallback(async (agent: AgentSummary) => {
    setToggling((prev) => new Set(prev).add(agent.alias));
    try {
      await toggleAgentEnabled(agent.alias, !agent.enabled);
      setState((s) => ({
        ...s,
        agents: s.agents.map((a) =>
          a.alias === agent.alias ? { ...a, enabled: !a.enabled } : a,
        ),
      }));
    } catch (err) {
      setState((s) => ({
        ...s,
        error: err instanceof Error ? err.message : `${t('agents_list.toggle_failed_prefix')}${agent.alias}`,
      }));
    } finally {
      setToggling((prev) => {
        const next = new Set(prev);
        next.delete(agent.alias);
        return next;
      });
    }
  }, []);

  const selectedAgent =
    selectedAlias === null
      ? null
      : state.agents.find((a) => a.alias === selectedAlias) ?? null;

  return (
    <div className="p-6 max-w-6xl mx-auto">
      <PageHeader
        className="mb-6"
        title={t('nav.agents')}
        description={t('agents_list.description')}
        actions={
          <Link to="/config/agents">
            <Button variant="primary" size="md">
              <Plus className="h-4 w-4" />
              {t('agents_list.new_agent')}
            </Button>
          </Link>
        }
      />

      {state.error && (
        <div className="mb-4 px-4 py-3 rounded-[var(--radius-md)] border border-status-error/20 bg-status-error/10 text-status-error flex items-start gap-2 text-sm">
          <AlertCircle className="h-4 w-4 flex-shrink-0 mt-0.5" />
          <span>{state.error}</span>
        </div>
      )}

      {state.loading && state.agents.length === 0 ? (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-8 text-center text-sm text-pc-text-muted">
          {t('common.loading')}
        </div>
      ) : state.agents.length === 0 ? (
        <EmptyState />
      ) : (
        <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface overflow-hidden">
          {state.agents.map((agent) => (
            <AgentCard
              key={agent.alias}
              agent={agent}
              selected={agent.alias === selectedAlias}
              onSelect={() => setSelectedAlias(agent.alias)}
            />
          ))}
        </div>
      )}

      <AgentDrawer
        agent={selectedAgent}
        onClose={() => setSelectedAlias(null)}
        onToggle={toggleEnabled}
        toggling={selectedAgent ? toggling.has(selectedAgent.alias) : false}
      />
    </div>
  );
}

function EmptyState() {
  return (
    <div className="rounded-[var(--radius-lg)] border border-dashed border-pc-border bg-pc-surface p-12 text-center">
      <div className="h-12 w-12 rounded-[var(--radius-lg)] mx-auto mb-4 flex items-center justify-center bg-pc-accent/10">
        <Bot className="h-6 w-6 text-pc-accent" />
      </div>
      <p className="text-base font-medium mb-1 text-pc-text">
        {t('agents_list.empty_title')}
      </p>
      <p className="text-sm mb-4 text-pc-text-muted">
        {t('agents_list.empty_hint')}
      </p>
      <Link to="/quickstart" className="inline-block">
        <Button variant="primary" size="md">
          <Plus className="h-4 w-4" />
          {t('agents_list.start_quickstart')}
        </Button>
      </Link>
    </div>
  );
}
