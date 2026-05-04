import { memo, useState, useEffect, useRef, useCallback } from 'react';
import { Send, Square, Bot, User, AlertCircle, Copy, Check, X, Trash2, Minimize2, Maximize2, ChevronDown } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useAgent, type ChatMessage } from '@/contexts/AgentContext';
import { useDraft } from '@/hooks/useDraft';
import { t } from '@/lib/i18n';

import ToolCallCard from '@/components/ToolCallCard';

const DRAFT_KEY = 'agent-chat';

export default function AgentChat() {
  const {
    messages,
    sendMessage,
    connected,
    error,
    typing,
    streamingContent,
    streamingThinking,
    currentModel,
    availableModels,
    switchModel,
    modelLoading,
    deleteMessage,
    clearAllMessages,
    abortSession,
  } = useAgent();

  const { draft, saveDraft, clearDraft } = useDraft(DRAFT_KEY);
  const [input, setInput] = useState(draft);
  const [showModelDropdown, setShowModelDropdown] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [compact, setCompact] = useState(() => {
    try { return localStorage.getItem('zeroclaw_chat_compact') === '1'; } catch { return false; }
  });

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const modelDropdownRef = useRef<HTMLDivElement>(null);

  // Persist draft to in-memory store so it survives route changes
  useEffect(() => {
    saveDraft(input);
  }, [input, saveDraft]);

  // Scroll to bottom on new messages / streaming
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing, streamingContent]);

  // Close model dropdown when clicking outside
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (modelDropdownRef.current && !modelDropdownRef.current.contains(e.target as Node)) {
        setShowModelDropdown(false);
      }
    }
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !connected) return;

    sendMessage(trimmed);
    setInput('');
    clearDraft();
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.focus();
    }
  };

  const isComposingRef = useRef(false);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing && !isComposingRef.current) {
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
    const onSuccess = () => {
      setCopiedId(msgId);
      setTimeout(() => setCopiedId((prev) => (prev === msgId ? null : prev)), 2000);
    };

    if (navigator.clipboard?.writeText) {
      navigator.clipboard.writeText(content).then(onSuccess).catch(() => {
        fallbackCopy(content) && onSuccess();
      });
    } else {
      fallbackCopy(content) && onSuccess();
    }
  }, []);

  const handleDeleteMessage = useCallback((msgId: string) => {
    deleteMessage(msgId);
  }, [deleteMessage]);

  const handleClearAll = useCallback(() => {
    clearAllMessages();
  }, [clearAllMessages]);

  // Stop button: POST /api/sessions/{id}/abort. The gateway cancels the
  // in-flight turn, the WS handler sends an `error` frame which our
  // onMessage handler already maps to typing=false.
  const handleAbort = useCallback(async () => {
    try {
      await abortSession();
    } catch {
      // Best-effort: surface nothing if the abort itself fails. The
      // user can retry, and any leaked typing state clears on the next
      // server frame.
    }
  }, [abortSession]);

  const toggleCompact = useCallback(() => {
    setCompact((prev) => {
      const next = !prev;
      try { localStorage.setItem('zeroclaw_chat_compact', next ? '1' : '0'); } catch { /* noop */ }
      return next;
    });
  }, []);

  /**
   * Fallback copy using a temporary textarea for HTTP contexts
   * where navigator.clipboard is unavailable.
   */
  function fallbackCopy(text: string): boolean {
    const textarea = document.createElement('textarea');
    textarea.value = text;
    textarea.style.position = 'fixed';
    textarea.style.opacity = '0';
    document.body.appendChild(textarea);
    textarea.select();
    try {
      document.execCommand('copy');
      return true;
    } catch {
      return false;
    } finally {
      document.body.removeChild(textarea);
    }
  }

  const handleModelSwitch = async (model: string) => {
    setShowModelDropdown(false);
    if (model === currentModel) return;
    try {
      await switchModel(model);
    } catch {
      // Error is already set by switchModel internally
    }
  };

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Header with model selector */}
      <div className="flex items-center justify-between px-4 py-2 border-b" style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}>
        <div className="flex items-center gap-2">
          <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>ZeroClaw Agent</span>
        </div>

        <div className="relative" ref={modelDropdownRef}>
          <button
            type="button"
            onClick={() => setShowModelDropdown((v) => !v)}
            disabled={modelLoading || typing || (availableModels.length === 0 && currentModel === null)}
            className="flex items-center gap-2 px-3 py-1.5 rounded-xl text-xs font-medium border transition-colors disabled:opacity-50"
            style={{
              background: 'var(--pc-bg-elevated)',
              borderColor: 'var(--pc-border)',
              color: 'var(--pc-text-secondary)',
            }}
            onMouseEnter={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
              e.currentTarget.style.color = 'var(--pc-text-primary)';
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-border)';
              e.currentTarget.style.color = 'var(--pc-text-secondary)';
            }}
          >
            <span className="max-w-[180px] truncate">
              {modelLoading
                ? t('agent.model_switching')
                : (currentModel ?? (availableModels.length === 0 ? t('agent.model_loading') : t('agent.select_model')))}
            </span>
            <ChevronDown className="h-3 w-3" />
          </button>

          {showModelDropdown && availableModels.length > 0 && (
            <div
              className="absolute right-0 mt-1.5 rounded-xl border shadow-lg z-50 py-1 min-w-[200px] max-h-60 overflow-y-auto"
              style={{
                background: 'var(--pc-bg-elevated)',
                borderColor: 'var(--pc-border)',
              }}
            >
              {availableModels.map((model) => (
                <button
                  key={model}
                  type="button"
                  onClick={() => handleModelSwitch(model)}
                  className="w-full text-left px-3 py-2 text-xs transition-colors"
                  style={{
                    color: model === currentModel ? 'var(--pc-accent)' : 'var(--pc-text-primary)',
                    background: model === currentModel ? 'var(--pc-accent-glow)' : 'transparent',
                  }}
                  onMouseEnter={(e) => {
                    if (model !== currentModel) {
                      e.currentTarget.style.background = 'var(--pc-bg-surface)';
                    }
                  }}
                  onMouseLeave={(e) => {
                    if (model !== currentModel) {
                      e.currentTarget.style.background = 'transparent';
                    }
                  }}
                >
                  {model}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 border-b flex items-center gap-2 text-sm animate-fade-in" style={{ background: 'var(--color-status-error-alpha-08)', borderColor: 'var(--color-status-error-alpha-20)', color: 'var(--color-status-error)' }}>
          <AlertCircle className="h-4 w-4 shrink-0" />
          {error}
        </div>
      )}

      {/* Chat toolbar */}
      {messages.length > 0 && (
        <div
          className="flex items-center justify-end gap-2 px-4 py-2 border-b"
          style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)' }}
        >
          <button
            type="button"
            onClick={toggleCompact}
            className="btn-secondary flex items-center gap-1.5 text-xs"
            style={{ padding: '0.3rem 0.75rem', borderRadius: '0.5rem' }}
            aria-label={t('agent.compact_mode')}
          >
            {compact ? <Maximize2 className="h-3 w-3" /> : <Minimize2 className="h-3 w-3" />}
            {t('agent.compact_mode')}
          </button>
          <button
            type="button"
            onClick={handleClearAll}
            className="btn-danger flex items-center gap-1.5 text-xs"
            style={{ padding: '0.3rem 0.75rem', borderRadius: '0.5rem' }}
            aria-label={t('agent.clear_all')}
          >
            <Trash2 className="h-3 w-3" />
            {t('agent.clear_all')}
          </button>
        </div>
      )}

      {/* Messages area */}
      <div className={`flex-1 overflow-y-auto p-4 ${compact ? 'space-y-1.5' : 'space-y-4'}`}>
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-center animate-fade-in" style={{ color: 'var(--pc-text-muted)' }}>
            <div className="h-16 w-16 rounded-3xl flex items-center justify-center mb-4 animate-float" style={{ background: 'var(--pc-accent-glow)' }}>
              <Bot className="h-8 w-8" style={{ color: 'var(--pc-accent)' }} />
            </div>
            <p className="text-lg font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>ZeroClaw Agent</p>
            <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>{t('agent.start_conversation')}</p>
          </div>
        )}

        {messages.map((msg, idx) => (
          <MessageItem
            key={msg.id}
            msg={msg}
            idx={idx}
            compact={compact}
            isCopied={copiedId === msg.id}
            onCopy={handleCopy}
            onDelete={handleDeleteMessage}
          />
        ))}

        {typing && (
          <div className="flex items-start gap-3 animate-fade-in">
            <div className="flex-shrink-0 w-9 h-9 rounded-2xl flex items-center justify-center border" style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}>
              <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
            </div>
            {streamingContent || streamingThinking ? (
              <div className="rounded-2xl px-4 py-3 border max-w-[75%]" style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }}>
                {streamingThinking && (
                  <details className="mb-2" open={!streamingContent}>
                    <summary className="text-xs cursor-pointer select-none" style={{ color: 'var(--pc-text-muted)' }}>Thinking{!streamingContent && '...'}</summary>
                    <pre className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-lg" style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-surface)' }}>{streamingThinking}</pre>
                  </details>
                )}
                {streamingContent && <p className="text-sm whitespace-pre-wrap break-words leading-relaxed">{streamingContent}</p>}
              </div>
            ) : (
              <div className="rounded-2xl px-4 py-3 border flex items-center gap-1.5" style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}>
                <span className="bounce-dot w-1.5 h-1.5 rounded-full" style={{ background: 'var(--pc-accent)' }} />
                <span className="bounce-dot w-1.5 h-1.5 rounded-full" style={{ background: 'var(--pc-accent)' }} />
                <span className="bounce-dot w-1.5 h-1.5 rounded-full" style={{ background: 'var(--pc-accent)' }} />
              </div>
            )}
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
            onCompositionStart={() => { isComposingRef.current = true; }}
            onCompositionEnd={() => { isComposingRef.current = false; }}
            placeholder={!connected
              ? t('agent.connecting')
              : typing
                ? t('agent.running')
                : t('agent.type_message')}
            disabled={!connected || typing}
            className="input-electric flex-1 px-4 text-sm resize-none disabled:opacity-40"
            style={{ minHeight: '44px', maxHeight: '200px', paddingTop: '10px', paddingBottom: '10px' }}
          />
          {typing ? (
            <button
              type="button"
              onClick={handleAbort}
              className="btn-danger flex-shrink-0 rounded-2xl flex items-center justify-center"
              style={{ color: 'white', width: '40px', height: '40px' }}
              aria-label={t('agent.stop')}
              title={t('agent.stop')}
            >
              <Square className="h-4 w-4" fill="currentColor" />
            </button>
          ) : (
            <button
              type='button'
              onClick={handleSend}
              disabled={!connected || !input.trim()}
              className="btn-electric flex-shrink-0 rounded-2xl flex items-center justify-center"
              style={{ color: 'white', width: '40px', height: '40px' }}
            >
              <Send className="h-5 w-5" />
            </button>
          )}
        </div>
        <div className="flex items-center justify-center mt-2 gap-2">
          <span
            className="status-dot"
            style={typing
              ? { background: 'var(--pc-accent)', boxShadow: '0 0 6px var(--pc-accent)' }
              : connected
                ? { background: 'var(--color-status-success)', boxShadow: '0 0 6px var(--color-status-success)' }
                : { background: 'var(--color-status-error)', boxShadow: '0 0 6px var(--color-status-error)' }
            }
          />
          <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
            {typing
              ? t('agent.running')
              : connected
                ? t('agent.connected_status')
                : t('agent.disconnected_status')}
          </span>
        </div>
      </div>
    </div>
  );
}

// Each chat message is rendered through this memoized component so that
// typing into the input does not re-render every existing message (and
// re-run ReactMarkdown on each one). Keep the prop surface small and pass
// `isCopied` rather than the parent's full copiedId so only the affected
// row re-renders when the copy indicator flips. See #5125.
interface MessageItemProps {
  msg: ChatMessage;
  idx: number;
  compact: boolean;
  isCopied: boolean;
  onCopy: (id: string, content: string) => void;
  onDelete: (id: string) => void;
}

const MessageItem = memo(function MessageItem({
  msg,
  idx,
  compact,
  isCopied,
  onCopy,
  onDelete,
}: MessageItemProps) {
  return (
    <div
      className={`group flex items-start ${compact ? 'gap-2' : 'gap-3'} ${
        msg.role === 'user' ? 'flex-row-reverse animate-slide-in-right' : 'animate-slide-in-left'
      }`}
      style={{ animationDelay: `${Math.min(idx * 30, 200)}ms` }}
    >
      {!compact && (
        <div
          className="flex-shrink-0 w-9 h-9 rounded-2xl flex items-center justify-center border"
          style={{
            background: msg.role === 'user' ? 'var(--pc-accent)' : 'var(--pc-bg-elevated)',
            borderColor: msg.role === 'user' ? 'var(--pc-accent)' : 'var(--pc-border)',
          }}
        >
          {msg.role === 'user' ? (
            <User className="h-4 w-4 text-white" />
          ) : (
            <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          )}
        </div>
      )}
      <div className="relative max-w-[75%]">
        <div
          className={compact ? 'rounded-xl px-3 py-1.5 border' : 'rounded-2xl px-4 py-3 border'}
          style={
            msg.role === 'user'
              ? { background: 'var(--pc-accent-glow)', borderColor: 'var(--pc-accent-dim)', color: 'var(--pc-text-primary)' }
              : { background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)', color: 'var(--pc-text-primary)' }
          }
        >
          {msg.thinking && (
            <details className="mb-2">
              <summary className="text-xs cursor-pointer select-none" style={{ color: 'var(--pc-text-muted)' }}>Thinking</summary>
              <pre className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-lg" style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-surface)' }}>{msg.thinking}</pre>
            </details>
          )}
          {msg.toolCall ? (
            <ToolCallCard toolCall={msg.toolCall} />
          ) : msg.markdown ? (
            <div className={`${compact ? 'text-xs' : 'text-sm'} break-words leading-relaxed chat-markdown`}><ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown></div>
          ) : (
            <p className={`${compact ? 'text-xs' : 'text-sm'} whitespace-pre-wrap break-words leading-relaxed`}>{msg.content}</p>
          )}
          {!compact && (
            <p
              className="text-[10px] mt-1.5" style={{ color: msg.role === 'user' ? 'var(--pc-accent-light)' : 'var(--pc-text-faint)' }}>
              {msg.timestamp.toLocaleTimeString()}
            </p>
          )}
        </div>
        <div className="flex items-center justify-end gap-1 mt-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <button
            onClick={() => onCopy(msg.id, msg.content)}
            aria-label={t('agent.copy_message')}
            className="p-1 rounded-lg"
            style={{ color: 'var(--pc-text-muted)' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = 'var(--pc-text-primary)'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; }}
          >
            {isCopied ? (
              <Check className="h-3.5 w-3.5" style={{ color: '#34d399' }} />
            ) : (
              <Copy className="h-3.5 w-3.5" />
            )}
          </button>
          <button
            onClick={() => onDelete(msg.id)}
            aria-label={t('agent.delete_message')}
            className="p-1 rounded-lg"
            style={{ color: 'var(--pc-text-muted)' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = '#f87171'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; }}
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
});
