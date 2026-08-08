import type { WorkspaceLayout } from '@/components/ChatTabBar';
import { getActiveSessionId, newSessionId } from '@/lib/chatSessions';
import { generateUUID } from '@/lib/uuid';

/**
 * Pane bookkeeping for the multi-tab chat workspace.
 *
 * Extracted from the component so the ownership rules — which pane holds which
 * conversation, and which conversations a pane may therefore not touch — are
 * reachable by tests without a browser. `ChatWorkspace` owns the React state;
 * this module owns the transitions applied to it, so there is exactly one
 * implementation of each rule.
 */

export const STORAGE_KEY = 'zeroclaw-chat-workspace';

/**
 * One open pane.
 *
 * `key`, not the alias, is the tab's identity — that is what lets an agent be
 * open more than once. It is minted when the tab opens and never changes, so
 * switching the conversation inside a pane does not alter the React key and
 * therefore does not remount the provider and drop its WebSocket.
 */
export interface ChatTab {
  key: string;
  alias: string;
  /** Conversation this pane is showing; updated when the pane switches. */
  sessionId: string;
}

export interface PersistedState {
  version: 2;
  tabs: ChatTab[];
  activeKey: string;
  layout: WorkspaceLayout;
  splitKeys: [string, string | null];
}

/** Pre-multi-tab shape, when a tab was just an alias. */
export interface PersistedStateV1 {
  openChats?: string[];
  activeAlias?: string;
  layout?: WorkspaceLayout;
  splitAliases?: [string, string | null];
}

/** Build a tab for `alias`, resuming its last conversation unless one is given. */
export function makeTab(alias: string, sessionId?: string): ChatTab {
  return { key: generateUUID(), alias, sessionId: sessionId ?? getActiveSessionId(alias) };
}

/**
 * Guarantee no two panes claim the same conversation.
 *
 * Live panes cannot reach that state — the picker refuses it — but a workspace
 * stored by an earlier build could, and restoring it would put two sockets on
 * one gateway session. The later pane is moved to a fresh conversation instead.
 */
export function withDistinctSessions(tabs: ChatTab[]): ChatTab[] {
  const claimed = new Set<string>();
  return tabs.map((tb) => {
    if (!claimed.has(tb.sessionId)) {
      claimed.add(tb.sessionId);
      return tb;
    }
    const sessionId = newSessionId();
    claimed.add(sessionId);
    return { ...tb, sessionId };
  });
}

/**
 * Read the stored workspace, upgrading the pre-multi-tab shape.
 *
 * A v1 entry listed bare aliases, so each becomes one tab resuming that alias's
 * last conversation — the layout an operator had before the upgrade.
 */
export function loadPersisted(): Partial<PersistedState> {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== 'object') return {};

    const candidate = parsed as Partial<PersistedState> & PersistedStateV1;
    if (Array.isArray(candidate.tabs)) {
      // Drop anything malformed rather than rendering a pane with no alias.
      const tabs = candidate.tabs.filter(
        (tb): tb is ChatTab =>
          !!tb && typeof tb.key === 'string' && typeof tb.alias === 'string' && typeof tb.sessionId === 'string',
      );
      if (!tabs.length) return { layout: candidate.layout };
      return {
        tabs: withDistinctSessions(tabs),
        activeKey: candidate.activeKey,
        layout: candidate.layout,
        splitKeys: candidate.splitKeys,
      };
    }

    const aliases = Array.from(new Set((candidate.openChats ?? []).filter(Boolean)));
    if (!aliases.length) return { layout: candidate.layout };
    const tabs = aliases.map((alias) => makeTab(alias));
    const active = tabs.find((tb) => tb.alias === candidate.activeAlias) ?? tabs[0];
    const splitLeft = tabs.find((tb) => tb.alias === candidate.splitAliases?.[0]) ?? active;
    const splitRight = tabs.find((tb) => tb.alias === candidate.splitAliases?.[1]);
    return {
      tabs,
      activeKey: active?.key,
      layout: candidate.layout,
      splitKeys: [splitLeft?.key ?? '', splitRight?.key ?? null],
    };
  } catch {
    return {};
  }
}

/**
 * Conversations held by the *other* panes, per pane key.
 *
 * Two sockets on one gateway session get two server-side Agents, each seeded
 * from the snapshot taken when it connected, so their histories diverge and
 * the persisted transcript interleaves two branches. Stop and "Clear all"
 * also address a session by id, so one pane would cancel or wipe the other's
 * turn. Panes therefore claim a conversation exclusively, and the picker
 * refuses to hand one to a second pane or delete it out from under the pane
 * that owns it.
 */
export function reservationsByKey(tabs: ChatTab[]): Record<string, string[]> {
  const byKey: Record<string, string[]> = {};
  for (const tb of tabs) {
    byKey[tb.key] = tabs.filter((other) => other.key !== tb.key).map((other) => other.sessionId);
  }
  return byKey;
}

/**
 * Mint the pane that "open chat" should append.
 *
 * A second pane for an already-open alias starts on a brand-new conversation
 * rather than that alias's last one, so two panes of one agent never open onto
 * the same transcript and fight over it.
 */
export function tabForOpenRequest(tabs: ChatTab[], alias: string): ChatTab {
  const alreadyOpen = tabs.some((tb) => tb.alias === alias);
  return makeTab(alias, alreadyOpen ? newSessionId() : undefined);
}

/** Record the conversation a pane moved to, so a reload restores it there. */
export function applySessionChange(
  tabs: ChatTab[],
  key: string,
  sessionId: string,
): ChatTab[] {
  return tabs.map((tb) => (tb.key === key && tb.sessionId !== sessionId ? { ...tb, sessionId } : tb));
}
