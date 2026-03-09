import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  loadStoredMessages,
  mergeHistoryMessages,
  saveStoredMessages,
  type ChatMessage,
} from './agentChatState';

class MemoryStorage implements Storage {
  private store = new Map<string, string>();

  get length(): number {
    return this.store.size;
  }

  clear(): void {
    this.store.clear();
  }

  getItem(key: string): string | null {
    return this.store.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.store.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.store.delete(key);
  }

  setItem(key: string, value: string): void {
    this.store.set(key, value);
  }
}

describe('agentChatState', () => {
  const storage = new MemoryStorage();
  const originalWindow = globalThis.window;

  beforeEach(() => {
    storage.clear();
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: { localStorage: storage },
    });
  });

  it('keeps current messages when server history is empty', () => {
    const current: ChatMessage[] = [
      { id: '1', role: 'user', content: 'hello', timestamp: new Date('2026-03-09T10:00:00Z') },
      { id: '2', role: 'agent', content: 'hi', timestamp: new Date('2026-03-09T10:00:02Z') },
    ];

    const merged = mergeHistoryMessages(current, []);
    expect(merged).toHaveLength(2);
    expect(merged[0]?.content).toBe('hello');
    expect(merged[1]?.content).toBe('hi');
  });

  it('keeps richer local timeline when history is only a prefix', () => {
    const current: ChatMessage[] = [
      { id: '1', role: 'user', content: 'hello', timestamp: new Date('2026-03-09T10:00:00Z') },
      { id: '2', role: 'agent', content: 'hi', timestamp: new Date('2026-03-09T10:00:02Z') },
      { id: '3', role: 'user', content: 'follow-up', timestamp: new Date('2026-03-09T10:00:03Z') },
    ];

    const merged = mergeHistoryMessages(current, [
      { role: 'user', content: 'hello' },
      { role: 'agent', content: 'hi' },
    ]);

    expect(merged).toHaveLength(3);
    expect(merged[2]?.content).toBe('follow-up');
  });

  it('replaces timeline when history diverges from local content', () => {
    const current: ChatMessage[] = [
      { id: '1', role: 'user', content: 'hello', timestamp: new Date('2026-03-09T10:00:00Z') },
      { id: '2', role: 'agent', content: 'hi', timestamp: new Date('2026-03-09T10:00:02Z') },
    ];

    const merged = mergeHistoryMessages(current, [
      { role: 'user', content: 'different prompt' },
      { role: 'agent', content: 'different response' },
    ]);

    expect(merged).toHaveLength(2);
    expect(merged[0]?.content).toBe('different prompt');
    expect(merged[1]?.content).toBe('different response');
  });

  it('round-trips persisted messages by session id', () => {
    const sessionId = 'sess_abc123';
    const initial: ChatMessage[] = [
      { id: '1', role: 'user', content: 'hello', timestamp: new Date('2026-03-09T10:00:00Z') },
      { id: '2', role: 'agent', content: 'hi', timestamp: new Date('2026-03-09T10:00:02Z') },
    ];

    saveStoredMessages(sessionId, initial);
    const restored = loadStoredMessages(sessionId);

    expect(restored).toHaveLength(2);
    expect(restored[0]?.role).toBe('user');
    expect(restored[1]?.role).toBe('agent');
    expect(restored[0]?.content).toBe('hello');
    expect(restored[1]?.content).toBe('hi');
  });

  it('isolates persisted messages across sessions', () => {
    saveStoredMessages('sess_one', [
      { id: '1', role: 'user', content: 'first', timestamp: new Date('2026-03-09T10:00:00Z') },
    ]);
    saveStoredMessages('sess_two', [
      { id: '2', role: 'user', content: 'second', timestamp: new Date('2026-03-09T10:00:01Z') },
    ]);

    expect(loadStoredMessages('sess_one')[0]?.content).toBe('first');
    expect(loadStoredMessages('sess_two')[0]?.content).toBe('second');
  });

  afterEach(() => {
    Object.defineProperty(globalThis, 'window', {
      configurable: true,
      value: originalWindow,
    });
  });
});
