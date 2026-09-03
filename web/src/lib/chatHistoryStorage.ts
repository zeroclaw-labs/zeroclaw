import type { SessionMessageRow } from '../types/api.ts';
import { generateUUID } from './uuid.ts';

const MAX_MESSAGES = 100;
const PREFIX = 'zeroclaw_chat_history_v1:';

export interface PersistedChatBubble {
  id: string;
  role: 'user' | 'agent';
  content: string;
  thinking?: string;
  markdown?: boolean;
  /** Trusted lifecycle marker retained for a locally durable terminal notice. */
  notice?: boolean;
  /** Streamed assistant output retained only when the terminal frame reports
   *  that the canonical failed-turn delta was not fully persisted. */
  terminalPartial?: boolean;
  /** Verbatim locally-composed user input — never gateway-prefixed, so the
   *  bubble skips stripServerTimestamp for it. (Server rows omit this.) */
  local?: boolean;
  toolCall?: { name: string; args?: unknown; output?: string };
  timestamp: string;
}

function storageKey(sessionId: string): string {
  return `${PREFIX}${sessionId}`;
}

export function loadChatHistory(sessionId: string): PersistedChatBubble[] {
  try {
    const raw = localStorage.getItem(storageKey(sessionId));
    if (!raw) return [];
    const parsed = JSON.parse(raw) as { messages?: PersistedChatBubble[] };
    if (!parsed.messages?.length) return [];
    return parsed.messages;
  } catch {
    return [];
  }
}

export function saveChatHistory(sessionId: string, messages: PersistedChatBubble[]): void {
  try {
    const slice = messages.slice(-MAX_MESSAGES);
    localStorage.setItem(storageKey(sessionId), JSON.stringify({ messages: slice }));
  } catch {
    // QuotaExceeded or private mode
  }
}

/**
 * Drop a conversation's cached transcript.
 *
 * Deleting a conversation has to take the local copy with it: the cache is
 * readable in the browser long after the gateway row is gone, which is not what
 * "delete" promises, and one key per conversation would otherwise accumulate
 * for as long as the origin's storage lives.
 */
export function removeChatHistory(sessionId: string): void {
  try {
    localStorage.removeItem(storageKey(sessionId));
  } catch {
    // Private mode — nothing was cached to begin with.
  }
}

/** Map server-persisted rows into UI messages, preserving durable ordering when available. */
export function mapServerMessagesToPersisted(rows: SessionMessageRow[]): PersistedChatBubble[] {
  const base = Date.now() - rows.length * 1000;
  const out: PersistedChatBubble[] = [];
  let idx = 0;
  for (const row of rows) {
    if (row.role === 'system') continue;
    const parsedCreatedAt = row.created_at === null ? Number.NaN : Date.parse(row.created_at);
    const ts = Number.isFinite(parsedCreatedAt)
      ? new Date(parsedCreatedAt).toISOString()
      : new Date(base + idx * 1000).toISOString();
    idx += 1;
    if (row.role === 'user') {
      out.push({
        id: generateUUID(),
        role: 'user',
        content: row.content,
        timestamp: ts,
      });
    } else if (row.role === 'assistant') {
      out.push({
        id: generateUUID(),
        role: 'agent',
        content: row.content,
        markdown: true,
        timestamp: ts,
      });
    } else {
      out.push({
        id: generateUUID(),
        role: 'agent',
        content: row.content,
        markdown: false,
        timestamp: ts,
      });
    }
  }
  return out;
}

export function persistedToUiMessages(
  rows: PersistedChatBubble[],
): Array<{
  id: string;
  role: 'user' | 'agent';
  content: string;
  thinking?: string;
  markdown?: boolean;
  notice?: boolean;
  terminalPartial?: boolean;
  local?: boolean;
  toolCall?: { name: string; args?: unknown; output?: string };
  timestamp: Date;
}> {
  return rows.map((m) => ({
    id: m.id,
    role: m.role,
    content: m.content,
    thinking: m.thinking,
    markdown: m.markdown,
    notice: m.notice,
    terminalPartial: m.terminalPartial,
    local: m.local,
    toolCall: m.toolCall,
    timestamp: new Date(m.timestamp),
  }));
}

export function uiMessagesToPersisted(
  messages: Array<{
    id: string;
    role: 'user' | 'agent';
    content: string;
    thinking?: string;
    markdown?: boolean;
    notice?: boolean;
    terminalPartial?: boolean;
    local?: boolean;
    ephemeral?: boolean;
    toolCall?: { name: string; args?: unknown; output?: string };
    timestamp: Date;
  }>,
): PersistedChatBubble[] {
  return messages
    // Skip messages flagged `ephemeral: true` (web slash-command output like
    // /help, /model banners, unknown-command notices). They are throwaway UI
    // feedback and must not be re-hydrated as fake assistant replies on reload. #7137
    .filter((m) => !m.ephemeral)
    .map((m) => ({
      id: m.id,
      role: m.role,
      content: m.content,
      thinking: m.thinking,
      markdown: m.markdown,
      notice: m.notice,
      terminalPartial: m.terminalPartial,
      // Preserve the verbatim-user-input flag so reloaded bubbles still skip
      // server-timestamp stripping.
      local: m.local,
      toolCall: m.toolCall,
      timestamp: m.timestamp.toISOString(),
    }));
}

/**
 * Merge only explicitly retained lifecycle notices into an otherwise
 * authoritative server transcript. A `persisted: false` WebSocket frame marks
 * its notice as locally durable; server hydration must not erase it merely
 * because a session backend exists but missed that message.
 *
 * Each local notice is anchored to the preceding local user turn and matched
 * to the corresponding server user occurrence. This keeps a missed notice
 * before later turns and prevents an unrelated identical assistant message
 * from consuming the fallback.
 */
const RUNTIME_USER_ENVELOPE_RE =
  /^\[CURRENT DATE & TIME: [^\]\r\n]+\]\r?\n\r?\n/;

/** Resolve a persisted runtime user row to the verbatim ingress content used
 *  by the web composer. Only the runtime-owned leading envelope is removed;
 *  a matching-looking string inside the message remains ordinary content. */
function rawServerUserContent(content: string): string {
  return content.replace(RUNTIME_USER_ENVELOPE_RE, '');
}

function sameUserOccurrence(
  serverMessage: PersistedChatBubble,
  localMessage: PersistedChatBubble,
): boolean {
  return serverMessage.role === 'user'
    && rawServerUserContent(serverMessage.content) === localMessage.content;
}

function isRetainedTerminalMessage(message: PersistedChatBubble): boolean {
  return message.notice === true || message.terminalPartial === true;
}

export function mergeServerHistoryWithLocalNotices(
  server: PersistedChatBubble[],
  local: PersistedChatBubble[],
): PersistedChatBubble[] {
  const insertions = new Map<number, PersistedChatBubble[]>();
  const matchedServerMessages = new Set<number>();
  for (let localIndex = 0; localIndex < local.length; localIndex += 1) {
    const message = local[localIndex];
    if (!message) continue;
    if (message.notice !== true) continue;

    let localAnchorIndex = localIndex - 1;
    while (localAnchorIndex >= 0 && local[localAnchorIndex]?.role !== 'user') {
      localAnchorIndex -= 1;
    }
    const localAnchor = local[localAnchorIndex];
    let serverAnchorIndex = -1;
    if (localAnchor) {
      let anchorOccurrence = 0;
      for (let index = 0; index <= localAnchorIndex; index += 1) {
        const candidate = local[index];
        if (candidate?.role === 'user' && candidate.content === localAnchor.content) {
          anchorOccurrence += 1;
        }
      }

      let seen = 0;
      serverAnchorIndex = server.findIndex((candidate) => {
        if (!sameUserOccurrence(candidate, localAnchor)) return false;
        seen += 1;
        return seen === anchorOccurrence;
      });
    }

    const nextTurnOffset = serverAnchorIndex < 0
      ? -1
      : server
        .slice(serverAnchorIndex + 1)
        .findIndex((candidate) => candidate.role === 'user');
    const nextTurnIndex = serverAnchorIndex < 0 || nextTurnOffset < 0
      ? server.length
      : serverAnchorIndex + 1 + nextTurnOffset;

    // The notice is the durable marker for one failed terminal sequence. Keep
    // any marked partial immediately before it, but never resurrect unrelated
    // local-only bubbles. Walk backwards so a missing partial is inserted
    // before an already-persisted notice and the original order is preserved.
    const terminalStart = localAnchorIndex + 1;
    const retained = local
      .slice(terminalStart, localIndex + 1)
      .filter(isRetainedTerminalMessage);
    let beforeIndex = nextTurnIndex;
    const serverRangeStart = serverAnchorIndex < 0 ? 0 : serverAnchorIndex + 1;
    for (let retainedIndex = retained.length - 1; retainedIndex >= 0; retainedIndex -= 1) {
      const retainedMessage = retained[retainedIndex];
      if (!retainedMessage) continue;
      let persistedIndex = -1;
      for (let index = beforeIndex - 1; index >= serverRangeStart; index -= 1) {
        const candidate = server[index];
        if (
          candidate
          && candidate.role === retainedMessage.role
          && candidate.content === retainedMessage.content
          && !matchedServerMessages.has(index)
        ) {
          persistedIndex = index;
          break;
        }
      }
      if (persistedIndex >= 0) {
        matchedServerMessages.add(persistedIndex);
        beforeIndex = persistedIndex;
        continue;
      }
      insertions.set(beforeIndex, [retainedMessage, ...(insertions.get(beforeIndex) ?? [])]);
    }
  }

  if (insertions.size === 0) return server;
  const merged: PersistedChatBubble[] = [];
  for (let index = 0; index <= server.length; index += 1) {
    merged.push(...(insertions.get(index) ?? []));
    const current = server[index];
    if (current) merged.push(current);
  }
  return merged;
}
