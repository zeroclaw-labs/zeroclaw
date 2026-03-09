export interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

export interface HistoryMessage {
  role: string;
  content?: string;
}

interface StoredChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: string;
}

const AGENT_MESSAGES_KEY_PREFIX = 'zeroclaw.agent.messages.v1';
let fallbackIdCounter = 0;

export function createMessageId(): string {
  const id = globalThis.crypto?.randomUUID?.();
  if (id) {
    return id;
  }
  fallbackIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackIdCounter.toString(36)}`;
}

function storageKeyForSession(sessionId: string): string {
  return `${AGENT_MESSAGES_KEY_PREFIX}.${sessionId}`;
}

export function loadStoredMessages(sessionId: string): ChatMessage[] {
  try {
    const raw = window.localStorage.getItem(storageKeyForSession(sessionId));
    if (!raw) {
      return [];
    }
    const parsed = JSON.parse(raw) as StoredChatMessage[];
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed
      .filter((msg) => typeof msg.content === 'string' && msg.content.trim().length > 0)
      .map((msg) => ({
        id: msg.id || createMessageId(),
        role: msg.role === 'user' ? 'user' : 'agent',
        content: msg.content,
        timestamp: new Date(msg.timestamp),
      }));
  } catch {
    return [];
  }
}

export function saveStoredMessages(sessionId: string, messages: ChatMessage[]): void {
  try {
    const payload: StoredChatMessage[] = messages.map((msg) => ({
      id: msg.id,
      role: msg.role,
      content: msg.content,
      timestamp: msg.timestamp.toISOString(),
    }));
    window.localStorage.setItem(storageKeyForSession(sessionId), JSON.stringify(payload));
  } catch {
    // Best effort only.
  }
}

function mapHistoryMessages(historyMessages: HistoryMessage[]): ChatMessage[] {
  return historyMessages
    .filter((msg) => (msg.content ?? '').trim().length > 0)
    .map((msg) => ({
      id: createMessageId(),
      role: msg.role === 'user' ? 'user' : 'agent',
      content: (msg.content ?? '').trim(),
      timestamp: new Date(),
    }));
}

function hasMatchingPrefix(current: ChatMessage[], candidate: ChatMessage[]): boolean {
  if (candidate.length > current.length) {
    return false;
  }
  return candidate.every((msg, i) => {
    const currentMsg = current[i];
    if (!currentMsg) {
      return false;
    }
    return currentMsg.role === msg.role && currentMsg.content.trim() === msg.content.trim();
  });
}

export function mergeHistoryMessages(
  current: ChatMessage[],
  historyMessages: HistoryMessage[],
): ChatMessage[] {
  const mappedHistory = mapHistoryMessages(historyMessages);
  if (mappedHistory.length === 0) {
    // Never erase current UI context from empty history payloads.
    return current;
  }
  if (current.length > 0 && hasMatchingPrefix(current, mappedHistory)) {
    // Keep richer local timeline if server history is behind.
    return current;
  }
  return mappedHistory;
}
