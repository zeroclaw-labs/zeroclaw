import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AgentProvider } from '@/contexts/AgentContext';
import { AgentChatInner, type AgentChatStatus } from '@/pages/AgentChat';
import { ChatTabBar, type TabIndicator, type WorkspaceLayout } from '@/components/ChatTabBar';
import { basePath } from '@/lib/basePath';
import {
  applySessionChange,
  loadPersisted,
  makeTab,
  reservationsByKey,
  STORAGE_KEY,
  tabForOpenRequest,
  type ChatTab,
  type PersistedState,
} from '@/pages/chatWorkspace.state';

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

  const [tabs, setTabs] = useState<ChatTab[]>(() => {
    const stored = persisted.current.tabs ?? [];
    // The route's agent is always open, but only add it when it is not already
    // there — a deep link should land on the existing pane, not fork a new one.
    return stored.some((tb) => tb.alias === initialAlias)
      ? stored
      : [...stored, makeTab(initialAlias)];
  });
  // Resolved up front rather than in an effect: an empty first commit would
  // activate tabs[0] and replaceState the URL to the wrong agent before the
  // route settled. Prefer the pane that was active when the workspace was
  // stored, so reloading with two panes of one agent returns to the right one.
  const [activeKey, setActiveKey] = useState<string>(() => {
    const preferred = persisted.current.activeKey;
    const match =
      tabs.find((tb) => tb.key === preferred && tb.alias === initialAlias) ??
      tabs.find((tb) => tb.alias === initialAlias);
    return match?.key ?? tabs[0]?.key ?? '';
  });
  const [layout, setLayout] = useState<WorkspaceLayout>(persisted.current.layout ?? 'tabs');
  const [splitKeys, setSplitKeys] = useState<[string, string | null]>(
    persisted.current.splitKeys ?? ['', null],
  );

  const tabsRef = useRef(tabs);
  useEffect(() => { tabsRef.current = tabs; }, [tabs]);

  const activeKeyRef = useRef(activeKey);
  useEffect(() => { activeKeyRef.current = activeKey; }, [activeKey]);

  // Resolve the active tab, tolerating a stored key that no longer exists.
  const activeTab = useMemo(
    () => tabs.find((tb) => tb.key === activeKey) ?? tabs[0],
    [tabs, activeKey],
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
    const has = (k: string | null): boolean => !!k && tabs.some((tb) => tb.key === k);
    const left = has(splitKeys[0]) ? splitKeys[0] : (activeTab?.key ?? '');
    let right = splitKeys[1];
    if (!has(right) || right === left) {
      right = tabs.find((tb) => tb.key !== left)?.key ?? null;
    }
    return [left, right];
  }, [splitKeys, tabs, activeTab]);

  // Keys of the panes currently visible (so background panes can be `hidden`).
  const visibleKeys = useMemo<Set<string>>(() => {
    if (effectiveLayout === 'split') {
      return new Set([resolvedSplit[0], resolvedSplit[1]].filter(Boolean) as string[]);
    }
    return new Set(activeTab ? [activeTab.key] : []);
  }, [effectiveLayout, resolvedSplit, activeTab]);

  // Recompute the rendered indicator map from the status ref. An alias that is
  // currently visible is never shown as unread.
  const syncIndicators = useCallback(() => {
    const next: Record<string, TabIndicator> = {};
    for (const [key, s] of Object.entries(statusRef.current)) {
      next[key] = {
        streaming: s.streaming,
        unread: s.unread && !visibleKeys.has(key),
      };
    }
    setIndicators(next);
  }, [visibleKeys]);

  // Stable ref to the latest syncIndicators so the per-alias onStatus closures
  // (cached for identity stability) always run against current visibility.
  const syncIndicatorsRef = useRef(syncIndicators);
  useEffect(() => { syncIndicatorsRef.current = syncIndicators; }, [syncIndicators]);

  // Status callback handed to each pane. Marks a hidden tab unread when its
  // message count grows; tracks streaming from `typing`. Cached per alias so
  // each pane receives a STABLE function identity — otherwise AgentChatInner's
  // onStatus effect would re-run on every workspace render.
  const onStatusCacheRef = useRef<Record<string, (s: AgentChatStatus) => void>>({});
  const onStatusFor = useCallback((key: string) => {
    const cached = onStatusCacheRef.current[key];
    if (cached) return cached;
    const fn = (s: AgentChatStatus) => {
      const prev = statusRef.current[key] ?? {
        lastSeenCount: s.messageCount, liveCount: s.messageCount, streaming: false, unread: false,
      };
      const visible = visibleKeysRef.current.has(key);
      const grew = s.messageCount > prev.lastSeenCount;
      statusRef.current[key] = {
        lastSeenCount: visible ? s.messageCount : prev.lastSeenCount,
        liveCount: s.messageCount,
        streaming: s.typing,
        unread: visible ? false : prev.unread || grew,
      };
      syncIndicatorsRef.current();
    };
    onStatusCacheRef.current[key] = fn;
    return fn;
  }, []);

  // Keep a ref mirror of visibleKeys so the stable onStatus closure reads
  // the latest visibility without being re-created on every visibility change.
  const visibleKeysRef = useRef(visibleKeys);
  useEffect(() => {
    visibleKeysRef.current = visibleKeys;
    // When visibility changes, clear unread for newly-visible panes and
    // snapshot their seen-count to the latest reported live count.
    for (const key of visibleKeys) {
      const s = statusRef.current[key];
      if (s) { s.unread = false; s.lastSeenCount = s.liveCount; }
    }
    syncIndicators();
  }, [visibleKeys, syncIndicators]);

  // Open + activate the route alias on mount and on every change, without
  // remounting the workspace (the workspace is keyed by nothing volatile).
  // Activates the agent's FIRST open pane rather than forking another: a deep
  // link means "show me this agent", not "give me one more of it".
  useEffect(() => {
    // Already showing this agent — including on mount, where the initial state
    // above resolved it. Bailing keeps this idempotent under StrictMode's
    // double-invoked effects, which would otherwise re-activate the agent's
    // first pane and undo the restored selection.
    const current = tabsRef.current.find((tb) => tb.key === activeKeyRef.current);
    if (current?.alias === initialAlias) return;

    const existing = tabsRef.current.find((tb) => tb.alias === initialAlias);
    if (existing) {
      setActiveKey(existing.key);
      return;
    }
    const tab = makeTab(initialAlias);
    setTabs((prev) => (prev.some((tb) => tb.alias === initialAlias) ? prev : [...prev, tab]));
    setActiveKey(tab.key);
  }, [initialAlias]);

  // Persist workspace shape on any structural change.
  useEffect(() => {
    if (!activeTab) return;
    const snapshot: PersistedState = {
      version: 2,
      tabs,
      activeKey: activeTab.key,
      layout,
      splitKeys: resolvedSplit,
    };
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(snapshot)); } catch { /* noop */ }
  }, [tabs, activeTab, layout, resolvedSplit]);

  /** Record the conversation a pane moved to, so a reload restores it there. */
  const handleSessionChange = useCallback((key: string, sessionId: string) => {
    setTabs((prev) => applySessionChange(prev, key, sessionId));
  }, []);

  /**
   * Conversations held by the *other* panes.
   *
   * Two sockets on one gateway session get two server-side Agents, each seeded
   * from the snapshot taken when it connected, so their histories diverge and
   * the persisted transcript interleaves two branches. Stop and "Clear all"
   * also address a session by id, so one pane would cancel or wipe the other's
   * turn. Panes therefore claim a conversation exclusively, and the picker
   * refuses to hand one to a second pane or delete it out from under the pane
   * that owns it. The identity array is cached so a pane's props stay stable
   * between renders that did not change its siblings.
   */
  const reservedByKey = useMemo(() => reservationsByKey(tabs), [tabs]);

  // Cached per tab so each pane gets a stable callback identity, matching the
  // reason onStatusFor is cached.
  const onSessionChangeCacheRef = useRef<Record<string, (sessionId: string) => void>>({});
  const onSessionChangeFor = useCallback((key: string) => {
    const cached = onSessionChangeCacheRef.current[key];
    if (cached) return cached;
    const fn = (sessionId: string) => handleSessionChange(key, sessionId);
    onSessionChangeCacheRef.current[key] = fn;
    return fn;
  }, [handleSessionChange]);

  // Mirror the active alias to the URL via history.replaceState only — never a
  // React Router navigate, which would remount AgentChat and kill connections.
  useEffect(() => {
    // Include the reverse-proxy prefix so `target` matches the real
    // `window.location.pathname` under a gateway base path (e.g. "/zeroclaw").
    // Without it the comparison would never match, firing replaceState every
    // render and rewriting the bar to a prefix-less path that breaks
    // reload/deep-link (Router's basename no longer matches). basePath is
    // already normalized to "" (root) or a no-trailing-slash prefix, so plain
    // concatenation can't produce a double slash.
    const target = `${basePath}/agent/${activeTab?.alias ?? initialAlias}`;
    if (window.location.pathname !== target) {
      try { window.history.replaceState(window.history.state, '', target); } catch { /* noop */ }
    }
  }, [activeTab, initialAlias]);

  // ── Tab bar handlers ──────────────────────────────────────────────────
  const selectTab = useCallback((key: string) => {
    setActiveKey(key);
  }, []);

  /**
   * Open another pane for `alias`, even when one is already open — that is the
   * point of the picker now. The extra pane starts on a brand-new conversation
   * rather than the alias's last one, so two panes of one agent never open onto
   * the same transcript and fight over it.
   */
  // Mint the tab outside the updater: React double-invokes updaters in
  // StrictMode, which would generate two keys and activate the discarded one.
  const openChat = useCallback((alias: string) => {
    const tab = tabForOpenRequest(tabsRef.current, alias);
    setTabs((prev) => [...prev, tab]);
    setActiveKey(tab.key);
  }, []);

  const closeChat = useCallback((key: string) => {
    const prev = tabsRef.current;
    if (prev.length <= 1) return; // never close the last chat
    const next = prev.filter((tb) => tb.key !== key);
    setTabs(next);

    // If we closed the active pane, move activation to a neighbour.
    setActiveKey((cur) => {
      if (cur !== key) return cur;
      const idx = prev.findIndex((tb) => tb.key === key);
      return next[Math.min(idx, next.length - 1)]?.key ?? next[0]?.key ?? cur;
    });

    delete statusRef.current[key];
    delete onStatusCacheRef.current[key];
    delete onSessionChangeCacheRef.current[key];
    syncIndicators();
  }, [syncIndicators]);

  const toggleLayout = useCallback(() => {
    setLayout((l) => (l === 'split' ? 'tabs' : 'split'));
    // Seed split with the active pane + the next open one when entering split.
    setSplitKeys((prev) => {
      const left = activeTab?.key ?? '';
      const right = tabs.find((tb) => tb.key !== left)?.key ?? null;
      const prevRightOpen = prev[1] && tabs.some((tb) => tb.key === prev[1]);
      return prev[0] === left && prevRightOpen ? prev : [left, right];
    });
  }, [activeTab, tabs]);

  // Split is only offered when there are >= 2 panes.
  const splitDisabled = tabs.length < 2;

  // Number the panes of an agent that is open more than once, so two tabs
  // reading "coder" can be told apart. A single pane keeps the bare alias.
  const labelledTabs = useMemo(() => {
    const totals = new Map<string, number>();
    for (const tb of tabs) totals.set(tb.alias, (totals.get(tb.alias) ?? 0) + 1);
    const seen = new Map<string, number>();
    return tabs.map((tb) => {
      const ordinal = (seen.get(tb.alias) ?? 0) + 1;
      seen.set(tb.alias, ordinal);
      return {
        ...tb,
        label: (totals.get(tb.alias) ?? 0) > 1 ? `${tb.alias} ${ordinal}` : tb.alias,
      };
    });
  }, [tabs]);

  return (
    <div translate="no" className="notranslate flex flex-col h-full min-h-0">
      <ChatTabBar
        tabs={labelledTabs}
        activeKey={activeTab?.key ?? ''}
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
        {tabs.map((tab) => {
          const visible = visibleKeys.has(tab.key);
          // In split, each visible pane takes an equal share of the row.
          const paneClass = visible
            ? effectiveLayout === 'split'
              ? 'flex flex-col flex-1 min-w-0 min-h-0'
              : 'flex flex-col h-full'
            : 'hidden';
          return (
            <div
              key={tab.key}
              role="tabpanel"
              id={`chat-panel-${tab.key}`}
              aria-labelledby={`chat-tab-${tab.key}`}
              aria-hidden={!visible}
              className={paneClass}
            >
              {/* Keyed by the tab, not the alias, so one agent can be mounted
                  twice — and so a conversation switch inside a pane does not
                  remount the provider and drop its socket. */}
              <AgentProvider
                key={tab.key}
                agentAlias={tab.alias}
                initialSessionId={tab.sessionId}
                reservedSessionIds={reservedByKey[tab.key]}
                onSessionChange={onSessionChangeFor(tab.key)}
              >
                <AgentChatInner agentAlias={tab.alias} onStatus={onStatusFor(tab.key)} />
              </AgentProvider>
            </div>
          );
        })}
      </div>
    </div>
  );
}
