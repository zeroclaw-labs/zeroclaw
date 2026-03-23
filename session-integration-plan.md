# TeleClaws Multi-Session 功能集成 — Claude Code 执行指令

> **仓库**: https://github.com/zeroclaw-labs/zeroclaw.git
> **工作目录**: `web/` (Vite + React 19 + TypeScript + Tailwind v4 + react-router-dom v7)
> **目标**: 在 `/agent` 路由的 `AgentChat` 页面中集成多 Session 管理。纯前端改造，不改 ZeroClaw 后端。

---

## 项目结构概览（供参考，不需要修改这些文件除非明确指定）

```
web/src/
├── App.tsx                        # 路由: Layout 包裹，/agent → AgentChat
├── main.tsx
├── index.css                      # Electric Blue 主题，已有 .btn-electric, .input-electric, .glass-card 等
├── components/layout/
│   ├── Layout.tsx                 # 外层布局: 左侧固定 Sidebar(w-60) + 右侧 main
│   ├── Sidebar.tsx                # 全局导航栏（Dashboard/Agent/Tools/...）← 不要修改
│   └── Header.tsx                 # 顶部标题栏 h-14
├── pages/
│   ├── AgentChat.tsx              # ★ 主要修改目标
│   └── ...
├── hooks/
│   ├── useDraft.ts                # 内存草稿，基于 DraftContext（无 localStorage）
│   ├── useAuth.ts
│   ├── useApi.ts
│   ├── useSSE.ts
│   └── useWebSocket.ts
├── lib/
│   ├── ws.ts                      # WebSocketClient class — sendMessage(content) 发送 {type:'message', content}
│   ├── api.ts                     # REST: apiFetch() 封装，有 /api/* 各种接口
│   ├── auth.ts                    # getToken/setToken/clearToken (localStorage)
│   ├── uuid.ts                    # generateUUID() — crypto.randomUUID fallback
│   ├── sse.ts
│   └── i18n.ts
└── types/
    └── api.ts                     # WsMessage, StatusResponse, MemoryEntry 等类型
```

**关键已知信息**:
- `Layout.tsx` 结构: `<Sidebar />(fixed w-60)` + `<div className="ml-60"><Header /><main><Outlet /></main></div>`
- `AgentChat` 的容器高度是 `h-[calc(100vh-3.5rem)]`（减去 Header 的 h-14 = 3.5rem）
- WebSocket 地址: `ws(s)://{host}/ws/chat?token=...&session_id=...`
- `ws.sendMessage(content)` 发送 `JSON.stringify({ type: 'message', content })`
- `WsMessage.type`: `'message' | 'chunk' | 'tool_call' | 'tool_result' | 'done' | 'error'`
- `ws.ts` 已有 `SESSION_STORAGE_KEY = 'zeroclaw_session_id'`（这是后端的 session_id，用于 WebSocket 连接，和我们的前端 UI Session 概念不同，不要混淆）
- 项目不用 `npm run dev`，构建后嵌入 Rust gateway 运行

---

## 执行步骤

### Step 1: 新建 `web/src/types/session.ts`

创建 Session 相关类型定义。

```typescript
// web/src/types/session.ts

export interface SessionMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: string;  // ISO 8601，便于 JSON 序列化到 localStorage
  toolCall?: {
    name: string;
    args: Record<string, unknown>;
    output?: string;
  };
}

export interface Session {
  id: string;
  title: string;
  created_at: string;
  updated_at: string;
  messages: SessionMessage[];
  status: 'active' | 'archived';
}
```

---

### Step 2: 新建 `web/src/lib/sessionStore.ts`

实现基于 localStorage 的 Session CRUD。

```typescript
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
```

---

### Step 3: 新建 `web/src/hooks/useSessionManager.ts`

Session 状态管理 Hook。

```typescript
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
```

---

### Step 4: 新建 `web/src/components/SessionSidebar.tsx`

Agent 页面专属的 Session 侧边栏组件。
注意：这**不是**全局导航 `components/layout/Sidebar.tsx`，是 AgentChat 内部的子组件，不要搞混。

```typescript
// web/src/components/SessionSidebar.tsx

import { useState } from 'react';
import { Plus, Trash2, MessageSquare, Search } from 'lucide-react';
import type { Session } from '@/types/session';

interface SessionSidebarProps {
  sessions: Session[];
  activeSessionId: string | null;
  onSelectSession: (id: string) => void;
  onNewChat: () => void;
  onDeleteSession: (id: string) => void;
}

/** 按时间分组 */
function groupByDate(sessions: Session[]): { label: string; items: Session[] }[] {
  const now = new Date();
  const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const yesterdayStart = todayStart - 86_400_000;
  const weekStart = todayStart - 7 * 86_400_000;

  const buckets: Record<string, Session[]> = {
    Today: [],
    Yesterday: [],
    'Last 7 days': [],
    Older: [],
  };

  for (const s of sessions) {
    const t = new Date(s.updated_at).getTime();
    if (t >= todayStart) buckets['Today'].push(s);
    else if (t >= yesterdayStart) buckets['Yesterday'].push(s);
    else if (t >= weekStart) buckets['Last 7 days'].push(s);
    else buckets['Older'].push(s);
  }

  return Object.entries(buckets)
    .filter(([, items]) => items.length > 0)
    .map(([label, items]) => ({ label, items }));
}

export default function SessionSidebar({
  sessions,
  activeSessionId,
  onSelectSession,
  onNewChat,
  onDeleteSession,
}: SessionSidebarProps) {
  const [search, setSearch] = useState('');

  const filtered = search
    ? sessions.filter((s) => s.title.toLowerCase().includes(search.toLowerCase()))
    : sessions;
  const groups = groupByDate(filtered);

  return (
    <div
      className="flex flex-col h-full w-[260px] flex-shrink-0 border-r border-[#1a1a3e]/40"
      style={{ background: 'linear-gradient(180deg, rgba(8,8,24,0.95), rgba(5,5,16,0.98))' }}
    >
      {/* New chat button */}
      <div className="p-3">
        <button
          onClick={onNewChat}
          className="w-full flex items-center justify-center gap-2 px-3 py-2.5 rounded-xl text-sm font-medium transition-all duration-300 border border-[#1a1a3e] text-[#8892a8] hover:text-white hover:border-[#0080ff40] hover:bg-[#0080ff10]"
        >
          <Plus className="h-4 w-4" />
          New chat
        </button>
      </div>

      {/* Search */}
      <div className="px-3 pb-2">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-[#556080]" />
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search sessions..."
            className="w-full pl-8 pr-3 py-1.5 rounded-lg text-xs bg-[#0a0a18] border border-[#1a1a3e]/60 text-[#e8edf5] placeholder:text-[#556080] focus:outline-none focus:border-[#0080ff40]"
          />
        </div>
      </div>

      {/* Session list */}
      <div className="flex-1 overflow-y-auto px-2 pb-3">
        {groups.length === 0 && (
          <div className="px-3 py-8 text-center">
            <MessageSquare className="h-8 w-8 text-[#1a1a3e] mx-auto mb-2" />
            <p className="text-xs text-[#556080]">No sessions yet</p>
          </div>
        )}

        {groups.map((group) => (
          <div key={group.label} className="mb-3">
            <p className="px-2 py-1.5 text-[10px] font-medium text-[#556080] uppercase tracking-wider">
              {group.label}
            </p>
            {group.items.map((session) => {
              const isActive = session.id === activeSessionId;
              return (
                <button
                  key={session.id}
                  onClick={() => onSelectSession(session.id)}
                  className={`group w-full flex items-center gap-2 px-2.5 py-2 rounded-lg text-left text-sm transition-all duration-200 mb-0.5 ${
                    isActive
                      ? 'text-white border-l-2 border-[#0080ff]'
                      : 'text-[#8892a8] hover:text-white hover:bg-[#1a1a3e]/40 border-l-2 border-transparent'
                  }`}
                  style={isActive ? { background: 'rgba(0,128,255,0.1)' } : undefined}
                >
                  <MessageSquare className="h-3.5 w-3.5 flex-shrink-0 opacity-50" />
                  <span className="flex-1 truncate text-xs">{session.title}</span>
                  <span
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteSession(session.id);
                    }}
                    className="opacity-0 group-hover:opacity-100 p-1 rounded hover:bg-[#ff446620] hover:text-[#ff4466] transition-all cursor-pointer"
                  >
                    <Trash2 className="h-3 w-3" />
                  </span>
                </button>
              );
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
```

---

### Step 5: 重写 `web/src/pages/AgentChat.tsx`

**完全替换**该文件内容。这是核心改造，集成 Session 逻辑 + 新布局。

**关键设计决策**:
1. `/new` 命令通过 WebSocket `sendMessage('/new')` 发送（和普通消息走同一个通道，底层发送 `{"type":"message","content":"/new"}`）
2. 后端对 `/new` 可能有响应消息，用 `awaitingNewAckRef` 标志位丢弃该响应
3. 使用 `useRef` 追踪 `activeSessionId` 解决 WebSocket 回调闭包捕获旧值问题
4. 新对话流程: 创建 Session → 发 `/new` → 等 300ms → 发用户消息
5. 继续对话流程: 直接发消息，不发 `/new`（Agent 的 memory 跨 session 共享）

```typescript
// web/src/pages/AgentChat.tsx

import { useState, useEffect, useRef, useCallback } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check, PanelLeftClose, PanelLeft } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import type { SessionMessage } from '@/types/session';
import { WebSocketClient } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import { useDraft } from '@/hooks/useDraft';
import { useSessionManager } from '@/hooks/useSessionManager';
import SessionSidebar from '@/components/SessionSidebar';

const DRAFT_KEY = 'agent-chat';

export default function AgentChat() {
  const { draft, saveDraft, clearDraft } = useDraft(DRAFT_KEY);
  const {
    sessions,
    activeSession,
    activeSessionId,
    startNewSession,
    switchSession,
    goHome,
    addMessage,
    deleteSession,
  } = useSessionManager();

  const [input, setInput] = useState(draft);
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [copiedId, setCopiedId] = useState<string | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const pendingContentRef = useRef('');

  // Refs to track current session ids inside WebSocket callback closures
  const activeSessionIdRef = useRef<string | null>(null);
  const pendingSessionIdRef = useRef<string | null>(null);
  // Flag: when true, the next 'message'/'done' from backend is a /new response → discard it
  const awaitingNewAckRef = useRef(false);

  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);

  // Persist draft
  useEffect(() => {
    saveDraft(input);
  }, [input, saveDraft]);

  // WebSocket setup — runs once on mount
  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = () => {
      setConnected(false);
    };

    ws.onError = () => {
      setError('Connection error. Attempting to reconnect...');
    };

    const getTargetSessionId = (): string | null =>
      activeSessionIdRef.current ?? pendingSessionIdRef.current;

    const pushMessage = (msg: Omit<SessionMessage, 'id' | 'timestamp'>) => {
      const targetId = getTargetSessionId();
      if (!targetId) return;
      addMessage(targetId, {
        ...msg,
        id: generateUUID(),
        timestamp: new Date().toISOString(),
      });
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;

        case 'message':
        case 'done': {
          const content = msg.full_response ?? msg.content ?? pendingContentRef.current;
          pendingContentRef.current = '';
          setTyping(false);

          // If we're waiting for the /new acknowledgment, discard this response
          if (awaitingNewAckRef.current) {
            awaitingNewAckRef.current = false;
            break;
          }

          if (content) {
            pushMessage({ role: 'agent', content });
          }
          pendingSessionIdRef.current = null;
          break;
        }

        case 'tool_call':
          pushMessage({
            role: 'agent',
            content: `[Tool Call] ${msg.name ?? 'unknown'}(${JSON.stringify(msg.args ?? {})})`,
            toolCall: { name: msg.name ?? 'unknown', args: msg.args ?? {} },
          });
          break;

        case 'tool_result':
          pushMessage({
            role: 'agent',
            content: `[Tool Result] ${msg.output ?? ''}`,
          });
          break;

        case 'error':
          pushMessage({
            role: 'agent',
            content: `[Error] ${msg.message ?? 'Unknown error'}`,
          });
          setTyping(false);
          pendingContentRef.current = '';
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
    // addMessage is stable (useCallback with stable deps)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Auto-scroll when messages change or typing
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [activeSession?.messages.length, typing]);

  // ---- Send logic ----

  const handleSend = useCallback(async () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    let sessionId = activeSessionId;

    // New conversation: create session + send /new to clear context window
    if (!sessionId) {
      sessionId = startNewSession(trimmed);
      pendingSessionIdRef.current = sessionId;

      try {
        awaitingNewAckRef.current = true;
        wsRef.current.sendMessage('/new');
      } catch {
        setError('Failed to clear context. Please try again.');
        awaitingNewAckRef.current = false;
        return;
      }

      // Brief delay to let backend process /new before the real message
      await new Promise((r) => setTimeout(r, 300));
    }

    // Add user message to session store
    addMessage(sessionId, {
      id: generateUUID(),
      role: 'user',
      content: trimmed,
      timestamp: new Date().toISOString(),
    });

    // Send to backend via WebSocket
    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    clearDraft();
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.focus();
    }
  }, [input, activeSessionId, startNewSession, addMessage, clearDraft]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleTextareaChange = (e: React.ChangeEvent<HTMLTextAreaElement>) => {
    setInput(e.target.value);
    e.target.style.height = 'auto';
    e.target.style.height = `${Math.min(e.target.scrollHeight, 200)}px`;
  };

  const handleCopy = useCallback((msgId: string, content: string) => {
    navigator.clipboard.writeText(content).then(() => {
      setCopiedId(msgId);
      setTimeout(() => setCopiedId((prev) => (prev === msgId ? null : prev)), 2000);
    });
  }, []);

  const currentMessages = activeSession?.messages ?? [];

  return (
    <div className="flex h-[calc(100vh-3.5rem)]">
      {/* Session sidebar (collapsible) */}
      <div
        className={`transition-all duration-300 overflow-hidden flex-shrink-0 ${
          sidebarOpen ? 'w-[260px]' : 'w-0'
        }`}
      >
        <SessionSidebar
          sessions={sessions}
          activeSessionId={activeSessionId}
          onSelectSession={switchSession}
          onNewChat={goHome}
          onDeleteSession={deleteSession}
        />
      </div>

      {/* Main chat area */}
      <div className="flex flex-col flex-1 min-w-0">
        {/* Top bar: sidebar toggle + session title */}
        <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[#1a1a3e]/30">
          <button
            onClick={() => setSidebarOpen(!sidebarOpen)}
            className="p-1.5 rounded-lg text-[#556080] hover:text-white hover:bg-[#1a1a3e]/40 transition-all"
          >
            {sidebarOpen ? (
              <PanelLeftClose className="h-4 w-4" />
            ) : (
              <PanelLeft className="h-4 w-4" />
            )}
          </button>
          {activeSession && (
            <span className="text-xs text-[#556080] truncate">{activeSession.title}</span>
          )}
        </div>

        {/* Connection error banner */}
        {error && (
          <div className="px-4 py-2 bg-[#ff446615] border-b border-[#ff446630] flex items-center gap-2 text-sm text-[#ff6680] animate-fade-in">
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            {error}
          </div>
        )}

        {/* Messages area */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Welcome page — no active session */}
          {!activeSession && (
            <div className="flex flex-col items-center justify-center h-full text-[#334060] animate-fade-in">
              <div
                className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float"
                style={{ background: 'linear-gradient(135deg, #0080ff15, #0080ff08)' }}
              >
                <Bot className="h-8 w-8 text-[#0080ff]" />
              </div>
              <p className="text-lg font-semibold text-white mb-1">ZeroClaw Agent</p>
              <p className="text-sm text-[#556080]">Send a message to start a new conversation</p>
            </div>
          )}

          {/* Active session with no messages yet (edge case during creation) */}
          {activeSession && currentMessages.length === 0 && (
            <div className="flex flex-col items-center justify-center h-full text-[#334060] animate-fade-in">
              <Bot className="h-8 w-8 text-[#0080ff] mb-2" />
              <p className="text-sm text-[#556080]">Send a message to start the conversation</p>
            </div>
          )}

          {/* Render messages */}
          {currentMessages.map((msg, idx) => (
            <div
              key={msg.id}
              className={`group flex items-start gap-3 ${
                msg.role === 'user'
                  ? 'flex-row-reverse animate-slide-in-right'
                  : 'animate-slide-in-left'
              }`}
              style={{ animationDelay: `${Math.min(idx * 30, 200)}ms` }}
            >
              <div
                className="flex-shrink-0 w-8 h-8 rounded-xl flex items-center justify-center"
                style={{
                  background:
                    msg.role === 'user'
                      ? 'linear-gradient(135deg, #0080ff, #0060cc)'
                      : 'linear-gradient(135deg, #1a1a3e, #12122a)',
                }}
              >
                {msg.role === 'user' ? (
                  <User className="h-4 w-4 text-white" />
                ) : (
                  <Bot className="h-4 w-4 text-[#0080ff]" />
                )}
              </div>
              <div className="relative max-w-[75%]">
                <div
                  className={`rounded-2xl px-4 py-3 ${
                    msg.role === 'user'
                      ? 'text-white'
                      : 'text-[#e8edf5] border border-[#1a1a3e]'
                  }`}
                  style={{
                    background:
                      msg.role === 'user'
                        ? 'linear-gradient(135deg, #0080ff, #0066cc)'
                        : 'linear-gradient(135deg, rgba(13,13,32,0.8), rgba(10,10,26,0.6))',
                  }}
                >
                  <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
                  <p
                    className={`text-[10px] mt-1.5 ${
                      msg.role === 'user' ? 'text-white/50' : 'text-[#334060]'
                    }`}
                  >
                    {new Date(msg.timestamp).toLocaleTimeString()}
                  </p>
                </div>
                <button
                  onClick={() => handleCopy(msg.id, msg.content)}
                  aria-label="Copy message"
                  className="absolute top-1 right-1 opacity-0 group-hover:opacity-100 transition-all duration-300 p-1.5 rounded-lg bg-[#0a0a18] border border-[#1a1a3e] text-[#556080] hover:text-white hover:border-[#0080ff40]"
                >
                  {copiedId === msg.id ? (
                    <Check className="h-3 w-3 text-[#00e68a]" />
                  ) : (
                    <Copy className="h-3 w-3" />
                  )}
                </button>
              </div>
            </div>
          ))}

          {/* Typing indicator */}
          {typing && (
            <div className="flex items-start gap-3 animate-fade-in">
              <div
                className="flex-shrink-0 w-8 h-8 rounded-xl flex items-center justify-center"
                style={{ background: 'linear-gradient(135deg, #1a1a3e, #12122a)' }}
              >
                <Bot className="h-4 w-4 text-[#0080ff]" />
              </div>
              <div
                className="rounded-2xl px-4 py-3 border border-[#1a1a3e]"
                style={{
                  background:
                    'linear-gradient(135deg, rgba(13,13,32,0.8), rgba(10,10,26,0.6))',
                }}
              >
                <div className="flex items-center gap-1.5">
                  <span
                    className="w-1.5 h-1.5 bg-[#0080ff] rounded-full animate-bounce"
                    style={{ animationDelay: '0ms' }}
                  />
                  <span
                    className="w-1.5 h-1.5 bg-[#0080ff] rounded-full animate-bounce"
                    style={{ animationDelay: '150ms' }}
                  />
                  <span
                    className="w-1.5 h-1.5 bg-[#0080ff] rounded-full animate-bounce"
                    style={{ animationDelay: '300ms' }}
                  />
                </div>
              </div>
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>

        {/* Input area */}
        <div
          className="border-t border-[#1a1a3e]/40 p-4"
          style={{
            background:
              'linear-gradient(180deg, rgba(8,8,24,0.9), rgba(5,5,16,0.95))',
          }}
        >
          <div className="flex items-end gap-3 max-w-4xl mx-auto">
            <div className="flex-1">
              <textarea
                ref={inputRef}
                rows={1}
                value={input}
                onChange={handleTextareaChange}
                onKeyDown={handleKeyDown}
                placeholder={connected ? 'Type a message...' : 'Connecting...'}
                disabled={!connected}
                className="input-electric w-full px-4 py-3 text-sm resize-none overflow-y-auto disabled:opacity-40"
                style={{ minHeight: '44px', maxHeight: '200px' }}
              />
            </div>
            <button
              onClick={handleSend}
              disabled={!connected || !input.trim()}
              className="btn-electric flex-shrink-0 p-3 rounded-xl"
            >
              <Send className="h-5 w-5" />
            </button>
          </div>
          <div className="flex items-center justify-center mt-2 gap-2">
            <span
              className={`inline-block h-1.5 w-1.5 rounded-full glow-dot ${
                connected
                  ? 'text-[#00e68a] bg-[#00e68a]'
                  : 'text-[#ff4466] bg-[#ff4466]'
              }`}
            />
            <span className="text-[10px] text-[#334060]">
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
```

---

## 文件清单

| 操作 | 路径 | 说明 |
|------|------|------|
| **新建** | `web/src/types/session.ts` | Session / SessionMessage 类型定义 |
| **新建** | `web/src/lib/sessionStore.ts` | localStorage CRUD 工具 |
| **新建** | `web/src/hooks/useSessionManager.ts` | 状态管理 Hook |
| **新建** | `web/src/components/SessionSidebar.tsx` | Session 列表侧边栏组件 |
| **重写** | `web/src/pages/AgentChat.tsx` | 集成 Session 逻辑 + 新布局 |

**不修改的文件**（确认不要动）:
- `components/layout/Sidebar.tsx` — 全局导航栏
- `components/layout/Layout.tsx` — 外层布局
- `components/layout/Header.tsx` — 顶部栏
- `lib/ws.ts` — WebSocket 客户端
- `types/api.ts` — API 类型定义
- `App.tsx` — 路由配置
- `hooks/useDraft.ts` — 草稿 hook（继续使用）
- `index.css` — 主题样式（已有所需的 class）

---

## `/new` 命令处理详细说明

**发送方式**: `wsRef.current.sendMessage('/new')`
底层 JSON: `{"type":"message","content":"/new"}`（与普通消息格式完全一致）

**响应丢弃机制**:
- 发 `/new` 前设置 `awaitingNewAckRef.current = true`
- 收到下一条 `message`/`done` 时检查该标志：如果为 `true`，丢弃响应内容并重置标志
- 这确保 `/new` 的确认消息（如 "Context cleared"）不被写入 session

**如果后端对 `/new` 完全没有响应**（没有 message/done 回复），则 `awaitingNewAckRef` 会在用户的真正消息回复到来时误丢弃第一条回复。如果测试中发现这个问题，按如下方式修复：

在 `handleSend` 中，将 `/new` 发送后的 `await` 延迟后面加一行：
```typescript
await new Promise((r) => setTimeout(r, 300));
// 如果 300ms 内没收到 /new 响应，重置标志避免误丢弃
setTimeout(() => { awaitingNewAckRef.current = false; }, 1000);
```

---

## 验收测试

完成后依次验证：

1. `cd web && npm run build` — 无 TypeScript 编译错误
2. 打开 `/agent` 页面 → 左侧显示 Session 侧边栏（初始空），右侧显示欢迎页
3. 输入消息发送 → 自动创建新 Session → 侧边栏出现条目（标题为消息前 50 字符）
4. Agent 正常回复 → 消息出现在对话区
5. 继续输入第二条消息 → 在同一 Session 内继续（不触发 `/new`）
6. 点击 "New chat" → 回到欢迎页
7. 再次输入消息 → 创建第二个 Session → 先发 `/new` 再发消息
8. 点击侧边栏历史 Session → 显示该 Session 的历史消息
9. 在历史 Session 中继续输入 → 消息追加到该 Session（不发 `/new`）
10. 刷新页面 → Sessions 从 localStorage 恢复 → 侧边栏列表仍在
11. 删除 Session → 从列表移除 → 如果是当前 active 则回到欢迎页
12. 侧边栏折叠/展开按钮正常工作
