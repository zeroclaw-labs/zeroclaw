import { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check, SquarePen } from 'lucide-react';
import { marked } from 'marked';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';

// Configure marked for safe rendering
marked.setOptions({
  breaks: true,
  gfm: true,
});

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK =
  'Tool execution completed, but no final response text was returned.';

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;

  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 10)}`;
}

/** Render markdown string to sanitized HTML */
function renderMarkdown(content: string): string {
  try {
    return marked.parse(content, { async: false }) as string;
  } catch {
    // Fallback: escape HTML and preserve whitespace
    return content
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>');
  }
}

/** Copy button component */
function CopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for older browsers
      const textarea = document.createElement('textarea');
      textarea.value = content;
      textarea.style.position = 'fixed';
      textarea.style.opacity = '0';
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [content]);

  return (
    <button
      onClick={handleCopy}
      className="inline-flex items-center gap-1 text-xs text-gray-500 hover:text-gray-300 transition-colors px-2 py-1 rounded hover:bg-gray-700/50"
      title="Copy as Markdown"
    >
      {copied ? (
        <>
          <Check className="h-3.5 w-3.5 text-green-400" />
          <span className="text-green-400">Copied</span>
        </>
      ) : (
        <>
          <Copy className="h-3.5 w-3.5" />
          <span>Copy</span>
        </>
      )}
    </button>
  );
}

/** Rendered markdown message component */
function MarkdownMessage({ content }: { content: string }) {
  const html = useMemo(() => renderMarkdown(content), [content]);

  return (
    <div
      className="markdown-body text-sm break-words"
      dangerouslySetInnerHTML={{ __html: html }}
    />
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

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: finalContent,
              timestamp: new Date(),
            },
          ]);

          pendingContentRef.current = '';
          setTyping(false);
          break;
        }

        case 'tool_call':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Call]\` **${msg.name ?? 'unknown'}**\n\`\`\`json\n${JSON.stringify(msg.args ?? {}, null, 2)}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'tool_result':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Result]\`\n\`\`\`\n${msg.output ?? ''}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'error': {
          const errorText = msg.message ?? 'Unknown error';
          const isApiKeyError =
            msg.code === 'missing_api_key' || msg.code === 'provider_auth_error';
          const displayContent = isApiKeyError
            ? `**[API Key Error]** ${errorText}\n\nPlease configure your API key in Settings → Integrations.`
            : `**[Error]** ${errorText}`;

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: displayContent,
              timestamp: new Date(),
            },
          ]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }
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
        id: makeMessageId(),
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

  const handleNewChat = () => {
    if (!wsRef.current) return;
    setMessages([]);
    setTyping(false);
    setError(null);
    pendingContentRef.current = '';
    wsRef.current.resetSession();
    inputRef.current?.focus();
  };

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Chat header with New Chat button */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-900/80">
        <div className="flex items-center gap-2">
          <Bot className="h-5 w-5 text-gray-400" />
          <span className="text-sm font-medium text-gray-300">Agent Chat</span>
        </div>
        <button
          onClick={handleNewChat}
          className="inline-flex items-center gap-1.5 text-sm text-gray-400 hover:text-white px-3 py-1.5 rounded-lg hover:bg-gray-700/60 transition-colors"
          title="New Chat"
        >
          <SquarePen className="h-4 w-4" />
          <span>New Chat</span>
        </button>
      </div>

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
            className={`flex items-start gap-3 ${
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
            <div
              className={`max-w-[75%] rounded-xl px-4 py-3 ${
                msg.role === 'user'
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-800 text-gray-100 border border-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
              ) : (
                <MarkdownMessage content={msg.content} />
              )}
              <div className={`flex items-center justify-between mt-2 ${
                msg.role === 'user' ? '' : 'border-t border-gray-700/50 pt-1.5'
              }`}>
                <p
                  className={`text-xs ${
                    msg.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                  }`}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
                {msg.role === 'agent' && (
                  <CopyButton content={msg.content} />
                )}
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
