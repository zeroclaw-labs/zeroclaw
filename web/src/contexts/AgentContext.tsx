import { createContext, useContext, useEffect, useRef, useState, useCallback } from 'react';
import type { ApprovalDecision, PendingApproval, WsMessage } from '@/types/api';
import { WebSocketClient, getOrCreateSessionId } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import { t } from '@/lib/i18n';
import { getProp, putProp, listProps, getStatus, getSessionMessages, abortSession, deleteSession } from '@/lib/api';
import { primeModelProviderCatalog, modelProviderDisplayName } from '@/lib/modelProviders';
import type { ToolCallInfo } from '@/components/ToolCallCard';
import { resolveToolResultIndex } from '@/lib/toolCardMatch';
import {
  loadChatHistory,
  mapServerMessagesToPersisted,
  persistedToUiMessages,
  saveChatHistory,
  uiMessagesToPersisted,
} from '@/lib/chatHistoryStorage';

export interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  thinking?: string;
  markdown?: boolean;
  toolCall?: ToolCallInfo;
  timestamp: Date;
  /** True for messages composed locally in the web UI (verbatim user input).
   *  Such content never carries the gateway's `[timestamp]` prefix, so the
   *  bubble must NOT run stripServerTimestamp on it — otherwise a user message
   *  that happens to start with a bracketed datetime would be clipped. Only
   *  server-sourced messages (live stream + hydrated history) can be prefixed. */
  local?: boolean;
}

interface AgentContextValue {
  messages: ChatMessage[];
  sendMessage: (content: string) => void;
  connected: boolean;
  error: string | null;
  typing: boolean;
  streamingContent: string;
  streamingThinking: string;
  currentModel: string | null;
  availableModels: string[];
  switchModel: (model: string) => Promise<void>;
  modelLoading: boolean;
  /** Re-fetch model list from server. Useful after user edits config externally. */
  refreshModels: () => void;
  deleteMessage: (id: string) => void;
  clearAllMessages: () => void;
  abortSession: () => Promise<void>;
  /**
   * Pending supervised-mode tool-approval prompt, or null. Populated when the
   * gateway emits an `approval_request` frame; cleared once the user responds
   * or a fresh `approval_request` arrives. See #6522.
   */
  pendingApproval: PendingApproval | null;
  respondToApproval: (decision: ApprovalDecision) => void;
}

const AgentContext = createContext<AgentContextValue | null>(null);

export function useAgent() {
  const ctx = useContext(AgentContext);
  if (!ctx) throw new Error('useAgent must be used within AgentProvider');
  return ctx;
}

const MODEL_SWITCH_TIMEOUT_MS = 10_000;

function friendlyAgentError(message?: string): string {
  const raw = message?.trim() || t('agent.unknown_error');
  const localConnectFailure = raw.match(
    /model_provider=(\w+)\s+model=([^\s]+).*?url \((https?:\/\/[^)]+)\).*?(?:Connection refused|tcp connect error)/i,
  );
  if (localConnectFailure) {
    const provider = localConnectFailure[1] ?? '';
    const model = localConnectFailure[2] ?? 'the selected model';
    const url = localConnectFailure[3] ?? 'the configured endpoint';
    const displayProvider = modelProviderDisplayName(provider);
    return `${displayProvider} is unreachable at ${url}. Start the local provider service, confirm it serves ${model}, then try again.`;
  }
  return raw;
}

export interface AgentProviderProps {
  /** Configured agent alias this provider is bound to. The WebSocket
   * connection, session ID, and chat history are all scoped to this alias. */
  agentAlias: string;
  children: React.ReactNode;
}

export function AgentProvider({ agentAlias, children }: AgentProviderProps) {
  const sessionIdRef = useRef(getOrCreateSessionId(agentAlias));
  const [messages, setMessages] = useState<ChatMessage[]>(() => {
    const persisted = loadChatHistory(sessionIdRef.current);
    return persisted.length > 0 ? persistedToUiMessages(persisted) : [];
  });
  const [historyReady, setHistoryReady] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [typing, setTyping] = useState(false);
  const [streamingContent, setStreamingContent] = useState('');
  const [streamingThinking, setStreamingThinking] = useState('');
  const [currentModel, setCurrentModel] = useState<string | null>(null);
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const [modelInfoVersion, setModelInfoVersion] = useState(0);
  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null);

  const wsRef = useRef<WebSocketClient | null>(null);
  const pendingContentRef = useRef('');
  const pendingThinkingRef = useRef('');
  const capturedThinkingRef = useRef('');
  const pendingModelSwitchRef = useRef<string | null>(null);
  const switchTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const wsVersionRef = useRef(0);
  const localMessageMutationVersionRef = useRef(0);

  // Prime the model-provider catalog once so error formatting can resolve
  // display names from the backend registry rather than a local shadow list.
  useEffect(() => {
    void primeModelProviderCatalog();
  }, []);

  // Hydrate chat from server (preferred) or localStorage fallback
  useEffect(() => {
    const sid = sessionIdRef.current;
    const hydrationStartedAtMutationVersion = localMessageMutationVersionRef.current;
    let cancelled = false;

    (async () => {
      try {
        const res = await getSessionMessages(sid);
        if (cancelled) return;
        if (res.session_persistence) {
          if (localMessageMutationVersionRef.current === hydrationStartedAtMutationVersion) {
            setMessages(persistedToUiMessages(mapServerMessagesToPersisted(res.messages)));
          }
        } else if (!res.session_persistence) {
          setMessages((prev) => {
            if (prev.length > 0) return prev;
            const ls = loadChatHistory(sid);
            return ls.length ? persistedToUiMessages(ls) : prev;
          });
        }
      } catch {
        if (!cancelled) {
          setMessages((prev) => {
            if (prev.length > 0) return prev;
            const ls = loadChatHistory(sid);
            return ls.length ? persistedToUiMessages(ls) : prev;
          });
        }
      } finally {
        if (!cancelled) setHistoryReady(true);
      }
    })();

    return () => {
      cancelled = true;
    };
  }, []);

  // Mirror transcript to localStorage (bounded); server remains source of truth when persistence is on
  useEffect(() => {
    if (!historyReady) return;
    saveChatHistory(sessionIdRef.current, uiMessagesToPersisted(messages));
  }, [messages, historyReady]);

  // Auto-clear a pending approval when its timeout elapses on the backend.
  // The gateway auto-denies after `timeout_secs`; without this effect the
  // banner would linger indefinitely if the user just walked away. Add a
  // small grace buffer so the user is not penalised for last-second clicks.
  useEffect(() => {
    if (!pendingApproval) return;
    const elapsed = Date.now() - pendingApproval.receivedAt;
    const remainingMs = pendingApproval.timeoutSecs * 1000 - elapsed + 500;
    if (remainingMs <= 0) {
      setPendingApproval(null);
      return;
    }
    const id = setTimeout(() => {
      setPendingApproval((current) =>
        current && current.requestId === pendingApproval.requestId ? null : current,
      );
    }, remainingMs);
    return () => clearTimeout(id);
  }, [pendingApproval]);

  // Centralised WebSocket message handler — reused across initial connect and reconnects.
  const handleWsMessage = useCallback((msg: WsMessage) => {
    switch (msg.type) {
      case 'session_start':
      case 'connected':
        break;

      case 'thinking':
        setTyping(true);
        pendingThinkingRef.current += msg.content ?? '';
        setStreamingThinking(pendingThinkingRef.current);
        break;

      case 'chunk':
        setTyping(true);
        pendingContentRef.current += msg.content ?? '';
        setStreamingContent(pendingContentRef.current);
        break;

      case 'chunk_reset':
        // Server signals that the authoritative done message follows.
        // Snapshot thinking before clearing display state.
        capturedThinkingRef.current = pendingThinkingRef.current;
        pendingContentRef.current = '';
        pendingThinkingRef.current = '';
        setStreamingContent('');
        setStreamingThinking('');
        break;

      case 'message':
      case 'done': {
        const raw_content = msg.full_response ?? msg.content ?? pendingContentRef.current;
        // Skip whitespace-only content (e.g. models that emit "\n\n"
        // alongside tool_calls) to avoid accumulating blank lines in the
        // assistant bubble. Ref: #6702.
        const content = raw_content.trim();
        const thinking = capturedThinkingRef.current || pendingThinkingRef.current || undefined;
        if (content) {
          localMessageMutationVersionRef.current += 1;
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content,
              thinking,
              markdown: true,
              timestamp: new Date(),
            },
          ]);
        }
        pendingContentRef.current = '';
        pendingThinkingRef.current = '';
        capturedThinkingRef.current = '';
        setStreamingContent('');
        setStreamingThinking('');
        setTyping(false);
        break;
      }

      case 'tool_call': {
        // Defense in depth (issue #7151): the chat WebSocket shares a broadcast
        // bus with observability telemetry, whose `tool_call` frames have a
        // different shape (`tool`/`duration_ms`/`success`) and carry no `name`.
        // Such a frame would otherwise produce a permanent "unknown" tool card
        // with a spinner that never resolves (no matching `tool_result`). The
        // backend already filters these out, but ignore them here too so a
        // malformed telemetry frame can never render a stuck card.
        if (!msg.name) {
          break;
        }
        const toolName = msg.name;
        const toolArgs = msg.args;
        localMessageMutationVersionRef.current += 1;
        setMessages((prev) => {
          const argsKey = JSON.stringify(toolArgs ?? {});
          if (pendingContentRef.current) {
            const isDuplicate = prev.some(
              (m) => m.toolCall
                && m.toolCall.output === undefined
                && m.toolCall.name === toolName
                && JSON.stringify(m.toolCall.args ?? {}) === argsKey,
            );
            if (isDuplicate) return prev;
          }

          return [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent' as const,
              content: `${t('agent.tool_call_prefix')} ${toolName}(${argsKey})`,
              toolCall: { name: toolName, args: toolArgs, id: msg.id },
              timestamp: new Date(),
            },
          ];
        });
        break;
      }

      case 'tool_result': {
        // Defense in depth (issue #7151): mirrors the `tool_call` guard
        // above. Observability `tool_result`-shaped telemetry would otherwise
        // overwrite the most recent pending tool card with an empty output.
        if (!msg.name) {
          break;
        }
        const toolName = msg.name;
        const resultId = msg.id;
        localMessageMutationVersionRef.current += 1;
        setMessages((prev) => {
          // Correlate the result to its pending card by gateway tool_call_id so
          // out-of-order parallel results land on the right card; see
          // resolveToolResultIndex for the id-less fallback.
          const idx = resolveToolResultIndex(prev, resultId);
          if (idx !== -1) {
            const updated = [...prev];
            const existing = prev[idx]!;
            updated[idx] = {
              ...existing,
              toolCall: { ...existing.toolCall!, output: msg.output ?? '' },
            };
            return updated;
          }
          return [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent' as const,
              content: `${t('agent.tool_result_prefix')} ${msg.output ?? ''}`,
              toolCall: { name: toolName, output: msg.output ?? '' },
              timestamp: new Date(),
            },
          ];
        });
        break;
      }

      case 'cron_result': {
        const cronOutput = msg.output ?? '';
        if (cronOutput) {
          localMessageMutationVersionRef.current += 1;
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent' as const,
              content: cronOutput,
              markdown: true,
              timestamp: new Date(msg.timestamp ?? Date.now()),
            },
          ]);
        }
        break;
      }

      case 'approval_request': {
        // Supervised-mode tool consent prompt. Backend parks on a oneshot
        // until we send `approval_response`; if the socket closes or the
        // timeout elapses, the backend auto-denies on its side.
        if (!msg.request_id) break;
        setPendingApproval({
          requestId: msg.request_id,
          toolName: msg.tool ?? 'unknown',
          argumentsSummary: msg.arguments_summary ?? '',
          timeoutSecs: msg.timeout_secs ?? 120,
          receivedAt: Date.now(),
        });
        break;
      }

      case 'aborted': {
        // Gateway sends this after a cancelled turn; the parked approval (if
        // any) is no longer valid because its request_id belongs to the old
        // turn. Clear so the banner does not linger across the abort.
        pendingContentRef.current = '';
        pendingThinkingRef.current = '';
        capturedThinkingRef.current = '';
        setStreamingContent('');
        setStreamingThinking('');
        setTyping(false);
        setPendingApproval(null);
        break;
      }

      case 'error':
        const friendlyMessage = friendlyAgentError(msg.message);
        localMessageMutationVersionRef.current += 1;
        setMessages((prev) => [
          ...prev,
          {
            id: generateUUID(),
            role: 'agent',
            content: `${t('agent.error_prefix')} ${friendlyMessage}`,
            timestamp: new Date(),
          },
        ]);
        if (msg.code === 'AGENT_INIT_FAILED' || msg.code === 'AUTH_ERROR' || msg.code === 'PROVIDER_ERROR') {
          setError(`${t('agent.configuration_error')}: ${friendlyMessage}`);
        } else if (msg.code === 'INVALID_JSON' || msg.code === 'UNKNOWN_MESSAGE_TYPE' || msg.code === 'EMPTY_CONTENT') {
          setError(`${t('agent.message_error')}: ${msg.message}`);
        }
        setTyping(false);
        pendingContentRef.current = '';
        pendingThinkingRef.current = '';
        setStreamingContent('');
        setStreamingThinking('');
        setPendingApproval(null);
        break;
    }
  }, []);

  // Wire up a WebSocketClient instance with version-guarded callbacks.
  const attachSocketCallbacks = useCallback((ws: WebSocketClient) => {
    const version = ++wsVersionRef.current;

    ws.onOpen = () => {
      if (version !== wsVersionRef.current) return;
      setConnected(true);
      setError(null);

      // If we just reconnected after a model switch, apply the pending model now.
      if (pendingModelSwitchRef.current) {
        if (switchTimeoutRef.current) {
          clearTimeout(switchTimeoutRef.current);
          switchTimeoutRef.current = null;
        }
        setCurrentModel(pendingModelSwitchRef.current);
        setModelInfoVersion((v) => v + 1);
        pendingModelSwitchRef.current = null;
        setModelLoading(false);
      }
    };

    ws.onClose = (ev: CloseEvent) => {
      // Clear pending approval ahead of the version guard: even if this is a
      // stale socket whose other state we don't want to write, the parked
      // request_id is gone on the server side regardless and the banner must
      // not survive the close.
      setPendingApproval(null);
      if (version !== wsVersionRef.current) return;
      setConnected(false);

      if (pendingModelSwitchRef.current) {
        // We intentionally closed the old socket; non-normal codes mean the reconnect failed.
        if (ev.code !== 1000 && ev.code !== 1001) {
          setError(`${t('agent.connection_closed')} (code: ${ev.code}). ${t('agent.check_configuration')}.`);
        }
        pendingModelSwitchRef.current = null;
        if (switchTimeoutRef.current) {
          clearTimeout(switchTimeoutRef.current);
          switchTimeoutRef.current = null;
        }
        setModelLoading(false);
        return;
      }

      if (ev.code !== 1000 && ev.code !== 1001) {
        setError(`${t('agent.connection_closed')} (code: ${ev.code}). ${t('agent.check_configuration')}.`);
      }
    };

    ws.onError = () => {
      if (version !== wsVersionRef.current) return;
      // During a model switch we let onClose deliver the final verdict.
      if (!pendingModelSwitchRef.current) {
        setError(t('agent.connection_error'));
      }
    };

    ws.onMessage = (msg: WsMessage) => {
      if (version !== wsVersionRef.current) return;
      handleWsMessage(msg);
    };
  }, [handleWsMessage]);

  // WebSocket bound to the configured agent. Re-keys (via the outer
  // <AgentProvider key={alias}>) when the alias changes.
  useEffect(() => {
    const ws = new WebSocketClient({ agentAlias });
    attachSocketCallbacks(ws);
    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, [attachSocketCallbacks, agentAlias]);

  // Fetch current model and available models from config.
  useEffect(() => {
    let cancelled = false;

    async function loadModelInfo() {
      try {
        // Agent-scoped status: `/api/status?agent=<alias>` runs the same
        // `resolved_model_provider_for_agent` logic the gateway uses to build
        // the Agent, so the fallback `status.model` is correct for THIS agent
        // rather than the install-wide default. (Ported from
        // zeroclaw-labs/zeroclaw#7191.)
        const status = await getStatus(agentAlias);
        if (cancelled) return;

        let activeModel = status.model;

        // Per-agent model (multi-agent / schema V3). The old global `model` /
        // `default_model` keys were removed in V3, so the source of truth is THIS
        // agent's own `agents.<alias>.model_provider` — a ref into
        // `providers.models.<family>.<alias>`. (Previously this read the global
        // keys, which now 404, so the button fell back to the daemon's global
        // status model — the wrong model on every agent page.)
        let activeRef: string | null = null;
        try {
          const refProp = await getProp(`agents.${agentAlias}.model_provider`);
          // NOTE: GET /api/config/prop returns only `{ path, value }` — it has
          // no `populated` field (that exists only on /api/config/list). A set
          // prop has a string value; an unset one returns the "<unset>"
          // sentinel; a missing path throws. So gate on the value, not
          // `populated` (which would be undefined here and silently fail).
          if (typeof refProp.value === 'string' && refProp.value !== '<unset>') {
            activeRef = refProp.value;
          }
        } catch {
          // ignore — fall back to the status value below
        }
        if (cancelled) return;
        // Show the agent's configured provider ref (e.g. "kilo.minimax_m3"),
        // falling back to the daemon status model only if unset.
        setCurrentModel(activeRef ?? activeModel);

        // Available switch targets = every configured provider ref
        // (`providers.models.<family>.<alias>`), discovered via config/list.
        try {
          const list = await listProps('providers.models');
          if (cancelled) return;
          const refs = (list.entries ?? [])
            .map((e) => e.path)
            .filter((p) => /^providers\.models\.[^.]+\.[^.]+\.model$/.test(p))
            .map((p) => p.replace(/^providers\.models\./, '').replace(/\.model$/, ''));
          const unique = Array.from(new Set(refs));
          setAvailableModels(unique.length > 0 ? unique : activeRef ? [activeRef] : []);
        } catch {
          setAvailableModels(activeRef ? [activeRef] : []);
        }
      } catch {
        // Ignore errors — dropdown will just show current model once loaded
      }
    }

    loadModelInfo();

    return () => {
      cancelled = true;
    };
  }, [modelInfoVersion, agentAlias]);

  const sendMessage = useCallback((content: string) => {
    if (!wsRef.current?.connected) return;
    try {
      wsRef.current.sendMessage(content);
      setTyping(true);
      pendingContentRef.current = '';
      pendingThinkingRef.current = '';
      localMessageMutationVersionRef.current += 1;
      setMessages((prev) => [
        ...prev,
        {
          id: generateUUID(),
          role: 'user',
          content,
          timestamp: new Date(),
          local: true,
        },
      ]);
    } catch {
      setError(t('agent.send_error'));
    }
  }, []);

  const switchModel = useCallback(async (model: string) => {
    if (modelLoading) return; // debounce
    setModelLoading(true);
    pendingModelSwitchRef.current = model;

    // Watchdog so the UI can never get stuck on the loading spinner. It is
    // armed once per phase — for the config write, then again for the socket
    // reconnect — so each phase gets its own full budget. A single timer armed
    // at the top had to cover *both* phases: a slow daemon write could consume
    // the whole budget and fire "model switch timed out" while the switch was
    // still progressing (and, because it nulled the pending ref, the later
    // onOpen would skip updating currentModel — a timeout error for a switch
    // that actually succeeded). Splitting the budget keeps the spinner bounded
    // against a hung request *and* a reconnect that never opens, without the
    // false positive. The `=== model` identity check stops a fired watchdog
    // from clobbering a newer switch. (Ported from zeroclaw-labs/zeroclaw#7191.)
    const armWatchdog = () => {
      if (switchTimeoutRef.current) clearTimeout(switchTimeoutRef.current);
      switchTimeoutRef.current = setTimeout(() => {
        // The timer has fired; null its ref so it honestly reflects
        // "no watchdog armed" (a later clearTimeout on the dead id is a no-op).
        switchTimeoutRef.current = null;
        if (pendingModelSwitchRef.current === model) {
          pendingModelSwitchRef.current = null;
          setModelLoading(false);
          setError(t('agent.model_switch_timeout'));
        }
      }, MODEL_SWITCH_TIMEOUT_MS);
    };
    armWatchdog();

    try {
      // Per-agent switch: write THIS agent's own model_provider ref (multi-agent
      // / schema V3). `model` here is a provider ref (e.g. "kilo.minimax_m3").
      // The global `model`/`default_model` keys were removed in V3.
      await putProp(`agents.${agentAlias}.model_provider`, model);
      // The write-phase watchdog may have fired (or a newer switch may have
      // superseded this one) while the request was in flight. Bail before
      // touching the live socket so we never tear it down after giving up.
      if (pendingModelSwitchRef.current !== model) return;

      // If a turn is actively streaming, abort it on the backend before we tear
      // down the socket. This prevents the old model from continuing to execute
      // tools or persisting its response into the session after we switch.
      if (typing) {
        try {
          await Promise.race([
            abortSession(sessionIdRef.current),
            new Promise((_, reject) =>
              setTimeout(() => reject(new Error('abort-timeout')), 1_500),
            ),
          ]);
        } catch {
          // Best-effort: if abort fails or times out we still proceed with the
          // switch so the user is never stuck. The old turn may continue on the
          // server, but the UI will show a clean new session.
        }
      }

      // Abort any in-flight streaming before rebuilding the connection.
      pendingContentRef.current = '';
      pendingThinkingRef.current = '';
      capturedThinkingRef.current = '';
      setStreamingContent('');
      setStreamingThinking('');
      setTyping(false);
      // The old socket's request_id no longer maps to anything on the server
      // after we tear it down. Clear here explicitly because we null out the
      // old socket's callbacks below, so its onClose will not fire to do it.
      setPendingApproval(null);

      // Re-arm the watchdog with a fresh budget for the reconnect phase — the
      // one step no awaited promise covers. Bail first if the write phase
      // already timed out (or a newer switch superseded this one).
      if (pendingModelSwitchRef.current !== model) return;
      armWatchdog();

      // Tear down the old socket and create a fresh one.
      // The backend will read the updated config when the new socket opens
      // and construct a new Agent with the selected model.
      const oldWs = wsRef.current;
      if (oldWs) {
        oldWs.onOpen = null;
        oldWs.onClose = null;
        oldWs.onError = null;
        oldWs.onMessage = null;
        oldWs.disconnect();
      }

      const ws = new WebSocketClient({ agentAlias });
      // Point wsRef at the NEW client before connect(), so a synchronous
      // connect() throw (e.g. an invalid WebSocket URL/protocol token) still
      // leaves a live, reconnect-capable socket in the ref instead of the old
      // intentionally-closed one — otherwise the page strands offline with no
      // reconnect path until reload.
      wsRef.current = ws;
      attachSocketCallbacks(ws);
      ws.connect();
    } catch (err) {
      // If the per-phase watchdog already fired (timed out) and nulled the
      // pending ref while this request was in flight, bail so a late rejection
      // doesn't overwrite the timeout state. (The `if (modelLoading) return`
      // debounce above prevents truly concurrent switches.)
      if (pendingModelSwitchRef.current !== model) return;
      if (switchTimeoutRef.current) {
        clearTimeout(switchTimeoutRef.current);
        switchTimeoutRef.current = null;
      }
      pendingModelSwitchRef.current = null;
      setModelLoading(false);
      setError(err instanceof Error ? err.message : t('agent.failed_switch_model'));
    }
  }, [attachSocketCallbacks, modelLoading, typing, agentAlias]);

  const deleteMessage = useCallback((id: string) => {
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => prev.filter((m) => m.id !== id));
  }, []);

  const clearAllMessages = useCallback(() => {
    // Optimistically wipe the rendered transcript and any in-flight streaming
    // state. Bumping the mutation version fences off a racing hydration fetch
    // so it can't repopulate the just-cleared view.
    localMessageMutationVersionRef.current += 1;
    setMessages([]);
    pendingContentRef.current = '';
    pendingThinkingRef.current = '';
    capturedThinkingRef.current = '';
    setStreamingContent('');
    setStreamingThinking('');
    setTyping(false);
    setPendingApproval(null);

    const sid = sessionIdRef.current;

    // The live WebSocket Agent holds the full conversation in memory for the
    // life of the connection, and the gateway persists each turn to the
    // session backend. Clearing only React state leaves both intact, so the
    // next turn still carries prior context and a reload/reconnect repopulates
    // the old transcript (issue #7126). To genuinely reset context we must:
    //   1. delete the persisted backend session history, and
    //   2. tear down + reconnect the socket so the gateway builds a fresh
    //      Agent that seeds from the now-empty backend.
    void (async () => {
      try {
        // Reuses the existing DELETE /api/sessions/{id} primitive
        // (gateway -> SessionBackend::delete_session). Best-effort: a 404 when
        // session persistence is disabled, or any transport failure, must not
        // block the local clear or the reconnect below.
        await deleteSession(sid);
      } catch {
        // Persistence disabled or request failed — proceed with the reconnect
        // so the live in-memory context is reset regardless.
      }

      // Rebuild the socket so the backend constructs a new Agent with no seeded
      // history. Detach the old socket's callbacks first so its onClose does
      // not write stale connection state. If there is no socket (disconnected),
      // the alias-bound effect will reconnect on its own — nothing to do here.
      const oldWs = wsRef.current;
      if (oldWs) {
        oldWs.onOpen = null;
        oldWs.onClose = null;
        oldWs.onError = null;
        oldWs.onMessage = null;
        oldWs.disconnect();

        const ws = new WebSocketClient({ agentAlias });
        // Assign wsRef before connect() so a synchronous throw can't strand the
        // page on the old intentionally-closed socket (see switchModel).
        wsRef.current = ws;
        attachSocketCallbacks(ws);
        ws.connect();
      }
    })();
  }, [agentAlias, attachSocketCallbacks]);

  const respondToApproval = useCallback((decision: ApprovalDecision) => {
    setPendingApproval((current) => {
      if (!current) return null;
      try {
        wsRef.current?.sendApprovalResponse(current.requestId, decision);
      } catch {
        // Socket closed mid-prompt; backend will auto-deny on its side.
      }
      return null;
    });
  }, []);

  const value: AgentContextValue = {
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
    refreshModels: () => setModelInfoVersion((v) => v + 1),
    deleteMessage,
    clearAllMessages,
    abortSession: async () => {
      // Clear local approval state immediately — the in-flight request_id
      // belongs to the turn we're cancelling and will be rejected by the
      // backend on a late click anyway. Don't wait for the `aborted` frame
      // to round-trip; the user clicked Stop and expects the UI to follow.
      setPendingApproval(null);
      try {
        await abortSession(sessionIdRef.current);
      } catch {
        // Best-effort abort
      }
    },
    pendingApproval,
    respondToApproval,
  };

  return <AgentContext.Provider value={value}>{children}</AgentContext.Provider>;
}
