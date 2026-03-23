// web/src/hooks/useSessionManager.ts

import { useState, useCallback, useEffect } from 'react';
import { sessionStore } from '@/lib/sessionStore';
import type { Session, SessionMessage } from '@/types/session';

export function useSessionManager() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);

  // 初始化从 localStorage 加载
  useEffect(() => {
    setSessions(sessionStore.listSessions());
  }, []);

  const activeSession = activeSessionId
    ? sessions.find((s) => s.id === activeSessionId) ?? null
    : null;

  const refreshSessions = useCallback(() => {
    setSessions(sessionStore.listSessions());
  }, []);

  /** 创建新 session，设为 active，返回 id */
  const startNewSession = useCallback(
    (firstMessage: string): string => {
      const session = sessionStore.createSession(firstMessage);
      refreshSessions();
      setActiveSessionId(session.id);
      return session.id;
    },
    [refreshSessions],
  );

  /** 切换到历史 session */
  const switchSession = useCallback((id: string) => {
    setActiveSessionId(id);
  }, []);

  /** 回到欢迎页 */
  const goHome = useCallback(() => {
    setActiveSessionId(null);
  }, []);

  /** 往指定 session 追加消息 */
  const addMessage = useCallback(
    (sessionId: string, message: SessionMessage) => {
      sessionStore.addMessage(sessionId, message);
      refreshSessions();
    },
    [refreshSessions],
  );

  /** 删除 session */
  const deleteSession = useCallback(
    (id: string) => {
      sessionStore.deleteSession(id);
      refreshSessions();
      if (id === activeSessionId) setActiveSessionId(null);
    },
    [activeSessionId, refreshSessions],
  );

  return {
    sessions,
    activeSession,
    activeSessionId,
    startNewSession,
    switchSession,
    goHome,
    addMessage,
    deleteSession,
  };
}
