import { useEffect, useState, useCallback } from 'react';
import { Link } from 'react-router-dom';
import { Bot, Plus, AlertCircle } from 'lucide-react';
import AgentCard from '@/components/AgentCard';
import AgentDrawer from '@/components/AgentDrawer';
import { Button, PageHeader } from '@/components/ui';
import { t } from '@/lib/i18n';
import { loadAgentSummaries, toggleAgentEnabled, type AgentSummary } from '@/lib/agents';

interface AgentSummariesState {
  loading: boolean;
  error: string | null;
  agents: AgentSummary[];
}

export default function AgentsList() {
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
          error: err instanceof Error ? err.message : 'Failed to load agents',
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
        error: err instanceof Error ? err.message : `Failed to toggle ${agent.alias}`,
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
        description="Configured agents on this ZeroClaw instance."
        actions={
          <Link to="/config/agents">
            <Button variant="primary" size="md">
              <Plus className="h-4 w-4" />
              New Agent
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
        No agents configured yet
      </p>
      <p className="text-sm mb-4 text-pc-text-muted">
        Run Quickstart to create your first agent.
      </p>
      <Link to="/quickstart" className="inline-block">
        <Button variant="primary" size="md">
          <Plus className="h-4 w-4" />
          Start Quickstart
        </Button>
      </Link>
    </div>
  );
}
