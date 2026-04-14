import { useState, useEffect, useRef, useCallback } from 'react';
import { Send, Bot, User, AlertCircle, Copy, Check, ChevronDown, Cpu } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useAgent } from '@/contexts/AgentContext';
import ToolCallCard from '@/components/ToolCallCard';
import { t } from '@/lib/i18n';

export default function AgentChat() {
  const {
    connected,
    error,
    messages,
    typing,
    streamingContent,
    streamingThinking,
    sendMessage,
    currentModel,
    availableModels,
    switchModel,
    loadingModels,
  } = useAgent();

  const [input, setInput] = useState('');
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [showModelDropdown, setShowModelDropdown] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing, streamingContent]);

  // Focus input on mount
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // Close dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setShowModelDropdown(false);
      }
    };
    document.addEventListener('mousedown', handleClickOutside);
    return () => document.removeEventListener('mousedown', handleClickOutside);
  }, []);

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !connected) return;

    sendMessage(trimmed);
    setInput('');
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
    if (model === currentModel) {
      setShowModelDropdown(false);
      return;
    }
    await switchModel(model);
    setShowModelDropdown(false);
  };

  // Extract model display name
  const getModelDisplayName = (model: string) => {
    if (!model) return 'Select Model';
    // Remove provider prefix if exists (e.g., "openai/gpt-4" -> "gpt-4")
    const parts = model.split('/');
    return parts[parts.length - 1];
  };

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Header with Model Switcher */}
      <div
        className="flex items-center justify-between px-4 py-2 border-b"
        style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
      >
        <div className="flex items-center gap-2">
          <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
          <span className="text-sm font-medium" style={{ color: 'var(--pc-text-primary)' }}>
            ZeroClaw Agent
          </span>
          {connected && (
            <span
              className="status-dot ml-2"
              style={{
                background: 'var(--color-status-success)',
                boxShadow: '0 0 6px var(--color-status-success)',
              }}
            />
          )}
        </div>

        {/* Model Switcher Dropdown */}
        <div className="relative" ref={dropdownRef}>
          <button
            onClick={() => setShowModelDropdown(!showModelDropdown)}
            disabled={loadingModels || !connected}
            className="flex items-center gap-2 px-3 py-1.5 rounded-xl border text-xs font-medium transition-all"
            style={{
              borderColor: 'var(--pc-border)',
              background: 'var(--pc-bg-elevated)',
              color: 'var(--pc-text-primary)',
              opacity: loadingModels || !connected ? 0.5 : 1,
            }}
            onMouseEnter={(e) => {
              if (!loadingModels && connected) {
                e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
                e.currentTarget.style.background = 'var(--pc-hover)';
              }
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.borderColor = 'var(--pc-border)';
              e.currentTarget.style.background = 'var(--pc-bg-elevated)';
            }}
          >
            <Cpu className="h-3 w-3" style={{ color: 'var(--pc-accent)' }} />
            <span className="max-w-[120px] truncate">
              {loadingModels ? 'Loading...' : getModelDisplayName(currentModel)}
            </span>
            <ChevronDown
              className="h-3 w-3 transition-transform"
              style={{
                color: 'var(--pc-text-muted)',
                transform: showModelDropdown ? 'rotate(180deg)' : 'rotate(0deg)',
              }}
            />
          </button>

          {/* Dropdown Menu */}
          {showModelDropdown && availableModels.length > 0 && (
            <div
              className="absolute right-0 top-full mt-1 w-56 rounded-xl border shadow-lg z-50 animate-fade-in"
              style={{
                background: 'var(--pc-bg-elevated)',
                borderColor: 'var(--pc-border)',
                maxHeight: '300px',
                overflowY: 'auto',
              }}
            >
              <div
                className="px-3 py-2 text-[10px] uppercase tracking-wider"
                style={{ color: 'var(--pc-text-faint)' }}
              >
                Available Models
              </div>
              {availableModels.map((model) => {
                const isActive = model === currentModel;
                return (
                  <button
                    key={model}
                    onClick={() => handleModelSwitch(model)}
                    className="w-full px-3 py-2 text-left text-xs transition-colors flex items-center gap-2"
                    style={{
                      background: isActive ? 'var(--pc-accent-glow)' : 'transparent',
                      color: isActive ? 'var(--pc-accent-light)' : 'var(--pc-text-primary)',
                    }}
                    onMouseEnter={(e) => {
                      if (!isActive) {
                        e.currentTarget.style.background = 'var(--pc-hover)';
                      }
                    }}
                    onMouseLeave={(e) => {
                      if (!isActive) {
                        e.currentTarget.style.background = 'transparent';
                      }
                    }}
                  >
                    {isActive && (
                      <Check className="h-3 w-3" style={{ color: 'var(--pc-accent)' }} />
                    )}
                    <span className={isActive ? 'ml-0' : 'ml-5'}>{getModelDisplayName(model)}</span>
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* Connection status bar */}
      {error && (
        <div
          className="px-4 py-2 border-b flex items-center gap-2 text-sm animate-fade-in"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          <AlertCircle className="h-4 w-4 shrink-0" />
          {error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div
            className="flex flex-col items-center justify-center h-full text-center animate-fade-in"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            <div
              className="h-16 w-16 rounded-3xl flex items-center justify-center mb-4 animate-float"
              style={{ background: 'var(--pc-accent-glow)' }}
            >
              <Bot className="h-8 w-8" style={{ color: 'var(--pc-accent)' }} />
            </div>
            <p className="text-lg font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
              ZeroClaw Agent
            </p>
            <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
              {t('agent.start_conversation')}
            </p>
            {currentModel && (
              <p className="text-xs mt-2" style={{ color: 'var(--pc-text-faint)' }}>
                Current model: {getModelDisplayName(currentModel)}
              </p>
            )}
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
            <div className="relative max-w-[75%]">
              <div
                className="rounded-2xl px-4 py-3 border"
                style={
                  msg.role === 'user'
                    ? {
                        background: 'var(--pc-accent-glow)',
                        borderColor: 'var(--pc-accent-dim)',
                        color: 'var(--pc-text-primary)',
                      }
                    : {
                        background: 'var(--pc-bg-elevated)',
                        borderColor: 'var(--pc-border)',
                        color: 'var(--pc-text-primary)',
                      }
                }
              >
                {msg.thinking && (
                  <details className="mb-2">
                    <summary
                      className="text-xs cursor-pointer select-none"
                      style={{ color: 'var(--pc-text-muted)' }}
                    >
                      Thinking
                    </summary>
                    <pre
                      className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-lg"
                      style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-surface)' }}
                    >
                      {msg.thinking}
                    </pre>
                  </details>
                )}
                {msg.toolCall ? (
                  <ToolCallCard toolCall={msg.toolCall} />
                ) : msg.markdown ? (
                  <div className="text-sm break-words leading-relaxed chat-markdown">
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{msg.content}</ReactMarkdown>
                  </div>
                ) : (
                  <p className="text-sm whitespace-pre-wrap break-words leading-relaxed">{msg.content}</p>
                )}
                <p
                  className="text-[10px] mt-1.5"
                  style={{ color: msg.role === 'user' ? 'var(--pc-accent-light)' : 'var(--pc-text-faint)' }}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
              </div>
              <button
                onClick={() => handleCopy(msg.id, msg.content)}
                aria-label={t('agent.copy_message')}
                className="absolute top-1 right-1 opacity-0 group-hover:opacity-100 transition-all p-1.5 rounded-xl"
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

        {typing && (
          <div className="flex items-start gap-3 animate-fade-in">
            <div
              className="flex-shrink-0 w-9 h-9 rounded-2xl flex items-center justify-center border"
              style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}
            >
              <Bot className="h-4 w-4" style={{ color: 'var(--pc-accent)' }} />
            </div>
            {streamingContent || streamingThinking ? (
              <div
                className="rounded-2xl px-4 py-3 border max-w-[75%]"
                style={{
                  background: 'var(--pc-bg-elevated)',
                  borderColor: 'var(--pc-border)',
                  color: 'var(--pc-text-primary)',
                }}
              >
                {streamingThinking && (
                  <details className="mb-2" open={!streamingContent}>
                    <summary
                      className="text-xs cursor-pointer select-none"
                      style={{ color: 'var(--pc-text-muted)' }}
                    >
                      Thinking{!streamingContent && '...'}
                    </summary>
                    <pre
                      className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-lg"
                      style={{ color: 'var(--pc-text-muted)', background: 'var(--pc-bg-surface)' }}
                    >
                      {streamingThinking}
                    </pre>
                  </details>
                )}
                {streamingContent && (
                  <p className="text-sm whitespace-pre-wrap break-words leading-relaxed">{streamingContent}</p>
                )}
              </div>
            ) : (
              <div
                className="rounded-2xl px-4 py-3 border flex items-center gap-1.5"
                style={{ background: 'var(--pc-bg-elevated)', borderColor: 'var(--pc-border)' }}
              >
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
            placeholder={connected ? t('agent.type_message') : t('agent.connecting')}
            disabled={!connected}
            className="input-electric flex-1 px-4 text-sm resize-none disabled:opacity-40"
            style={{ minHeight: '44px', maxHeight: '200px', paddingTop: '10px', paddingBottom: '10px' }}
          />
          <button
            type="button"
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="btn-electric flex-shrink-0 rounded-2xl flex items-center justify-center"
            style={{ color: 'white', width: '40px', height: '40px' }}
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="flex items-center justify-center mt-2 gap-2">
          <span
            className="status-dot"
            style={
              connected
                ? { background: 'var(--color-status-success)', boxShadow: '0 0 6px var(--color-status-success)' }
                : { background: 'var(--color-status-error)', boxShadow: '0 0 6px var(--color-status-error)' }
            }
          />
          <span className="text-[10px]" style={{ color: 'var(--pc-text-faint)' }}>
            {connected ? t('agent.connected_status') : t('agent.disconnected_status')}
          </span>
        </div>
      </div>
    </div>
  );
}
