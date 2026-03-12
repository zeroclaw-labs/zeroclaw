'use client';

import { useState, useEffect, useRef } from 'react';
import { useRouter } from 'next/navigation';
import Link from 'next/link';
import { Send, Bot, User, AlertCircle, ArrowLeft, Settings, LogOut } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';
import { getToken, clearToken, isAuthenticated } from '@/lib/auth';

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK = 'Tool execution completed, but no final response text was returned.';

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;
  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random().toString(36).slice(2, 10)}`;
}

export default function ChatPage() {
  const router = useRouter();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [authChecked, setAuthChecked] = useState(false);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef('');

  // Auth gate: redirect to /auth if not authenticated
  useEffect(() => {
    if (!isAuthenticated()) {
      router.replace('/auth?redirect=/chat');
      return;
    }
    setAuthChecked(true);
  }, [router]);

  // WebSocket connection (only after auth is confirmed)
  useEffect(() => {
    if (!authChecked) return;

    const ws = new WebSocketClient();

    ws.onOpen = () => { setConnected(true); setError(null); };
    ws.onClose = () => { setConnected(false); };
    ws.onError = () => { setError('Connection error. Attempting to reconnect...'); };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'history': {
          const restored: ChatMessage[] = (msg.messages ?? [])
            .filter((entry) => entry.content?.trim())
            .map((entry) => ({
              id: makeMessageId(),
              role: (entry.role === 'user' ? 'user' : 'agent') as 'user' | 'agent',
              content: entry.content.trim(),
              timestamp: new Date(),
            }));
          setMessages(restored);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }
        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;
        case 'message':
        case 'done': {
          const content = (msg.full_response ?? msg.content ?? pendingContentRef.current ?? '').trim();
          const finalContent = content || EMPTY_DONE_FALLBACK;
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: finalContent, timestamp: new Date() }]);
          pendingContentRef.current = '';
          setTyping(false);
          break;
        }
        case 'tool_call':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Tool Call] ${msg.name ?? 'unknown'}(${JSON.stringify(msg.args ?? {})})`, timestamp: new Date() }]);
          break;
        case 'tool_result':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Tool Result] ${msg.output ?? ''}`, timestamp: new Date() }]);
          break;
        case 'error':
          setMessages((prev) => [...prev, { id: makeMessageId(), role: 'agent', content: `[Error] ${msg.message ?? 'Unknown error'}`, timestamp: new Date() }]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;
    return () => { ws.disconnect(); };
  }, [authChecked]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;
    setMessages((prev) => [...prev, { id: makeMessageId(), role: 'user', content: trimmed, timestamp: new Date() }]);
    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }
    setInput('');
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleLogout = () => {
    clearToken();
    wsRef.current?.disconnect();
    router.replace('/auth');
  };

  // Show nothing while checking auth
  if (!authChecked) {
    return (
      <div className="flex items-center justify-center h-screen bg-dark-950">
        <div className="flex flex-col items-center gap-3">
          <div className="h-8 w-8 border-2 border-primary-500 border-t-transparent rounded-full animate-spin" />
          <p className="text-sm text-dark-400">Loading...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col h-screen bg-dark-950">
      {/* Chat header bar */}
      <div className="flex items-center justify-between border-b border-dark-800/50 bg-dark-900/80 backdrop-blur-xl px-4 py-3 flex-shrink-0">
        <div className="flex items-center gap-3">
          <Link
            href="/"
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
            aria-label="Back to home"
          >
            <ArrowLeft className="h-4 w-4" />
          </Link>
          <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-primary-500/10 border border-primary-500/20">
            <span className="text-sm font-bold text-primary-400">M</span>
          </div>
          <div>
            <h2 className="text-sm font-semibold text-dark-100">MoA Agent</h2>
            <div className="flex items-center gap-1.5">
              <div className={`h-1.5 w-1.5 rounded-full ${connected ? 'bg-green-400' : 'bg-red-400'}`} />
              <span className="text-xs text-dark-400">
                {connected ? 'Connected' : 'Disconnected'}
              </span>
            </div>
          </div>
        </div>
        <div className="flex items-center gap-2">
          <Link
            href="/workspace/dashboard"
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-dark-200 transition-all"
            aria-label="Workspace"
          >
            <Settings className="h-4 w-4" />
          </Link>
          <button
            onClick={handleLogout}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-dark-400 hover:bg-dark-800 hover:text-red-400 transition-all"
            aria-label="Logout"
          >
            <LogOut className="h-4 w-4" />
          </button>
        </div>
      </div>

      {/* Connection error banner */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300 flex-shrink-0">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />{error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4 custom-scrollbar">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-dark-500">
            <div className="flex h-16 w-16 items-center justify-center rounded-2xl bg-primary-500/10 border border-primary-500/20 mb-4">
              <Bot className="h-8 w-8 text-primary-400" />
            </div>
            <p className="text-lg font-semibold text-dark-200">MoA Agent</p>
            <p className="text-sm mt-1 text-dark-400">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div key={msg.id} className={`flex items-start gap-3 chat-bubble-enter ${msg.role === 'user' ? 'flex-row-reverse' : ''}`}>
            <div className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${msg.role === 'user' ? 'bg-primary-500' : 'bg-dark-700 border border-dark-600'}`}>
              {msg.role === 'user' ? <User className="h-4 w-4 text-white" /> : <Bot className="h-4 w-4 text-primary-400" />}
            </div>
            <div className={`max-w-[75%] rounded-2xl px-4 py-3 ${msg.role === 'user' ? 'bg-primary-500 text-white rounded-br-md' : 'bg-dark-800 text-dark-100 border border-dark-700/50 rounded-bl-md'}`}>
              <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
              <p className={`text-xs mt-1 ${msg.role === 'user' ? 'text-primary-200/60' : 'text-dark-500'}`}>
                {msg.timestamp.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
              </p>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3 chat-bubble-enter">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-dark-700 border border-dark-600 flex items-center justify-center">
              <Bot className="h-4 w-4 text-primary-400" />
            </div>
            <div className="bg-dark-800 border border-dark-700/50 rounded-2xl rounded-bl-md px-5 py-4">
              <div className="typing-indicator flex gap-1.5">
                <span></span>
                <span></span>
                <span></span>
              </div>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-dark-800/50 bg-dark-900/80 backdrop-blur-xl p-4 flex-shrink-0">
        <div className="flex items-center gap-3 max-w-4xl mx-auto">
          <div className="flex-1 relative">
            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="w-full bg-dark-800/50 border border-dark-700 rounded-xl px-4 py-3 text-sm text-dark-100 placeholder-dark-500 focus:outline-none focus:ring-2 focus:ring-primary-500/50 focus:border-primary-500/50 disabled:opacity-50 transition-all"
            />
          </div>
          <button
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-primary-500 hover:bg-primary-600 disabled:bg-dark-700 disabled:text-dark-500 text-white rounded-xl p-3 transition-all active:scale-95"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <p className="mt-2 text-center text-[10px] text-dark-600">
          Shift+Enter for newline | Enter to send
        </p>
      </div>
    </div>
  );
}
