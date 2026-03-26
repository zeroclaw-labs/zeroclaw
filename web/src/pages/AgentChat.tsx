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
        case 'thinking':
          setTyping(true);
          pendingThinkingRef.current += msg.content ?? '';
          setStreamingThinking(pendingThinkingRef.current);
          break;

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
      pendingThinkingRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    clearDraft();
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.style.height = `${Math.min(inputRef.current.scrollHeight, 200)}px`;
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
        <div className="flex items-center gap-2 px-3 py-1.5" style={{ borderBottom: '1px solid var(--pc-border)' }}>
          <button
            onClick={() => setSidebarOpen(!sidebarOpen)}
            className="p-1.5 rounded-lg transition-all"
            style={{ color: 'var(--pc-text-muted)' }}
            onMouseEnter={(e) => {
              e.currentTarget.style.color = 'var(--pc-text-primary)';
              e.currentTarget.style.background = 'var(--pc-hover)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.color = 'var(--pc-text-muted)';
              e.currentTarget.style.background = 'transparent';
            }}
          >
            {sidebarOpen ? (
              <PanelLeftClose className="h-4 w-4" />
            ) : (
              <PanelLeft className="h-4 w-4" />
            )}
          </button>
          {activeSession && (
            <span className="text-xs truncate" style={{ color: 'var(--pc-text-muted)' }}>{activeSession.title}</span>
          )}
        </div>

        {/* Connection error banner */}
        {error && (
          <div className="px-4 py-2 flex items-center gap-2 text-sm animate-fade-in" style={{ background: 'rgba(239, 68, 68, 0.08)', borderBottom: '1px solid rgba(239, 68, 68, 0.2)', color: '#f87171' }}>
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            {error}
          </div>
        )}

        {/* Messages area */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {/* Welcome page — no active session */}
          {!activeSession && (
            <div className="flex flex-col items-center justify-center h-full animate-fade-in" style={{ color: 'var(--pc-text-muted)' }}>
              <div
                className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float"
                style={{ background: 'var(--pc-accent-glow)' }}
              >
                <Bot className="h-8 w-8" style={{ color: 'var(--pc-accent)' }} />
              </div>
              <p className="text-lg font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>ZeroClaw Agent</p>
              <p className="text-sm">Send a message to start a new conversation</p>
            </div>
          )}

          {/* Active session with no messages yet (edge case during creation) */}
          {activeSession && currentMessages.length === 0 && (
            <div className="flex flex-col items-center justify-center h-full animate-fade-in" style={{ color: 'var(--pc-text-muted)' }}>
              <Bot className="h-8 w-8 mb-2" style={{ color: 'var(--pc-accent)' }} />
              <p className="text-sm">Send a message to start the conversation</p>
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
                      ? 'var(--pc-accent)'
                      : 'var(--pc-bg-elevated)',
                  border: `1px solid ${msg.role === 'user' ? 'var(--pc-accent)' : 'var(--pc-border)'}`,
                }}
              >
                {msg.role === 'user' ? (
                  <User className="h-4 w-4 text-white" />
                ) : (
                  <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
                )}
              </div>
              <div className="relative max-w-[75%]">
                <div
                  className="rounded-2xl px-4 py-3 border"
                  style={{
                    background:
                      msg.role === 'user'
                        ? 'var(--pc-accent-glow)'
                        : 'var(--pc-bg-elevated)',
                    borderColor: msg.role === 'user' ? 'var(--pc-accent-dim)' : 'var(--pc-border)',
                    color: 'var(--pc-text-primary)',
                  }}
                >
                  <p className="text-sm whitespace-pre-wrap break-words leading-relaxed">{msg.content}</p>
                  <p
                    className="text-[10px] mt-1.5"
                    style={{ color: msg.role === 'user' ? 'var(--pc-accent-light)' : 'var(--pc-text-faint)' }}
                  >
                    {new Date(msg.timestamp).toLocaleTimeString()}
                  </p>
                </div>
                <button
                  onClick={() => handleCopy(msg.id, msg.content)}
                  aria-label="Copy message"
                  className="absolute top-1 right-1 opacity-0 group-hover:opacity-100 transition-all duration-300 p-1.5 rounded-lg"
                  style={{
                    background: 'var(--pc-bg-elevated)',
                    border: '1px solid var(--pc-border)',
                    color: 'var(--pc-text-muted)',
                  }}
                  onMouseEnter={(e) => {
                    e.currentTarget.style.color = 'var(--pc-text-primary)';
                    e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
                  }}
                  onMouseLeave={(e) => {
                    e.currentTarget.style.color = 'var(--pc-text-muted)';
                    e.currentTarget.style.borderColor = 'var(--pc-border)';
                  }}
                >
                  {copiedId === msg.id ? (
                    <Check className="h-3 w-3" style={{ color: '#34d399' }} />
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
                className="flex-shrink-0 w-8 h-8 rounded-xl flex items-center justify-center border"
                style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}
              >
                <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
              </div>
              <div
                className="rounded-2xl px-4 py-3 border flex items-center gap-1.5"
                style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}
              >
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ background: 'var(--pc-accent)', animationDelay: '0ms' }} />
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ background: 'var(--pc-accent)', animationDelay: '150ms' }} />
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ background: 'var(--pc-accent)', animationDelay: '300ms' }} />
              </div>
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>

        {/* Input area */}
        <div className="border-t p-4" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}>
          <div className="flex items-center gap-3 max-w-4xl mx-auto">
            <textarea
              ref={inputRef}
              rows={1}
              value={input}
              onChange={handleTextareaChange}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="flex-1 px-4 py-2.5 text-sm rounded-xl resize-none disabled:opacity-40 focus:outline-none transition-colors"
              style={{
                minHeight: '44px',
                maxHeight: '200px',
                background: 'var(--pc-bg-input)',
                border: '1px solid var(--pc-border)',
                color: 'var(--pc-text-primary)',
              }}
              onFocus={(e) => {
                e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
              }}
              onBlur={(e) => {
                e.currentTarget.style.borderColor = 'var(--pc-border)';
              }}
            />
            <button
              type="button"
              onClick={handleSend}
              disabled={!connected || !input.trim()}
              className="flex-shrink-0 w-10 h-10 rounded-xl flex items-center justify-center transition-all disabled:opacity-40 disabled:cursor-not-allowed"
              style={{
                background: connected && input.trim() ? 'var(--pc-accent)' : 'var(--pc-bg-elevated)',
                border: '1px solid var(--pc-border)',
              }}
            >
              <Send className="h-5 w-5 text-white" />
            </button>
          </div>
          <div className="flex items-center justify-center mt-2 gap-2">
            <span
              className="w-1.5 h-1.5 rounded-full"
              style={connected
                ? { background: 'var(--color-status-success)', boxShadow: '0 0 6px var(--color-status-success)' }
                : { background: 'var(--color-status-error)', boxShadow: '0 0 6px var(--color-status-error)' }
              }
            />
            <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </div>
        </div>
      </div>
    </div>
  );
}
