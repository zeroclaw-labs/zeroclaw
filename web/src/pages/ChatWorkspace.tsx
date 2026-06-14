import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AgentProvider } from '@/contexts/AgentContext';
import { AgentChatInner, type AgentChatStatus } from '@/pages/AgentChat';
import { ChatTabBar, type TabIndicator, type WorkspaceLayout } from '@/components/ChatTabBar';

const STORAGE_KEY = 'zeroclaw-chat-workspace';

interface PersistedState {
  openChats: string[];
  activeAlias: string;
  layout: WorkspaceLayout;
  splitAliases: [string, string | null];
}

interface PaneStatus {
  /** Last message count the workspace has "seen" while this alias was visible. */
  lastSeenCount: number;
  /** Most recent message count the pane reported (visible or not). */
  liveCount: number;
  /** Agent is currently mid-turn. */
  streaming: boolean;
  /** New messages arrived while this alias was hidden. */
  unread: boolean;
}

function loadPersisted(): Partial<PersistedState> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Partial<PersistedState>;
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

function dedupe(aliases: string[]): string[] {
  return Array.from(new Set(aliases.filter(Boolean)));
}

export interface ChatWorkspaceProps {
  /** Alias from the `/agent/:alias` route — opened + activated on mount and
   * whenever it changes (deep links / "Open chat"), without remounting the
   * workspace. */
  initialAlias: string;
}

/**
 * Multi-agent chat workspace.
 *
 * Renders several agent chats as tabs. EVERY open chat is mounted at all times
 * inside its own `<AgentProvider>` (one provider = one live WebSocket). Tab and
 * layout switches change only CSS visibility (`hidden`), never the mounted set,
 * so background chats stay connected and keep streaming. A pane only unmounts —
 * and its socket only closes — when its tab is explicitly closed.
 */
export default function ChatWorkspace({ initialAlias }: ChatWorkspaceProps) {
  const persisted = useRef<Partial<PersistedState>>(loadPersisted());

  const [openChats, setOpenChats] = useState<string[]>(() => {
    const fromStorage = persisted.current.openChats ?? [];
    return dedupe([...fromStorage, initialAlias]);
  });
  const [activeAlias, setActiveAlias] = useState<string>(initialAlias);
  const [layout, setLayout] = useState<WorkspaceLayout>(persisted.current.layout ?? 'tabs');
  const [splitAliases, setSplitAliases] = useState<[string, string | null]>(
    persisted.current.splitAliases ?? [initialAlias, null],
  );

  // Per-alias streaming / unread bookkeeping. Kept in a ref (source of truth,
  // mutated synchronously from onStatus) plus mirrored to state for rendering.
  const statusRef = useRef<Record<string, PaneStatus>>({});
  const [indicators, setIndicators] = useState<Record<string, TabIndicator>>({});

  // Effective layout. Split works on mobile too — the panes stack vertically
  // there (top/bottom) instead of side-by-side; see the split container below.
  const effectiveLayout: WorkspaceLayout = layout;

  // The two aliases shown in split. Default the second to the next open chat
  // after the active one (or the active itself if it's the only chat).
  const resolvedSplit = useMemo<[string, string | null]>(() => {
    const left = openChats.includes(splitAliases[0]) ? splitAliases[0] : activeAlias;
    let right = splitAliases[1];
    if (!right || !openChats.includes(right) || right === left) {
      right = openChats.find((a) => a !== left) ?? null;
    }
    return [left, right];
  }, [splitAliases, openChats, activeAlias]);

  // Set of aliases currently visible (so background panes can be `hidden`).
  const visibleAliases = useMemo<Set<string>>(() => {
    if (effectiveLayout === 'split') {
      return new Set([resolvedSplit[0], resolvedSplit[1]].filter(Boolean) as string[]);
    }
    return new Set([activeAlias]);
  }, [effectiveLayout, resolvedSplit, activeAlias]);

  // Recompute the rendered indicator map from the status ref. An alias that is
  // currently visible is never shown as unread.
  const syncIndicators = useCallback(() => {
    const next: Record<string, TabIndicator> = {};
    for (const [alias, s] of Object.entries(statusRef.current)) {
      next[alias] = {
        streaming: s.streaming,
        unread: s.unread && !visibleAliases.has(alias),
      };
    }
    setIndicators(next);
  }, [visibleAliases]);

  // Stable ref to the latest syncIndicators so the per-alias onStatus closures
  // (cached for identity stability) always run against current visibility.
  const syncIndicatorsRef = useRef(syncIndicators);
  useEffect(() => { syncIndicatorsRef.current = syncIndicators; }, [syncIndicators]);

  // Status callback handed to each pane. Marks a hidden tab unread when its
  // message count grows; tracks streaming from `typing`. Cached per alias so
  // each pane receives a STABLE function identity — otherwise AgentChatInner's
  // onStatus effect would re-run on every workspace render.
  const onStatusCacheRef = useRef<Record<string, (s: AgentChatStatus) => void>>({});
  const onStatusFor = useCallback((alias: string) => {
    const cached = onStatusCacheRef.current[alias];
    if (cached) return cached;
    const fn = (s: AgentChatStatus) => {
      const prev = statusRef.current[alias] ?? {
        lastSeenCount: s.messageCount, liveCount: s.messageCount, streaming: false, unread: false,
      };
      const visible = visibleAliasesRef.current.has(alias);
      const grew = s.messageCount > prev.lastSeenCount;
      statusRef.current[alias] = {
        lastSeenCount: visible ? s.messageCount : prev.lastSeenCount,
        liveCount: s.messageCount,
        streaming: s.typing,
        unread: visible ? false : prev.unread || grew,
      };
      syncIndicatorsRef.current();
    };
    onStatusCacheRef.current[alias] = fn;
    return fn;
  }, []);

  // Keep a ref mirror of visibleAliases so the stable onStatus closure reads
  // the latest visibility without being re-created on every visibility change.
  const visibleAliasesRef = useRef(visibleAliases);
  useEffect(() => {
    visibleAliasesRef.current = visibleAliases;
    // When visibility changes, clear unread for newly-visible aliases and
    // snapshot their seen-count to the latest reported live count.
    for (const alias of visibleAliases) {
      const s = statusRef.current[alias];
      if (s) { s.unread = false; s.lastSeenCount = s.liveCount; }
    }
    syncIndicators();
  }, [visibleAliases, syncIndicators]);

  // Open + activate the route alias on mount and on every change, without
  // remounting the workspace (the workspace is keyed by nothing volatile).
  useEffect(() => {
    setOpenChats((prev) => (prev.includes(initialAlias) ? prev : [...prev, initialAlias]));
    setActiveAlias(initialAlias);
  }, [initialAlias]);

  // Persist workspace shape on any structural change.
  useEffect(() => {
    const snapshot: PersistedState = { openChats, activeAlias, layout, splitAliases: resolvedSplit };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot)); } catch { /* noop */ }
  }, [openChats, activeAlias, layout, resolvedSplit]);

  // Mirror the active alias to the URL via history.replaceState only — never a
  // React Router navigate, which would remount AgentChat and kill connections.
  useEffect(() => {
    const target = `/agent/${activeAlias}`;
    if (window.location.pathname !== target) {
      try { window.history.replaceState(window.history.state, '', target); } catch { /* noop */ }
    }
  }, [activeAlias]);

  // ── Tab bar handlers ──────────────────────────────────────────────────
  const selectTab = useCallback((alias: string) => {
    setActiveAlias(alias);
  }, []);

  const openChat = useCallback((alias: string) => {
    setOpenChats((prev) => (prev.includes(alias) ? prev : [...prev, alias]));
    setActiveAlias(alias);
  }, []);

  const closeChat = useCallback((alias: string) => {
    setOpenChats((prev) => {
      if (prev.length <= 1) return prev; // never close the last chat
      const next = prev.filter((a) => a !== alias);
      // If we closed the active tab, move activation to a neighbour.
      setActiveAlias((cur) => {
        if (cur !== alias) return cur;
        const idx = prev.indexOf(alias);
        return next[Math.min(idx, next.length - 1)] ?? next[0] ?? cur;
      });
      return next;
    });
    delete statusRef.current[alias];
    delete onStatusCacheRef.current[alias];
    syncIndicators();
  }, [syncIndicators]);

  const toggleLayout = useCallback(() => {
    setLayout((l) => (l === 'split' ? 'tabs' : 'split'));
    // Seed split with the active alias + next open chat when entering split.
    setSplitAliases((prev) => {
      const left = activeAlias;
      const right = openChats.find((a) => a !== left) ?? null;
      return prev[0] === left && prev[1] && openChats.includes(prev[1]) ? prev : [left, right];
    });
  }, [activeAlias, openChats]);

  // Split is only offered when there are >= 2 chats and the viewport is wide.
  const splitDisabled = openChats.length < 2;

  return (
    <div translate="no" className="notranslate flex flex-col h-full min-h-0">
      <ChatTabBar
        openChats={openChats}
        activeAlias={activeAlias}
        indicators={indicators}
        layout={effectiveLayout}
        splitDisabled={splitDisabled}
        onSelect={selectTab}
        onClose={closeChat}
        onOpen={openChat}
        onToggleLayout={toggleLayout}
      />

      {/* Content area. Every open chat is mounted here at all times; only CSS
          visibility changes between tab/layout switches, so background sockets
          stay alive. In split layout the two visible panes share the width. */}
      <div className={effectiveLayout === 'split' ? 'flex flex-col md:flex-row flex-1 min-h-0 divide-y md:divide-y-0 md:divide-x divide-pc-border' : 'flex-1 min-h-0'}>
        {openChats.map((alias) => {
          const visible = visibleAliases.has(alias);
          // In split, each visible pane takes an equal share of the row.
          const paneClass = visible
            ? effectiveLayout === 'split'
              ? 'flex flex-col flex-1 min-w-0 min-h-0'
              : 'flex flex-col h-full'
            : 'hidden';
          return (
            <div
              key={alias}
              role="tabpanel"
              id={`chat-panel-${alias}`}
              aria-labelledby={`chat-tab-${alias}`}
              aria-hidden={!visible}
              className={paneClass}
            >
              <AgentProvider key={alias} agentAlias={alias}>
                <AgentChatInner agentAlias={alias} onStatus={onStatusFor(alias)} />
              </AgentProvider>
            </div>
          );
        })}
      </div>
    </div>
  );
}
