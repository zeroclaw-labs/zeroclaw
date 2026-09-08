import { useEffect, useRef, useState } from 'react';
import { Plus, X, Columns2, Square, Bot, Loader2 } from 'lucide-react';
import { t } from '@/lib/i18n';
import { Button } from '@/components/ui';
import { getMapKeys } from '@/lib/api';

/** Per-alias indicator state surfaced in the tab bar. */
export interface TabIndicator {
  /** Agent is mid-turn (streaming) — accent pulse dot. */
  streaming: boolean;
  /** New messages arrived while the tab was not visible — accent unread dot. */
  unread: boolean;
}

export type WorkspaceLayout = 'tabs' | 'split';

/** One tab as the bar renders it: identity, agent, and display label. */
export interface ChatTabView {
  /** Stable pane identity. An agent open twice yields two keys, one alias. */
  key: string;
  alias: string;
  /** Alias, numbered when the same agent is open more than once. */
  label: string;
}

export interface ChatTabBarProps {
  /** Open panes, in tab order. */
  tabs: ChatTabView[];
  /** Currently active (visible, in `tabs` layout) pane key. */
  activeKey: string;
  /** Per-pane streaming / unread indicators, keyed by tab key. */
  indicators: Record<string, TabIndicator>;
  layout: WorkspaceLayout;
  /** When true the layout toggle is disabled and forced to `tabs` (mobile). */
  splitDisabled: boolean;
  onSelect: (key: string) => void;
  onClose: (key: string) => void;
  onOpen: (alias: string) => void;
  onToggleLayout: () => void;
}

/**
 * Tab bar for the multi-agent ChatWorkspace.
 *
 * One tab per open pane (agent name + live status dot + close affordance), a
 * `+` agent picker, and a layout toggle that flips between stacked tabs and a
 * side-by-side split. Fully keyboard navigable: `role="tablist"` with arrow-key
 * movement between tabs.
 *
 * Tabs are identified by pane key rather than alias, so the same agent can be
 * open several times over different conversations; the picker therefore offers
 * every configured agent, including ones already open.
 */
export function ChatTabBar({
  tabs,
  activeKey,
  indicators,
  layout,
  splitDisabled,
  onSelect,
  onClose,
  onOpen,
  onToggleLayout,
}: ChatTabBarProps) {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [allAgents, setAllAgents] = useState<string[]>([]);
  const [pickerLoading, setPickerLoading] = useState(false);
  const [pickerError, setPickerError] = useState(false);
  const pickerRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  // Close the agent picker on outside click.
  useEffect(() => {
    if (!pickerOpen) return;
    function handle(e: MouseEvent) {
      if (pickerRef.current && !pickerRef.current.contains(e.target as Node)) {
        setPickerOpen(false);
      }
    }
    document.addEventListener('mousedown', handle);
    return () => document.removeEventListener('mousedown', handle);
  }, [pickerOpen]);

  // Enumerate configured agents when the picker opens. Aliases come straight
  // from the config map key `agents` — the same source the Dashboard and
  // agents list use — so the picker never drifts from real configuration.
  function openPicker() {
    setPickerOpen((v) => !v);
    if (pickerOpen) return;
    setPickerLoading(true);
    setPickerError(false);
    getMapKeys('agents')
      .then((r) => setAllAgents(r.keys))
      .catch(() => setPickerError(true))
      .finally(() => setPickerLoading(false));
  }

  function focusTab(tab: ChatTabView | undefined) {
    if (!tab) return;
    onSelect(tab.key);
    tabRefs.current[tab.key]?.focus();
  }

  function handleTabKeyDown(e: React.KeyboardEvent, idx: number) {
    if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') {
      e.preventDefault();
      const dir = e.key === 'ArrowRight' ? 1 : -1;
      focusTab(tabs[(idx + dir + tabs.length) % tabs.length]);
    } else if (e.key === 'Home') {
      e.preventDefault();
      focusTab(tabs[0]);
    } else if (e.key === 'End') {
      e.preventDefault();
      focusTab(tabs[tabs.length - 1]);
    }
  }

  const closableLast = tabs.length <= 1;
  // Every configured agent is offered: choosing one already open adds a second
  // pane for it rather than being unavailable.
  const openCounts = new Map<string, number>();
  for (const tb of tabs) openCounts.set(tb.alias, (openCounts.get(tb.alias) ?? 0) + 1);

  return (
    <div className="relative z-20 flex items-stretch border-b border-pc-border bg-pc-surface">
      <div
        role="tablist"
        aria-label={t('workspace.tablist_label')}
        aria-orientation="horizontal"
        className="flex items-stretch gap-1 overflow-x-auto px-2 py-1.5 flex-1 min-w-0"
      >
        {tabs.map((tab, idx) => {
          const active = tab.key === activeKey;
          const ind = indicators[tab.key];
          return (
            <button
              key={tab.key}
              ref={(el) => { tabRefs.current[tab.key] = el; }}
              role="tab"
              id={`chat-tab-${tab.key}`}
              aria-selected={active}
              aria-controls={`chat-panel-${tab.key}`}
              tabIndex={active ? 0 : -1}
              onClick={() => onSelect(tab.key)}
              onKeyDown={(e) => handleTabKeyDown(e, idx)}
              className={[
                'group flex items-center gap-2 h-8 pl-2.5 pr-1.5 rounded-[var(--radius-md)]',
                'text-xs font-medium whitespace-nowrap transition-colors',
                'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]',
                active
                  ? 'bg-pc-accent/10 text-pc-accent border border-pc-accent/30'
                  : 'border border-transparent text-pc-text-secondary hover:bg-[var(--pc-hover)] hover:text-pc-text',
              ].join(' ')}
            >
              <StatusDot streaming={ind?.streaming} unread={ind?.unread} active={active} />
              <Bot className="h-3.5 w-3.5 shrink-0" aria-hidden />
              <span className="max-w-[160px] truncate">{tab.label}</span>
              <span
                role="button"
                tabIndex={-1}
                aria-label={t('workspace.close_chat')}
                title={t('workspace.close_chat')}
                aria-disabled={closableLast}
                onClick={(e) => {
                  e.stopPropagation();
                  if (!closableLast) onClose(tab.key);
                }}
                className={[
                  'inline-flex items-center justify-center h-5 w-5 rounded-[var(--radius-sm)] shrink-0',
                  'text-pc-text-muted transition-colors',
                  closableLast
                    ? 'opacity-30 cursor-not-allowed'
                    : 'hover:bg-status-error/15 hover:text-status-error cursor-pointer',
                ].join(' ')}
              >
                <X className="h-3 w-3" />
              </span>
            </button>
          );
        })}
      </div>

      {/* Agent picker — sibling of the scrolling tablist (NOT inside it):
          overflow-x-auto on the tablist clips the y-axis too, which would hide
          this dropdown. */}
      <div className="relative flex items-center px-1" ref={pickerRef}>
          <button
            type="button"
            onClick={openPicker}
            aria-haspopup="menu"
            aria-expanded={pickerOpen}
            aria-label={t('workspace.open_chat')}
            title={t('workspace.open_chat')}
            className="inline-flex items-center justify-center h-8 w-8 rounded-[var(--radius-md)] text-pc-text-secondary border border-transparent transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
          >
            <Plus className="h-4 w-4" />
          </button>

          {pickerOpen && (
            <div
              role="menu"
              aria-label={t('workspace.open_chat')}
              className="absolute right-0 top-full mt-1.5 z-50 min-w-[200px] max-h-72 overflow-y-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-elevated py-1 shadow-[var(--pc-shadow-md)]"
            >
              {pickerLoading && (
                <div className="flex items-center gap-2 px-3 py-2 text-xs text-pc-text-muted">
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {t('common.loading')}
                </div>
              )}
              {!pickerLoading && pickerError && (
                <div className="px-3 py-2 text-xs text-status-error">{t('workspace.picker_error')}</div>
              )}
              {!pickerLoading && !pickerError && allAgents.length === 0 && (
                <div className="px-3 py-2 text-xs text-pc-text-muted">{t('workspace.no_agents')}</div>
              )}
              {!pickerLoading && !pickerError && allAgents.map((alias) => {
                const openCount = openCounts.get(alias) ?? 0;
                return (
                  <button
                    key={alias}
                    type="button"
                    role="menuitem"
                    onClick={() => { onOpen(alias); setPickerOpen(false); }}
                    className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs text-pc-text transition-colors hover:bg-[var(--pc-hover)] focus-visible:outline-none focus-visible:bg-[var(--pc-hover)]"
                  >
                    <Bot className="h-3.5 w-3.5 text-pc-accent shrink-0" aria-hidden />
                    <span className="truncate">{alias}</span>
                    {/* Say what picking this will do, so choosing an agent that
                        is already open does not look like a no-op. */}
                    {openCount > 0 && (
                      <span className="ml-auto shrink-0 text-[11px] text-pc-text-muted">
                        {t('workspace.open_another')}
                      </span>
                    )}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      {/* Layout toggle */}
      <div className="flex items-center px-2 border-l border-pc-border">
        <Button
          variant="ghost"
          size="sm"
          onClick={onToggleLayout}
          disabled={splitDisabled}
          aria-pressed={layout === 'split'}
          aria-label={layout === 'split' ? t('workspace.layout_tabs') : t('workspace.layout_split')}
          title={layout === 'split' ? t('workspace.layout_tabs') : t('workspace.layout_split')}
        >
          {layout === 'split' ? <Square className="h-3.5 w-3.5" /> : <Columns2 className="h-3.5 w-3.5" />}
          <span className="hidden sm:inline">
            {layout === 'split' ? t('workspace.layout_tabs') : t('workspace.layout_split')}
          </span>
        </Button>
      </div>
    </div>
  );
}

/**
 * Tab status dot:
 * - streaming → pulsing accent dot (agent is mid-turn)
 * - unread (and not streaming) → solid accent dot
 * - otherwise → a faint idle dot so tab metrics stay aligned
 */
function StatusDot({ streaming, unread, active }: { streaming?: boolean; unread?: boolean; active: boolean }) {
  if (streaming) {
    return (
      <span className="relative inline-flex h-2 w-2 shrink-0" aria-hidden>
        <span className="absolute inline-flex h-full w-full rounded-full bg-pc-accent opacity-60 animate-ping" />
        <span className="relative inline-flex h-2 w-2 rounded-full bg-pc-accent" />
      </span>
    );
  }
  if (unread) {
    return <span className="inline-flex h-2 w-2 shrink-0 rounded-full bg-pc-accent" aria-hidden />;
  }
  return (
    <span
      className={['inline-flex h-2 w-2 shrink-0 rounded-full', active ? 'bg-pc-accent/40' : 'bg-pc-text-faint/40'].join(' ')}
      aria-hidden
    />
  );
}

export default ChatTabBar;
