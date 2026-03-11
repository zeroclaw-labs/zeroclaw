import { useState, useEffect, useRef } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check } from 'lucide-react';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

// Copy button component for individual messages
function MessageCopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for non-secure contexts
      console.warn('Clipboard write failed');
    }
  };

  return (
    <button
      onClick={handleCopy}
      className="opacity-0 group-hover:opacity-100 focus:opacity-100 p-1.5 rounded-md bg-gray-700/50 hover:bg-gray-600 text-gray-400 hover:text-white transition-all duration-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
      aria-label={copied ? 'Copied' : 'Copy message'}
      title={copied ? 'Copied!' : 'Copy to clipboard'}
    >
      {copied ? (
        <Check className="h-3.5 w-3.5 text-green-400" />
      ) : (
        <Copy className="h-3.5 w-3.5" />
      )}
    </button>
  );
}

export default function AgentChat() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef('');

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
                id: crypto.randomUUID(),
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
              id: crypto.randomUUID(),
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
              id: crypto.randomUUID(),
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
              id: crypto.randomUUID(),
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
        id: crypto.randomUUID(),
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
    inputRef.current?.focus();
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-gray-500">
            <Bot className="h-12 w-12 mb-3 text-gray-600" />
            <p className="text-lg font-medium">ZeroClaw Agent</p>
            <p className="text-sm mt-1">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`group flex items-start gap-3 ${
              msg.role === 'user' ? 'flex-row-reverse' : ''
            }`}
          >
            <div
              className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${
                msg.role === 'user'
                  ? 'bg-blue-600'
                  : 'bg-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <User className="h-4 w-4 text-white" />
              ) : (
                <Bot className="h-4 w-4 text-white" />
              )}
            </div>
            <div className="flex flex-col gap-1 max-w-[75%]">
              <div
                className={`relative rounded-xl px-4 py-3 ${
                  msg.role === 'user'
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-800 text-gray-100 border border-gray-700'
                }`}
              >
                <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
                <p
                  className={`text-xs mt-1 ${
                    msg.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                  }`}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
              </div>
              {/* Copy button - appears on hover/focus */}
              <div className={`flex ${msg.role === 'user' ? 'justify-end' : 'justify-start'}`}>
                <MessageCopyButton content={msg.content} />
              </div>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-gray-700 flex items-center justify-center">
              <Bot className="h-4 w-4 text-white" />
            </div>
            <div className="bg-gray-800 border border-gray-700 rounded-xl px-4 py-3">
              <div className="flex items-center gap-1">
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '0ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '150ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '300ms' }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">Typing...</p>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-gray-800 bg-gray-900 p-4">
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
              className="w-full bg-gray-800 border border-gray-700 rounded-xl px-4 py-3 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-50"
            />
          </div>
          <button
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-xl p-3 transition-colors"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="flex items-center justify-center mt-2 gap-2">
          <span
            className={`inline-block h-2 w-2 rounded-full ${
              connected ? 'bg-green-500' : 'bg-red-500'
            }`}
          />
          <span className="text-xs text-gray-500">
            {connected ? 'Connected' : 'Disconnected'}
          </span>
        </div>
      </div>
    </div>
  );
}
