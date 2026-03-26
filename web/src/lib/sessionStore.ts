// web/src/lib/sessionStore.ts

import type { Session, SessionMessage } from '@/types/session';
import { generateUUID } from '@/lib/uuid';

const STORAGE_KEY = 'teleclaws_sessions';

function loadAll(): Session[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Session[]) : [];
  } catch {
    return [];
  }
}

function saveAll(sessions: Session[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(sessions));
  } catch {
    // localStorage might be full or unavailable
  }
}

export const sessionStore = {
  /** 获取所有 sessions，按 updated_at 降序 */
  listSessions(): Session[] {
    return loadAll().sort(
      (a, b) => new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime(),
    );
  },

  /** 获取单个 session */
  getSession(id: string): Session | null {
    return loadAll().find((s) => s.id === id) ?? null;
  },

  /** 创建新 session。title = 第一条消息前 50 字符 */
  createSession(firstMessage: string): Session {
    const now = new Date().toISOString();
    const session: Session = {
      id: generateUUID(),
      title: firstMessage.slice(0, 50) + (firstMessage.length > 50 ? '...' : ''),
      created_at: now,
      updated_at: now,
      messages: [],
      status: 'active',
    };
    const all = loadAll();
    all.push(session);
    saveAll(all);
    return session;
  },

  /** 往 session 追加消息 */
  addMessage(sessionId: string, message: SessionMessage): void {
    const all = loadAll();
    const session = all.find((s) => s.id === sessionId);
    if (!session) return;
    session.messages.push(message);
    session.updated_at = new Date().toISOString();
    saveAll(all);
  },

  /** 更新 session 字段 */
  updateSession(id: string, updates: Partial<Pick<Session, 'title' | 'status'>>): void {
    const all = loadAll();
    const session = all.find((s) => s.id === id);
    if (!session) return;
    Object.assign(session, updates, { updated_at: new Date().toISOString() });
    saveAll(all);
  },

  /** 删除 session */
  deleteSession(id: string): void {
    const all = loadAll().filter((s) => s.id !== id);
    saveAll(all);
  },
};
