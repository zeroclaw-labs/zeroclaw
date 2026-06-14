import { memo, useState, useEffect, useRef, useCallback } from 'react';
import { Navigate, useParams } from 'react-router-dom';
import { Send, Square, Bot, User, AlertCircle, Copy, Check, X, Trash2, Minimize2, Maximize2, ChevronDown, Wrench } from 'lucide-react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { useAgent, type ChatMessage } from '@/contexts/AgentContext';
import { useDraft } from '@/hooks/useDraft';
import { t } from '@/lib/i18n';
import { Badge, Button } from '@/components/ui';
import ChatWorkspace from '@/pages/ChatWorkspace';

import ToolCallCard from '@/components/ToolCallCard';
import ApprovalBanner from '@/components/ApprovalBanner';

const DRAFT_KEY_PREFIX = 'agent-chat';

/**
 * Route entry point for `/agent/:alias`. Reads the alias from the URL and
 * hands it to the multi-agent ChatWorkspace as the initial chat to open and
 * activate. The workspace itself owns the set of open chats and never
 * remounts on tab/layout switches, so the alias is passed as a prop (not used
 * as a React `key`) — that keeps every chat's AgentProvider WebSocket alive
 * across tab switches. Missing alias → redirect to the agents list.
 */
export default function AgentChat() {
  const { alias } = useParams<{ alias: string }>();
  if (!alias) {
    return <Navigate to="/agents" replace />;
  }
  return <ChatWorkspace initialAlias={alias} />;
}

/** Status snapshot a chat pane pushes up to the workspace tab bar. */
export interface AgentChatStatus {
  typing: boolean;
  messageCount: number;
}

/**
 * Full chat view for a single agent. Must be rendered inside an
 * `<AgentProvider>` (it calls `useAgent()` internally). Exported so the
 * multi-agent `ChatWorkspace` can mount one instance per open chat and keep
 * them all alive simultaneously.
 *
 * `onStatus` lets the host (the workspace) observe live typing / message-count
 * changes per pane without itself subscribing to the agent context — used to
 * drive the streaming and unread indicators in the tab bar.
 */
export function AgentChatInner({
  agentAlias,
  onStatus,
}: {
  agentAlias: string;
  onStatus?: (s: AgentChatStatus) => void;
}) {
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
    pendingApproval,
    respondToApproval,
  } = useAgent();

  const { draft, saveDraft, clearDraft } = useDraft(`${DRAFT_KEY_PREFIX}.${agentAlias}`);
  const [input, setInput] = useState(draft);
  const [showModelDropdown, setShowModelDropdown] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const [compact, setCompact] = useState(() => {
    try { return localStorage.getItem('zeroclaw_chat_compact') === '1'; } catch { return false; }
  });
  // Tool execution is plumbing, not chat. Default off so tool_call /
  // tool_result frames do not surface inline in the conversation transcript.
  // Toggleable from the chat toolbar (Wrench button). The WebSocket lives in
  // AgentContext, which always pushes tool cards into messages; this toggle
  // filters them at render time so toggling on retroactively reveals prior
  // tool activity.
  const [showToolActivity, setShowToolActivity] = useState(() => {
    try { return localStorage.getItem('zeroclaw_show_tool_activity') === '1'; } catch { return false; }
  });

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const modelDropdownRef = useRef<HTMLDivElement>(null);

  // Persist draft to in-memory store so it survives route changes
  useEffect(() => {
    saveDraft(input);
  }, [input, saveDraft]);

  // Report live status (typing + message count) up to the host workspace so it
  // can render streaming / unread indicators in the tab bar. Fires on every
  // typing flip or message-count change; the workspace decides what to do with
  // it (e.g. mark a hidden tab unread when its count grows).
  useEffect(() => {
    onStatus?.({ typing, messageCount: messages.length });
  }, [typing, messages.length, onStatus]);

  // Scroll to bottom on new messages / streaming.
  // Note: WebSocket lifecycle, hydration, and tool_call/tool_result handling
  // moved to AgentContext (PR #6101). Tool activity is filtered at render
  // time below using `showToolActivity`, not at the message-handler layer.
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

  const toggleToolActivity = useCallback(() => {
    setShowToolActivity((prev) => {
      const next = !prev;
      try { localStorage.setItem('zeroclaw_show_tool_activity', next ? '1' : '0'); } catch { /* noop */ }
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
    /* translate="no" / notranslate (#7057): browser auto-translation (e.g.
       Chrome → Google Translate) rewrites text nodes into <font> wrappers.
       React reconciliation then trips "Failed to execute 'removeChild' on
       'Node'" and unmounts the view. The crash repro surface spans every
       dynamic-text region on this page: streaming output, ReactMarkdown
       message bodies, the {error} banner above the toolbar, and
       ApprovalBanner (whose <pre>{argumentsSummary}</pre> and per-second
       remainingSec re-render are at least as crash-prone as streaming).
       Hoisting the opt-out to the outermost container covers all of them
       with a single ancestor. Static UI chrome here localizes through
       t() i18n, so losing browser translation on it is intentional. */
    <div translate="no" className="notranslate flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Header with model selector */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-pc-border bg-pc-surface">
        <div className="flex items-center gap-2">
          <Bot className="h-4 w-4 text-pc-accent" />
          <span className="text-sm font-medium text-pc-text">{agentAlias}</span>
        </div>

        <div className="relative" ref={modelDropdownRef}>
          <button
            type="button"
            onClick={() => setShowModelDropdown((v) => !v)}
            disabled={modelLoading || typing || (availableModels.length === 0 && currentModel === null)}
            className="flex items-center gap-2 px-3 h-7 rounded-[var(--radius-md)] text-xs font-medium border border-pc-border bg-pc-elevated text-pc-text-secondary transition-colors hover:text-pc-text hover:border-pc-border-strong disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
          >
            <span className="max-w-[180px] truncate">
              {modelLoading
                ? t('agent.model_switching')
                : (currentModel ?? (availableModels.length === 0 ? t('agent.model_loading') : t('agent.select_model')))}
            </span>
            <ChevronDown className="h-3 w-3" />
          </button>

          {showModelDropdown && availableModels.length > 0 && (
            <div className="absolute right-0 mt-1.5 rounded-[var(--radius-md)] border border-pc-border bg-pc-elevated shadow-[var(--pc-shadow-md)] z-50 py-1 min-w-[200px] max-h-60 overflow-y-auto">
              {availableModels.map((model) => {
                const isActive = model === currentModel;
                return (
                  <button
                    key={model}
                    type="button"
                    onClick={() => handleModelSwitch(model)}
                    className={`w-full text-left px-3 py-2 text-xs transition-colors ${
                      isActive
                        ? 'text-pc-accent bg-pc-accent/10'
                        : 'text-pc-text hover:bg-[var(--pc-hover)]'
                    }`}
                  >
                    {model}
                  </button>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 border-b border-status-error/20 bg-status-error/10 text-status-error flex items-center gap-2 text-sm animate-fade-in">
          <AlertCircle className="h-4 w-4 shrink-0" />
          {error}
        </div>
      )}

      {/* Chat toolbar */}
      {messages.length > 0 && (
        <div className="flex items-center justify-end gap-2 px-4 py-2 border-b border-pc-border bg-pc-surface">
          <Button
            variant="ghost"
            size="sm"
            onClick={toggleCompact}
            aria-label={t('agent.compact_mode')}
          >
            {compact ? <Maximize2 className="h-3 w-3" /> : <Minimize2 className="h-3 w-3" />}
            {t('agent.compact_mode')}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={toggleToolActivity}
            aria-label={showToolActivity ? t('agent.tool_activity_hide') : t('agent.tool_activity_show')}
            aria-pressed={showToolActivity}
          >
            <Wrench className="h-3 w-3" />
            {showToolActivity ? t('agent.tool_activity_hide') : t('agent.tool_activity_show')}
          </Button>
          <Button
            variant="danger"
            size="sm"
            onClick={handleClearAll}
            aria-label={t('agent.clear_all')}
          >
            <Trash2 className="h-3 w-3" />
            {t('agent.clear_all')}
          </Button>
        </div>
      )}

      {/* Messages area. */}
      <div
        className={`flex-1 overflow-y-auto p-4 ${compact ? 'space-y-1.5' : 'space-y-4'}`}
      >
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-center animate-fade-in text-pc-text-muted">
            <div className="h-14 w-14 rounded-[var(--radius-lg)] flex items-center justify-center mb-4 bg-pc-accent/10">
              <Bot className="h-7 w-7 text-pc-accent" />
            </div>
            <p className="text-base font-semibold mb-1 text-pc-text">ZeroClaw Agent</p>
            <p className="text-sm text-pc-text-muted">{t('agent.start_conversation')}</p>
          </div>
        )}

        {messages
          .filter((msg) => showToolActivity || !msg.toolCall)
          .map((msg, idx) => (
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
            <div className="flex-shrink-0 w-8 h-8 rounded-[var(--radius-md)] flex items-center justify-center border border-pc-border bg-pc-elevated">
              <Bot className="h-4 w-4 text-pc-accent" />
            </div>
            {streamingContent || streamingThinking ? (
              <div className="rounded-[var(--radius-lg)] px-4 py-3 border border-pc-border bg-pc-elevated text-pc-text max-w-[75%]">
                {streamingThinking && (
                  <details className="mb-2" open={!streamingContent}>
                    <summary className="text-xs cursor-pointer select-none text-pc-text-muted">Thinking{!streamingContent && '...'}</summary>
                    <pre className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-[var(--radius-sm)] text-pc-text-muted bg-pc-code">{streamingThinking}</pre>
                  </details>
                )}
                {streamingContent && <p className="text-sm whitespace-pre-wrap break-words leading-relaxed">{streamingContent}</p>}
              </div>
            ) : (
              <div className="rounded-[var(--radius-lg)] px-4 py-3 border border-pc-border bg-pc-elevated flex items-center gap-1.5">
                <span className="bounce-dot w-1.5 h-1.5 rounded-full bg-pc-accent" />
                <span className="bounce-dot w-1.5 h-1.5 rounded-full bg-pc-accent" />
                <span className="bounce-dot w-1.5 h-1.5 rounded-full bg-pc-accent" />
              </div>
            )}
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Tool approval banner — supervised-mode consent prompt (#6522). */}
      {pendingApproval && (
        <ApprovalBanner pending={pendingApproval} onRespond={respondToApproval} />
      )}

      {/* Input area */}
      <div className="border-t border-pc-border bg-pc-surface p-4">
        <div className="flex items-end gap-3 max-w-4xl mx-auto">
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
            className="flex-1 px-4 text-sm resize-none rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-muted transition-colors focus:outline-none focus:border-pc-accent focus:ring-2 focus:ring-pc-accent/30 disabled:opacity-40"
            style={{ minHeight: '40px', maxHeight: '200px', paddingTop: '9px', paddingBottom: '9px' }}
          />
          {typing ? (
            <Button
              variant="danger"
              size="md"
              onClick={handleAbort}
              className="flex-shrink-0 w-10 px-0"
              aria-label={t('agent.stop')}
              title={t('agent.stop')}
            >
              <Square className="h-4 w-4" fill="currentColor" />
            </Button>
          ) : (
            <Button
              variant="primary"
              size="md"
              onClick={handleSend}
              disabled={!connected || !input.trim()}
              className="flex-shrink-0 w-10 px-0"
              aria-label={t('agent.send')}
            >
              <Send className="h-4 w-4" />
            </Button>
          )}
        </div>
        <div className="flex items-center justify-center mt-2">
          <Badge tone={typing ? 'warn' : connected ? 'ok' : 'error'}>
            {typing
              ? t('agent.running')
              : connected
                ? t('agent.connected_status')
                : t('agent.disconnected_status')}
          </Badge>
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
          className={`flex-shrink-0 w-8 h-8 rounded-[var(--radius-md)] flex items-center justify-center border ${
            msg.role === 'user'
              ? 'bg-pc-accent/15 border-pc-accent/30'
              : 'bg-pc-elevated border-pc-border'
          }`}
        >
          {msg.role === 'user' ? (
            <User className="h-4 w-4 text-pc-accent" />
          ) : (
            <Bot className="h-4 w-4 text-pc-accent" />
          )}
        </div>
      )}
      <div className="relative max-w-[75%]">
        <div
          className={`${compact ? 'rounded-[var(--radius-md)] px-3 py-1.5 border' : 'rounded-[var(--radius-lg)] px-4 py-3 border'} text-pc-text ${
            msg.role === 'user'
              ? 'bg-pc-accent/10 border-pc-accent/20'
              : 'bg-pc-elevated border-pc-border'
          }`}
        >
          {msg.thinking && (
            <details className="mb-2">
              <summary className="text-xs cursor-pointer select-none text-pc-text-muted">Thinking</summary>
              <pre className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed overflow-auto max-h-60 p-2 rounded-[var(--radius-sm)] text-pc-text-muted bg-pc-code">{msg.thinking}</pre>
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
            <p className="text-[10px] mt-1.5 text-pc-text-faint">
              {msg.timestamp.toLocaleTimeString()}
            </p>
          )}
        </div>
        <div className="flex items-center justify-end gap-1 mt-1 opacity-0 group-hover:opacity-100 transition-opacity">
          <button
            onClick={() => onCopy(msg.id, msg.content)}
            aria-label={t('agent.copy_message')}
            className="p-1 rounded-[var(--radius-sm)] text-pc-text-muted hover:text-pc-text transition-colors"
          >
            {isCopied ? (
              <Check className="h-3.5 w-3.5 text-status-success" />
            ) : (
              <Copy className="h-3.5 w-3.5" />
            )}
          </button>
          <button
            onClick={() => onDelete(msg.id)}
            aria-label={t('agent.delete_message')}
            className="p-1 rounded-[var(--radius-sm)] text-pc-text-muted hover:text-status-error transition-colors"
          >
            <X className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>
    </div>
  );
});
