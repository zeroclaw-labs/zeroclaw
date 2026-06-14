import { useCallback, useEffect, useMemo, useRef, useState, type Dispatch, type SetStateAction } from 'react';
import {
  AlertCircle,
  Check,
  Plug,
  RefreshCw,
  Send,
  ShieldCheck,
  Square,
  Terminal,
  X,
} from 'lucide-react';
import { Badge, Button, Card, PageHeader, type BadgeTone } from '@/components/ui';
import { loadAgentPickerSummaries, type AgentPickerSummary } from '@/lib/agents';
import {
  AcpWebSocketClient,
  type AcpConnectionStatus,
  type AcpFrame,
  type AcpInitializeResult,
  type AcpNotification,
  type AcpPermissionOption,
  type AcpRequest,
  type AcpSessionNewResult,
  type AcpSessionPromptResult,
  type AcpSessionUpdateParams,
  type JsonRpcId,
} from '@/lib/acp';
import { t } from '@/lib/i18n';

type ConsoleMessageKind = 'user' | 'assistant' | 'thought' | 'tool' | 'system';

interface ConsoleMessage {
  id: string;
  kind: ConsoleMessageKind;
  title?: string;
  content: string;
  detail?: string;
  timestamp: string;
}

interface PermissionRequest {
  id: JsonRpcId;
  sessionId?: string;
  title: string;
  detail: string;
  options: AcpPermissionOption[];
}

const DEFAULT_PROMPT = 'Summarize the current ZeroClaw gateway state in one paragraph.';
const MAX_DETAIL_CHARS = 8_000;

function nowLabel(): string {
  return new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

function messageId(): string {
  if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
    return crypto.randomUUID();
  }
  return `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function stringifyDetail(value: unknown, maxChars = MAX_DETAIL_CHARS): string {
  if (value === undefined || value === null) return '';
  const raw = typeof value === 'string'
    ? value
    : (() => {
        try {
          return JSON.stringify(value, null, 2);
        } catch {
          return String(value);
        }
      })();

  if (raw.length <= maxChars) return raw;
  return `${raw.slice(0, maxChars)}\n\n[truncated ${raw.length - maxChars} characters]`;
}

function textContent(value: unknown): string {
  if (typeof value === 'string') return value;
  if (!value || typeof value !== 'object') return '';
  const record = value as Record<string, unknown>;
  if (typeof record.text === 'string') return record.text;
  if (typeof record.content === 'string') return record.content;
  if (record.content && typeof record.content === 'object') {
    return textContent(record.content);
  }
  return '';
}

function getUpdateText(update: Record<string, unknown>): string {
  const direct = textContent(update.content);
  if (direct) return direct;
  return textContent(update.rawOutput);
}

function getToolTitle(update: Record<string, unknown>): string {
  if (typeof update.title === 'string') return update.title;
  if (typeof update.name === 'string') return update.name;
  if (typeof update.kind === 'string') return update.kind;
  return 'Tool call';
}

function frameLabel(frame: AcpFrame): string {
  if ('method' in frame) return frame.method;
  if ('error' in frame && frame.error) return `error:${frame.id}`;
  return `response:${frame.id}`;
}

function addMessage(
  setMessages: Dispatch<SetStateAction<ConsoleMessage[]>>,
  message: Omit<ConsoleMessage, 'id' | 'timestamp'>,
): void {
  setMessages((current) => [
    ...current,
    {
      ...message,
      id: messageId(),
      timestamp: nowLabel(),
    },
  ]);
}

export default function AcpConsole() {
  const clientRef = useRef<AcpWebSocketClient | null>(null);
  const connectionSeqRef = useRef(0);
  const sessionIdRef = useRef<string | null>(null);
  const streamBufferRef = useRef('');
  const [status, setStatus] = useState<AcpConnectionStatus>('disconnected');
  const [initializing, setInitializing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [cancelRequested, setCancelRequested] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [workspaceDir, setWorkspaceDir] = useState<string | null>(null);
  const [initResult, setInitResult] = useState<AcpInitializeResult | null>(null);
  const [agents, setAgents] = useState<AgentPickerSummary[]>([]);
  const [agentsLoading, setAgentsLoading] = useState(true);
  const [selectedAgentAlias, setSelectedAgentAlias] = useState<string | null>(null);
  const [prompt, setPrompt] = useState(DEFAULT_PROMPT);
  const [streamingText, setStreamingText] = useState('');
  const [messages, setMessages] = useState<ConsoleMessage[]>([]);
  const [permissions, setPermissions] = useState<PermissionRequest[]>([]);
  const [events, setEvents] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const hasEnabledAgent = agents.some((agent) => agent.enabled);

  useEffect(() => {
    sessionIdRef.current = sessionId;
  }, [sessionId]);

  const pushEvent = useCallback((text: string) => {
    setEvents((current) => [`${nowLabel()} ${text}`, ...current].slice(0, 80));
  }, []);

  useEffect(() => {
    let cancelled = false;

    setAgentsLoading(true);
    loadAgentPickerSummaries()
      .then((loadedAgents) => {
        if (cancelled) return;
        setAgents(loadedAgents);
        const preferred = loadedAgents.find((agent) => agent.enabled);
        setSelectedAgentAlias((current) => {
          if (current && loadedAgents.some((agent) => agent.alias === current && agent.enabled)) {
            return current;
          }
          return null;
        });
        if (loadedAgents.length === 0) {
          setError(t('acp.error.no_agents'));
        } else if (!preferred) {
          setError(t('acp.error.no_enabled_agents'));
        }
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : t('acp.error.load_agents'));
      })
      .finally(() => {
        if (!cancelled) setAgentsLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, []);

  const appendAssistantChunk = useCallback((text: string) => {
    if (!text) return;
    streamBufferRef.current += text;
    setStreamingText(streamBufferRef.current);
  }, []);

  const resetTurnStream = useCallback(() => {
    streamBufferRef.current = '';
    setStreamingText('');
  }, []);

  const handleSessionUpdate = useCallback((params: unknown) => {
    if (!params || typeof params !== 'object') return;
    const updateParams = params as AcpSessionUpdateParams;
    if (
      updateParams.sessionId
      && sessionIdRef.current
      && updateParams.sessionId !== sessionIdRef.current
    ) {
      return;
    }
    const update = updateParams.update;
    if (!update || typeof update !== 'object') return;

    const updateKind = update.sessionUpdate;
    if (updateKind === 'agent_message_chunk') {
      appendAssistantChunk(getUpdateText(update));
      return;
    }

    if (updateKind === 'agent_thought_chunk') {
      const thought = getUpdateText(update);
      if (thought) {
        addMessage(setMessages, {
          kind: 'thought',
          title: 'Agent thought',
          content: thought,
        });
      }
      return;
    }

    if (updateKind === 'tool_call' || updateKind === 'tool_call_update') {
      addMessage(setMessages, {
        kind: 'tool',
        title: getToolTitle(update),
        content: updateKind === 'tool_call' ? 'Started' : 'Finished',
        detail: stringifyDetail(update),
      });
      return;
    }

    addMessage(setMessages, {
      kind: 'system',
      title: 'Session update',
      content: typeof updateKind === 'string' ? updateKind : 'Unknown update',
      detail: stringifyDetail(update),
    });
  }, [appendAssistantChunk]);

  const handlePermissionRequest = useCallback((request: AcpRequest) => {
    if (!request.params || typeof request.params !== 'object') return;
    const params = request.params as Record<string, unknown>;
    const requestSessionId = typeof params.sessionId === 'string' ? params.sessionId : undefined;
    if (requestSessionId && sessionIdRef.current && requestSessionId !== sessionIdRef.current) return;
    const toolCall = params.toolCall;
    const toolRecord = toolCall && typeof toolCall === 'object'
      ? toolCall as Record<string, unknown>
      : {};
    const rawOptions = Array.isArray(params.options) ? params.options : [];
    const options = rawOptions
      .filter((option): option is Record<string, unknown> => Boolean(option) && typeof option === 'object')
      .map((option) => ({
        optionId: typeof option.optionId === 'string' ? option.optionId : '',
        name: typeof option.name === 'string' ? option.name : undefined,
        kind: typeof option.kind === 'string' ? option.kind : undefined,
      }))
      .filter((option) => option.optionId.length > 0);

    const permission: PermissionRequest = {
      id: request.id,
      sessionId: requestSessionId,
      title: getToolTitle(toolRecord),
      detail: stringifyDetail(toolRecord.rawInput ?? toolRecord.content ?? toolRecord),
      options,
    };
    setPermissions((current) => [...current, permission]);
    pushEvent(`permission requested: ${permission.title}`);
  }, [pushEvent]);

  const handleNotification = useCallback((notification: AcpNotification) => {
    if (notification.method === 'session/update') {
      handleSessionUpdate(notification.params);
    } else {
      pushEvent(`notification ${notification.method}`);
    }
  }, [handleSessionUpdate, pushEvent]);

  const handleRequest = useCallback((request: AcpRequest) => {
    if (request.method === 'session/request_permission') {
      handlePermissionRequest(request);
    } else {
      try {
        clientRef.current?.respondError(request.id, {
          code: -32601,
          message: `Unsupported ACP request: ${request.method}`,
        });
      } catch {
        // The socket may close between receiving a server request and
        // returning this protocol error; close/error handlers update state.
      }
      pushEvent(`request ${request.method}`);
    }
  }, [handlePermissionRequest, pushEvent]);

  const handleFrame = useCallback((frame: AcpFrame) => {
    pushEvent(frameLabel(frame));
  }, [pushEvent]);

  const isCurrentConnection = useCallback((client: AcpWebSocketClient, connectionSeq: number) => (
    clientRef.current === client && connectionSeqRef.current === connectionSeq
  ), []);

  const initializeSession = useCallback(async (
    client: AcpWebSocketClient,
    connectionSeq: number,
    agentAlias: string | null,
  ) => {
    if (!client.connected || !isCurrentConnection(client, connectionSeq)) return;

    setInitializing(true);
    setError(null);
    try {
      const init = await client.request<AcpInitializeResult>('initialize');
      if (!isCurrentConnection(client, connectionSeq)) return;
      setInitResult(init);
      const sessionParams = agentAlias ? { agentAlias } : undefined;
      const session = await client.request<AcpSessionNewResult>('session/new', sessionParams);
      if (!isCurrentConnection(client, connectionSeq)) return;
      if (session.sessionId) setSessionId(session.sessionId);
      if (session.workspaceDir) setWorkspaceDir(session.workspaceDir);
      pushEvent(`session/new complete: ${agentAlias ?? 'server default'}`);
    } catch (err) {
      if (isCurrentConnection(client, connectionSeq)) {
        setError(err instanceof Error ? err.message : t('acp.error.init_failed'));
      }
    } finally {
      if (isCurrentConnection(client, connectionSeq)) {
        setInitializing(false);
      }
    }
  }, [isCurrentConnection, pushEvent]);

  const connect = useCallback(() => {
    if (!hasEnabledAgent) return;
    clientRef.current?.disconnect();
    const connectionSeq = connectionSeqRef.current + 1;
    connectionSeqRef.current = connectionSeq;
    setStatus('connecting');
    setError(null);
    setPermissions([]);
    setSessionId(null);
    setWorkspaceDir(null);
    setInitResult(null);
    setBusy(false);
    setCancelRequested(false);
    setMessages([]);
    setEvents([]);
    resetTurnStream();
    const agentAlias = selectedAgentAlias;
    let client: AcpWebSocketClient;
    client = new AcpWebSocketClient({
      onOpen: () => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        setStatus('connected');
        pushEvent('connected');
        void initializeSession(client, connectionSeq, agentAlias);
      },
      onClose: () => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        setStatus('disconnected');
        setBusy(false);
        setCancelRequested(false);
        pushEvent('disconnected');
      },
      onError: () => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        setStatus('disconnected');
        setError(t('acp.error.websocket'));
      },
      onNotification: (notification) => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        handleNotification(notification);
      },
      onRequest: (request) => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        handleRequest(request);
      },
      onFrame: (frame) => {
        if (!isCurrentConnection(client, connectionSeq)) return;
        handleFrame(frame);
      },
    });
    clientRef.current = client;
    client.connect();
  }, [
    handleFrame,
    handleNotification,
    handleRequest,
    hasEnabledAgent,
    initializeSession,
    isCurrentConnection,
    pushEvent,
    resetTurnStream,
    selectedAgentAlias,
  ]);

  useEffect(() => {
    if (!hasEnabledAgent) {
      clientRef.current?.disconnect();
      return;
    }
    connect();
    return () => {
      clientRef.current?.disconnect();
    };
  }, [connect, hasEnabledAgent]);

  const canSend = status === 'connected'
    && hasEnabledAgent
    && Boolean(sessionId)
    && !busy
    && prompt.trim().length > 0;
  const agentLabel = useMemo(() => {
    const info = initResult?.agentInfo;
    return info?.title ?? info?.name ?? 'ZeroClaw ACP';
  }, [initResult]);

  const sendPrompt = async () => {
    const client = clientRef.current;
    if (!client?.connected || !sessionId || !prompt.trim()) return;
    const connectionSeq = connectionSeqRef.current;

    const outgoing = prompt.trim();
    addMessage(setMessages, { kind: 'user', content: outgoing });
    setPrompt('');
    resetTurnStream();
    setBusy(true);
    setCancelRequested(false);
    setError(null);

    try {
      const result = await client.request<AcpSessionPromptResult>('session/prompt', {
        sessionId,
        prompt: outgoing,
      });
      if (!isCurrentConnection(client, connectionSeq)) return;
      const streamed = streamBufferRef.current.trim();
      const finalContent = result.content?.trim();
      if (finalContent && finalContent !== streamed) {
        addMessage(setMessages, {
          kind: 'assistant',
          title: result.stopReason ?? 'end_turn',
          content: finalContent,
        });
      } else if (streamed) {
        addMessage(setMessages, {
          kind: 'assistant',
          title: result.stopReason ?? 'end_turn',
          content: streamed,
        });
      }
      resetTurnStream();
      pushEvent(`prompt complete: ${result.stopReason ?? 'end_turn'}`);
    } catch (err) {
      if (isCurrentConnection(client, connectionSeq)) {
        setError(err instanceof Error ? err.message : t('acp.error.prompt_failed'));
      }
    } finally {
      if (isCurrentConnection(client, connectionSeq)) {
        setBusy(false);
        setCancelRequested(false);
      }
    }
  };

  const cancelPrompt = () => {
    const client = clientRef.current;
    if (!client?.connected || !sessionId) return;
    client.notify('session/cancel', { sessionId });
    setCancelRequested(true);
    pushEvent('cancel requested');
  };

  const answerPermission = (permission: PermissionRequest, optionId?: string) => {
    const client = clientRef.current;
    if (!client?.connected) return;
    const outcome = optionId
      ? { outcome: 'selected', optionId }
      : { outcome: 'cancelled' };
    client.respond(permission.id, { outcome });
    setPermissions((current) => current.filter((item) => item.id !== permission.id));
    pushEvent(optionId ? `permission selected: ${optionId}` : 'permission cancelled');
  };

  const statusTone: BadgeTone = status === 'connected'
    ? 'ok'
    : status === 'connecting'
      ? 'warn'
      : 'error';

  return (
    <div className="p-6 max-w-7xl mx-auto h-full flex flex-col gap-4">
      <PageHeader
        title={
          <span className="inline-flex items-center gap-2.5">
            <Terminal className="h-5 w-5 text-pc-accent" />
            {t('acp.title')}
          </span>
        }
        description={t('acp.subtitle')}
        actions={
          // Wrap so the agent-select / status badge / reconnect button stack on
          // narrow screens instead of clipping out of a single non-wrapping row.
          <div className="flex flex-wrap items-center gap-2 min-w-0">
            <select
              value={selectedAgentAlias ?? ''}
              onChange={(event) => setSelectedAgentAlias(event.target.value || null)}
              disabled={agentsLoading || !hasEnabledAgent || busy}
              className="h-9 min-w-0 max-w-full rounded-[var(--radius-md)] border border-pc-border bg-pc-input px-3 text-[13px] font-medium text-pc-text-secondary disabled:opacity-40 disabled:cursor-not-allowed focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40"
              aria-label="ACP agent"
              title="ACP agent"
            >
              {agents.length === 0 || !agents.some((agent) => agent.enabled) ? (
                <option value="">
                  {agentsLoading
                    ? t('acp.agent.loading')
                    : agents.length === 0
                      ? t('acp.agent.none_configured')
                      : t('acp.agent.none_enabled')}
                </option>
              ) : (
                <>
                  <option value="">{t('acp.agent.server_default')}</option>
                  {agents.map((agent) => (
                    <option key={agent.alias} value={agent.alias} disabled={!agent.enabled}>
                      {agent.alias}{agent.enabled ? '' : ` (${t('acp.agent.disabled')})`}
                    </option>
                  ))}
                </>
              )}
            </select>
            <Badge tone={statusTone}>
              <Plug className="h-3 w-3" />
              {status}
            </Badge>
            <Button variant="ghost" size="md" onClick={connect} disabled={!hasEnabledAgent}>
              <RefreshCw className="h-4 w-4" />
              {t('acp.reconnect')}
            </Button>
          </div>
        }
      />

      <section className="grid gap-3 md:grid-cols-4">
        <StatusTile label={t('acp.status.server')} value={agentLabel} />
        <StatusTile
          label={t('acp.status.agent')}
          value={selectedAgentAlias ?? (agentsLoading ? t('acp.agent.loading') : t('acp.agent.server_default'))}
        />
        <StatusTile
          label={t('acp.status.session')}
          value={sessionId ?? (initializing ? t('acp.status.creating') : t('acp.status.not_ready'))}
        />
        <StatusTile label={t('acp.status.workspace')} value={workspaceDir ?? t('acp.status.gateway_default')} />
      </section>

      {error && (
        <div className="rounded-[var(--radius-md)] border border-status-error/20 bg-status-error/10 px-4 py-3 flex items-start gap-2 text-sm text-status-error">
          <AlertCircle className="h-4 w-4 mt-0.5 shrink-0" />
          <span>{error}</span>
        </div>
      )}

      <main className="grid min-h-0 flex-1 gap-4 lg:grid-cols-[minmax(0,1fr)_360px]">
        <Card padded={false} className="overflow-hidden flex min-h-[560px] flex-col shadow-[var(--pc-shadow-sm)]">
          <div className="border-b border-pc-border px-4 py-3 flex items-center justify-between">
            <div className="flex items-center gap-2 text-sm font-medium text-pc-text-secondary">
              <Terminal className="h-4 w-4" />
              {t('acp.transcript')}
            </div>
            {busy && (
              <Button variant="ghost" size="sm" onClick={cancelPrompt} disabled={cancelRequested}>
                <Square className="h-3.5 w-3.5" />
                {cancelRequested ? t('acp.cancelling') : t('acp.cancel')}
              </Button>
            )}
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
            {messages.length === 0 && !streamingText ? (
              <div className="h-full min-h-80 flex items-center justify-center text-sm text-pc-text-muted">
                {t('acp.empty_transcript')}
              </div>
            ) : (
              <>
                {messages.map((message) => (
                  <TranscriptMessage key={message.id} message={message} />
                ))}
                {streamingText && (
                  <TranscriptMessage
                    message={{
                      id: 'streaming',
                      kind: 'assistant',
                      title: 'streaming',
                      content: streamingText,
                      timestamp: nowLabel(),
                    }}
                  />
                )}
              </>
            )}
          </div>

          <form
            className="border-t border-pc-border bg-pc-surface p-4 flex flex-col gap-3 sm:flex-row"
            onSubmit={(event) => {
              event.preventDefault();
              void sendPrompt();
            }}
          >
            <textarea
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              rows={3}
              className="min-h-20 flex-1 resize-none rounded-[var(--radius-md)] border border-pc-border bg-pc-input px-3 py-2 text-sm text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-pc-accent/40 focus-visible:border-pc-accent/40"
              placeholder={t('acp.prompt_placeholder')}
            />
            <Button type="submit" size="md" disabled={!canSend} className="sm:w-32">
              <Send className="h-4 w-4" />
              {t('acp.send')}
            </Button>
          </form>
        </Card>

        <aside className="flex min-h-0 flex-col gap-4">
          <Card padded={false} className="overflow-hidden shadow-[var(--pc-shadow-sm)]">
            <div className="border-b border-pc-border px-4 py-3 flex items-center gap-2">
              <ShieldCheck className="h-4 w-4 text-pc-accent" />
              <h2 className="text-sm font-semibold text-pc-text">
                {t('acp.permissions')}
              </h2>
            </div>
            <div className="p-4 space-y-3">
              {permissions.length === 0 ? (
                <p className="text-sm text-pc-text-muted">
                  {t('acp.permissions_empty')}
                </p>
              ) : (
                permissions.map((permission) => (
                  <div
                    key={String(permission.id)}
                    className="rounded-[var(--radius-md)] border border-status-warning/20 bg-status-warning/5 p-3 space-y-3"
                  >
                    <div>
                      <div className="text-sm font-medium text-pc-text">
                        {permission.title}
                      </div>
                      {permission.sessionId && (
                        <div className="text-xs mt-1 font-mono text-pc-text-muted">
                          {permission.sessionId}
                        </div>
                      )}
                    </div>
                    {permission.detail && (
                      <pre className="max-h-36 overflow-auto whitespace-pre-wrap rounded-[var(--radius-sm)] bg-pc-code p-2 text-xs text-pc-text-secondary">
                        {permission.detail}
                      </pre>
                    )}
                    <div className="flex flex-wrap gap-2">
                      {permission.options.map((option) => (
                        <Button
                          key={option.optionId}
                          variant="ghost"
                          size="sm"
                          onClick={() => answerPermission(permission, option.optionId)}
                        >
                          <Check className="h-3.5 w-3.5" />
                          {option.name ?? option.kind ?? option.optionId}
                        </Button>
                      ))}
                      <Button variant="ghost" size="sm" onClick={() => answerPermission(permission)}>
                        <X className="h-3.5 w-3.5" />
                        {t('acp.dismiss')}
                      </Button>
                    </div>
                  </div>
                ))
              )}
            </div>
          </Card>

          <Card padded={false} className="overflow-hidden flex min-h-0 flex-1 flex-col shadow-[var(--pc-shadow-sm)]">
            <div className="border-b border-pc-border px-4 py-3">
              <h2 className="text-sm font-semibold text-pc-text">
                {t('acp.protocol_log')}
              </h2>
            </div>
            <div className="min-h-0 flex-1 overflow-y-auto p-4">
              {events.length === 0 ? (
                <p className="text-sm text-pc-text-muted">
                  {t('acp.protocol_waiting')}
                </p>
              ) : (
                <ol className="space-y-2">
                  {events.map((event, index) => (
                    <li
                      key={`${event}-${index}`}
                      className="rounded-[var(--radius-sm)] bg-pc-code px-2 py-1.5 font-mono text-xs text-pc-text-secondary"
                    >
                      {event}
                    </li>
                  ))}
                </ol>
              )}
            </div>
          </Card>
        </aside>
      </main>
    </div>
  );
}

function StatusTile({ label, value }: { label: string; value: string }) {
  return (
    <Card>
      <div className="text-[11px] font-medium uppercase tracking-wide text-pc-text-faint">
        {label}
      </div>
      <div className="mt-1 truncate text-sm font-medium text-pc-text" title={value}>
        {value}
      </div>
    </Card>
  );
}

// Calm per-kind treatment for transcript rows. The user turn carries a faint
// accent tint; everything else sits on neutral surfaces keyed by tokens.
const TRANSCRIPT_TONE: Record<ConsoleMessageKind, { label: string; className: string }> = {
  user: { label: 'You', className: 'bg-pc-accent/[0.06] border-pc-accent/25' },
  assistant: { label: 'Agent', className: 'bg-pc-elevated border-pc-border' },
  thought: { label: 'Thought', className: 'bg-pc-surface border-pc-border' },
  tool: { label: 'Tool', className: 'bg-pc-code border-pc-border' },
  system: { label: 'System', className: 'bg-pc-surface border-pc-border' },
};

function TranscriptMessage({ message }: { message: ConsoleMessage }) {
  const tone = TRANSCRIPT_TONE[message.kind];

  return (
    <article className={`rounded-[var(--radius-md)] border p-3 ${tone.className}`}>
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="text-[11px] font-semibold uppercase tracking-wide text-pc-text-muted">
          {message.title ?? tone.label}
        </div>
        <time className="text-[11px] font-mono text-pc-text-faint">
          {message.timestamp}
        </time>
      </div>
      <div className="whitespace-pre-wrap break-words text-sm leading-relaxed text-pc-text">
        {message.content}
      </div>
      {message.detail && (
        <pre className="mt-3 max-h-60 overflow-auto whitespace-pre-wrap rounded-[var(--radius-sm)] bg-pc-base p-2 text-xs text-pc-text-secondary">
          {message.detail}
        </pre>
      )}
    </article>
  );
}
