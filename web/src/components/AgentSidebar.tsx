import { useEffect, useState } from 'react';
import { PanelLeft, Search, Settings, X, Columns2, Network } from 'lucide-react';
import { loadAgentPickerSummaries } from '@/lib/agents';
import { useWorkspaceSettings } from '@/components/WorkspaceSettings';
import type { ChatTab } from '@/pages/chatWorkspace.state';
import type { TabIndicator, WorkspaceLayout } from '@/components/ChatTabBar';
import { t } from '@/lib/i18n';

export default function AgentSidebar({
  tabs,
  activeKey,
  indicators,
  layout,
  splitDisabled,
  onSelect,
  onClose,
  onOpen,
  onToggleLayout,
  colonyMode,
  onToggleColony,
}: {
  tabs: (ChatTab & { label: string })[];
  activeKey: string;
  indicators: Record<string, TabIndicator>;
  layout: WorkspaceLayout;
  splitDisabled: boolean;
  onSelect: (key: string) => void;
  onClose: (key: string) => void;
  onOpen: (alias: string) => void;
  onToggleLayout: () => void;
  colonyMode: boolean;
  onToggleColony: () => void;
}) {
  const [agents, setAgents] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [open, setOpen] = useState(false);
  const [error, setError] = useState('');
  const settings = useWorkspaceSettings();
  useEffect(() => {
    let cancelled = false;
    void loadAgentPickerSummaries()
      .then((items) => {
        if (!cancelled) setAgents(items.map((a) => a.alias));
      })
      .catch((e: Error) => {
        if (!cancelled) setError(e.message);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const rows = [
    ...tabs,
    ...agents
      .filter((alias) => !tabs.some((tab) => tab.alias === alias))
      .map((alias) => ({ alias, label: alias, key: alias, sessionId: '' })),
  ].filter((row) => row.label.toLowerCase().includes(query.toLowerCase()));
  return (
    <>
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-label={t('workspace.toggle_agents')}
        aria-expanded={open}
        className="absolute top-2 left-3 z-20 rounded-lg border border-pc-border bg-pc-surface p-2 md:hidden"
      >
        <PanelLeft className="h-4 w-4" />
      </button>
      {open && (
        <button
          type="button"
          className="absolute inset-0 z-20 bg-black/40 md:hidden"
          aria-label={t('common.close')}
          onClick={() => setOpen(false)}
        />
      )}
      <aside
        aria-label={t('nav.agents')}
        className={`${open ? 'absolute inset-y-0 left-0 z-30 flex' : 'hidden'} w-56 shrink-0 flex-col border-r border-pc-border bg-pc-surface md:static md:flex`}
      >
        <div className="flex items-center justify-between px-4 py-4">
          <h2 className="text-xs font-medium text-pc-text-muted">
            {t('nav.agents')}
          </h2>
          <button
            type="button"
            aria-label={t('config.all_settings')}
            onClick={() => settings('/config/agents')}
            className="rounded p-1 text-pc-text-muted hover:text-pc-text"
          >
            <Settings className="h-3.5 w-3.5" />
          </button>
        </div>
        <div
          className="mx-3 mb-3 flex rounded-lg border border-pc-border bg-pc-elevated p-1"
          role="group"
          aria-label={t('workspace.agent')}
        >
          <button
            type="button"
            aria-pressed={!colonyMode}
            onClick={() => {
              if (colonyMode) onToggleColony();
              setOpen(false);
            }}
            className={`min-w-0 flex-1 rounded-md px-2 py-2 text-xs ${!colonyMode ? 'bg-pc-surface text-pc-text shadow-sm' : 'text-pc-text-muted'}`}
          >
            {t('colony.chat')}
          </button>
          <button
            type="button"
            aria-pressed={colonyMode}
            onClick={() => {
              if (!colonyMode) onToggleColony();
              setOpen(false);
            }}
            className={`flex min-w-0 flex-1 items-center justify-center gap-1 rounded-md px-2 py-2 text-xs ${colonyMode ? 'bg-pc-surface text-pc-accent shadow-sm' : 'text-pc-text-muted'}`}
          >
            <Network className="h-3 w-3" />
            {t('colony.title')}
          </button>
        </div>
        <label className="mx-3 mb-3 flex items-center gap-2 rounded-lg bg-pc-elevated px-3 py-2">
          <Search className="h-3.5 w-3.5 text-pc-text-muted" />
          <input
            aria-label={t('workspace.find_agent')}
            placeholder={t('workspace.find_agent')}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="min-w-0 w-full bg-transparent text-xs outline-none"
          />
        </label>
        {error && (
          <p role="alert" className="px-3 text-xs text-status-error">
            {error}
          </p>
        )}
        <div
          role="tablist"
          aria-orientation="vertical"
          aria-label={t('nav.agents')}
          className="min-h-0 flex-1 overflow-y-auto px-2 pb-3"
        >
          {rows.map((row) => (
            <div
              key={row.key}
              className={`group flex items-center rounded-lg ${row.key === activeKey ? 'bg-pc-elevated' : 'hover:bg-pc-elevated/60'}`}
            >
              <button
                type="button"
                role="tab"
                id={`chat-tab-${row.key}`}
                aria-selected={row.key === activeKey}
                aria-controls={
                  row.sessionId ? `chat-panel-${row.key}` : undefined
                }
                onClick={() => {
                  if (row.sessionId) onSelect(row.key);
                  else onOpen(row.alias);
                  setOpen(false);
                }}
                className="flex min-w-0 flex-1 items-center gap-2 px-3 py-3 text-left text-sm"
              >
                <span
                  className={`h-1.5 w-1.5 shrink-0 rounded-full ${indicators[row.key]?.streaming ? 'animate-pulse bg-pc-accent' : indicators[row.key]?.unread ? 'bg-pc-accent' : 'bg-pc-text-faint/30'}`}
                />
                <span className="truncate">{row.label}</span>
              </button>
              {row.sessionId &&
                tabs.length > 1 &&
                !indicators[row.key]?.streaming && (
                  <button
                    type="button"
                    aria-label={`${t('common.close')} ${row.label}`}
                    onClick={() => onClose(row.key)}
                    className="mr-2 rounded p-1 text-pc-text-muted opacity-0 group-hover:opacity-100 focus:opacity-100"
                  >
                    <X className="h-3 w-3" />
                  </button>
                )}
            </div>
          ))}
          {!rows.length && (
            <p className="p-3 text-xs text-pc-text-muted">
              {t('nav.cmdk.empty')}
            </p>
          )}
        </div>
        {!splitDisabled && !colonyMode && (
          <button
            type="button"
            aria-pressed={layout === 'split'}
            onClick={onToggleLayout}
            className="flex items-center gap-2 border-t border-pc-border p-4 text-xs text-pc-text-muted"
          >
            <Columns2 className="h-3.5 w-3.5" />
            {t('workspace.split')}
          </button>
        )}
      </aside>
    </>
  );
}
