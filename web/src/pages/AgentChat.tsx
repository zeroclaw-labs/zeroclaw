import { useState, useEffect, useRef, useCallback } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import { useDraft } from '@/hooks/useDraft';

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

const DRAFT_KEY = 'agent-chat';

export default function AgentChat() {
  const { draft, saveDraft, clearDraft } = useDraft(DRAFT_KEY);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState(draft);
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const pendingContentRef = useRef('');

  useEffect(() => {
    saveDraft(input);
  }, [input, saveDraft]);

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

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;

        case 'message':
        case 'done': {
          const content = msg.full_response ?? msg.content ?? pendingContentRef.current;
          if (content) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent',
                content,
                timestamp: new Date(),
              },
            ]);
          }
          pendingContentRef.current = '';
          setTyping(false);
          break;
        }

        case 'tool_call':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Tool Call] ${msg.name ?? 'unknown'}(${JSON.stringify(msg.args ?? {})})`,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'tool_result':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Tool Result] ${msg.output ?? ''}`,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'error':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `[Error] ${msg.message ?? 'Unknown error'}`,
              timestamp: new Date(),
            },
          ]);
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
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'user',
        content: trimmed,
        timestamp: new Date(),
      },
    ]);

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
  };

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

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {error && (
        <div className="px-4 py-2 border-b flex items-center gap-2 text-sm animate-fade-in" style={{ backgroundColor: 'var(--color-status-error)', opacity: 0.1, borderColor: 'var(--color-status-error)', color: 'var(--color-status-error)' }}>
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full animate-fade-in" style={{ color: 'var(--color-text-muted)' }}>
            <div className="h-16 w-16 rounded-2xl flex items-center justify-center mb-4 animate-float" style={{ background: 'linear-gradient(135deg, var(--color-accent-blue), var(--color-accent-cyan))', opacity: 0.1 }}>
              <Bot className="h-8 w-8" style={{ color: 'var(--color-accent-blue)' }} />
            </div>
            <p className="text-lg font-semibold mb-1" style={{ color: 'var(--color-text-primary)' }}>ZeroClaw Agent</p>
            <p className="text-sm" style={{ color: 'var(--color-text-muted)' }}>Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg, idx) => (
          <div
            key={msg.id}
            className={`group flex items-start gap-3 ${
              msg.role === 'user' ? 'flex-row-reverse animate-slide-in-right' : 'animate-slide-in-left'
            }`}
            style={{ animationDelay: `${Math.min(idx * 30, 200)}ms` }}
          >
            <div
              className="flex-shrink-0 w-8 h-8 rounded-xl flex items-center justify-center"
              style={{
                background: msg.role === 'user'
                  ? 'linear-gradient(135deg, var(--color-accent-blue), var(--color-accent-blue-hover))'
                  : 'var(--color-bg-secondary)'
              }}
            >
              {msg.role === 'user' ? (
                <User className="h-4 w-4 text-white" />
              ) : (
                <Bot className="h-4 w-4" style={{ color: 'var(--color-accent-blue)' }} />
              )}
            </div>
            <div className="relative max-w-[75%]">
              <div
                className={`rounded-2xl px-4 py-3 ${
                  msg.role === 'user'
                    ? 'text-white'
                    : ''
                }`}
                style={{
                  background: msg.role === 'user'
                    ? 'linear-gradient(135deg, var(--color-accent-blue), var(--color-accent-blue-hover))'
                    : 'var(--color-bg-card)',
                  border: msg.role === 'agent' ? '1px solid var(--color-border-default)' : 'none'
                }}
              >
                <p className="text-sm whitespace-pre-wrap break-words" style={{ color: msg.role === 'user' ? 'white' : 'var(--color-text-primary)' }}>{msg.content}</p>
                <p
                  className="text-xs mt-1.5"
                  style={{ color: msg.role === 'user' ? 'rgba(255,255,255,0.5)' : 'var(--color-text-muted)' }}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
              </div>
              <button
                onClick={() => handleCopy(msg.id, msg.content)}
                aria-label="Copy message"
                className="absolute top-1 right-1 opacity-0 group-hover:opacity-100 transition-all duration-300 p-1.5 rounded-lg border hover:opacity-80"
                style={{ backgroundColor: 'var(--color-bg-input)', borderColor: 'var(--color-border-default)', color: 'var(--color-text-muted)' }}
              >
                {copiedId === msg.id ? (
                  <Check className="h-3 w-3" style={{ color: 'var(--color-status-success)' }} />
                ) : (
                  <Copy className="h-3 w-3" />
                )}
              </button>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3 animate-fade-in">
            <div className="flex-shrink-0 w-8 h-8 rounded-xl flex items-center justify-center" style={{ background: 'var(--color-bg-secondary)' }}>
              <Bot className="h-4 w-4" style={{ color: 'var(--color-accent-blue)' }} />
            </div>
            <div className="rounded-2xl px-4 py-3 border" style={{ background: 'var(--color-bg-card)', borderColor: 'var(--color-border-default)' }}>
              <div className="flex items-center gap-1.5">
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ backgroundColor: 'var(--color-accent-blue)', animationDelay: '0ms' }} />
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ backgroundColor: 'var(--color-accent-blue)', animationDelay: '150ms' }} />
                <span className="w-1.5 h-1.5 rounded-full animate-bounce" style={{ backgroundColor: 'var(--color-accent-blue)', animationDelay: '300ms' }} />
              </div>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      <div className="border-t p-4" style={{ borderColor: 'var(--color-border-default)', backgroundColor: 'var(--color-bg-header)' }}>
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
            className="inline-block h-1.5 w-1.5 rounded-full"
            style={{ backgroundColor: connected ? 'var(--color-status-success)' : 'var(--color-status-error)' }}
          />
          <span className="text-xs" style={{ color: 'var(--color-text-muted)' }}>
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
      </div>
    </div>
  );
}
