import { createContext, useContext, useEffect, useRef, useState, useCallback } from 'react';
import type { ApprovalDecision, PendingApproval, WsMessage } from '@/types/api';
import { WebSocketClient, getOrCreateSessionId, isSideQueryRequestId } from '@/lib/ws';
import { generateUUID } from '@/lib/uuid';
import { t } from '@/lib/i18n';
import { sideQueryQuestion } from '@/lib/slashCommands';
import { getProp, putProp, listProps, getStatus, getSessionMessages, abortSession, deleteSession } from '@/lib/api';
import { primeModelProviderCatalog, modelProviderDisplayName } from '@/lib/modelProviders';
import type { ToolCallInfo } from '@/components/ToolCallCard';
import { resolveToolResultIndex } from '@/lib/toolCardMatch';
import { TurnSequencer, type SequencedRelease } from '@/lib/turnSequencer';
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
  /**
   * Locally-generated info/system message produced by web slash-command
   * handlers (`/help`, `/model`, `/btw`, unknown-command notices). Excluded from
   * persistence so command output does not pollute localStorage and reappear
   * as fake assistant replies on reload. See #7137.
   */
  ephemeral?: boolean;
  /** Read-only /btw exchange, kept only in this mounted chat view. */
  sideQuery?: boolean;
  /** User-visible lifecycle notice, rendered distinctly from agent output. */
  notice?: boolean;
}

/** S2 correlation state for the one in-flight request this surface tracks
 *  (#549). `turnId` binds after `turn_ack: accepted` (or the first event /
 *  replay, when the ACK was lost). Sequenced events are applied STRICTLY in
 *  seq order through `sequencer` (#563 review): the live ring and a resume
 *  snapshot can interleave (live seq N+1 may arrive before the replay
 *  carrying N), so N+1 is held until N is applied or declared unrecoverable.
 *  `sequencer.resumeSeq` is the contiguous anchor resume asks after. */
interface ActiveTurn {
  requestId: string;
  turnId: string | null;
  sequencer: TurnSequencer<WsMessage>;
  /** The terminal marker was RECEIVED (it may still be buffered behind a
   * hole); the outcome is known even before it is applied. */
  terminalSeen: boolean;
  /** Stop remains pending until the authoritative turn id can bind it. */
  cancelRequested: boolean;
  /** Grace timer armed while a hole holds events back. First expiry asks
   * for a resume; the second declares the hole unrecoverable and flushes. */
  holeTimer: ReturnType<typeof setTimeout> | null;
  holeTimerStage: 0 | 1;
}

/** Grace for a hole to be filled by an in-flight / requested replay. */
const HOLE_GRACE_MS = 3_000;
// Match gateway limits; allow its default 60s timeout to arrive first.
const MAX_SIDE_QUERIES = 4;
const MAX_SIDE_QUERY_BYTES = 16 * 1024;
const SIDE_QUERY_TIMEOUT_MS = 65_000;

interface AgentContextValue {
  messages: ChatMessage[];
  sendMessage: (content: string, attachments?: string[]) => void;
  sendSideQuery: (question: string, attachments?: string[]) => boolean;
  steerMessage: (content: string, attachments?: string[]) => boolean;
  canSteer: boolean;
  steeringPending: boolean;
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
  /**
   * Append a locally-generated info/system message to the transcript without
   * sending anything to the gateway. Used by web slash-command handlers
   * (`/help`, `/model`, unknown-command notices) to surface feedback inline.
   * See #7137.
   */
  addLocalMessage: (content: string) => void;
  abortSession: () => Promise<void>;
  /** True between a scoped Stop being requested and its terminal confirm —
   *  distinct from "confirmed stopped" so the UI never shows success early. */
  stopRequested: boolean;
  /**
   * Pending supervised-mode tool-approval prompt, or null. Populated when the
   * gateway emits an `approval_request` frame; cleared once the user responds
   * or a fresh `approval_request` arrives. See #6522.
   */
  pendingApproval: PendingApproval | null;
  respondToApproval: (decision: ApprovalDecision) => void;
  // Context window tracking (from "done" WS frames). See #7311.
  contextMaxTokens: number | null;
  contextInputTokens: number | null;
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
  // Context window tracking (from "done" WS frames). See #7311.
  const [contextMaxTokens, setContextMaxTokens] = useState<number | null>(null);
  const [contextInputTokens, setContextInputTokens] = useState<number | null>(null);
  // S2 (#549): between a scoped Stop being requested and its terminal confirm.
  const [stopRequested, setStopRequested] = useState(false);
  const [steeringAvailable, setSteeringAvailable] = useState(false);
  const [steeringPending, setSteeringPending] = useState(false);
  const pendingSteerRef = useRef<{
    requestId: string; parentRequestId: string; turnId: string; epoch: string;
    timer: ReturnType<typeof setTimeout> | null;
  } | null>(null);
  const sideQueriesRef = useRef(new Map<string, {
    messageId: string;
    timer: ReturnType<typeof setTimeout>;
  }>());

  const wsRef = useRef<WebSocketClient | null>(null);
  const pendingContentRef = useRef('');
  const pendingThinkingRef = useRef('');
  const capturedThinkingRef = useRef('');
  const pendingModelSwitchRef = useRef<string | null>(null);
  const switchTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const wsVersionRef = useRef(0);
  const localMessageMutationVersionRef = useRef(0);
  // S2 (#549): correlation for the one in-flight request, the epoch seen by
  // the previous socket (reconnect reconciliation), and model-switch waiters
  // parked until the cancelled turn's terminal marker arrives.
  const activeTurnRef = useRef<ActiveTurn | null>(null);
  const lastEpochRef = useRef<string | null>(null);
  const terminalWaitersRef = useRef(new Map<string, () => void>());

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

  // S2 lifecycle helpers (#549). Notices are ephemeral transcript bubbles;
  // finalize clears correlation, the stop badge and the typing spinner.
  const addNotice = useCallback((content: string) => {
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'agent',
        content,
        timestamp: new Date(),
        ephemeral: true,
        notice: true,
      },
    ]);
  }, []);

  const settleSideQuery = useCallback((requestId: string, content: string, notice = false) => {
    const pending = sideQueriesRef.current.get(requestId);
    if (!pending) return; // Duplicate, retired attachment or unknown broadcast.
    clearTimeout(pending.timer);
    sideQueriesRef.current.delete(requestId);
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => prev.map((message) => message.id === pending.messageId
      ? { ...message, content, markdown: !notice, notice }
      : message));
  }, []);

  const clearSideQueries = useCallback((notice?: string) => {
    const pending = [...sideQueriesRef.current.values()];
    sideQueriesRef.current.clear();
    for (const query of pending) clearTimeout(query.timer);
    if (!notice || pending.length === 0) return;
    const ids = new Set(pending.map((query) => query.messageId));
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => prev.map((message) => ids.has(message.id)
      ? { ...message, content: notice, markdown: false, notice: true }
      : message));
  }, []);

  const sendSideQuery = useCallback((rawQuestion: string, attachments?: string[]): boolean => {
    const question = rawQuestion.trim();
    if (attachments?.length) {
      addNotice(t('agent.btw_attachments'));
      return false;
    }
    if (!question || new TextEncoder().encode(question).length > MAX_SIDE_QUERY_BYTES) {
      addNotice(t('agent.btw_invalid'));
      return false;
    }
    if (sideQueriesRef.current.size >= MAX_SIDE_QUERIES) {
      addNotice(t('agent.btw_busy'));
      return false;
    }
    try {
      const requestId = wsRef.current?.sendSideQuery(question);
      if (!requestId) {
        addNotice(t('agent.btw_not_ready'));
        return false;
      }
      const messageId = generateUUID();
      const timer = setTimeout(() => settleSideQuery(requestId, t('agent.btw_unknown'), true), SIDE_QUERY_TIMEOUT_MS);
      sideQueriesRef.current.set(requestId, { messageId, timer });
      localMessageMutationVersionRef.current += 1;
      setMessages((prev) => [
        ...prev,
        { id: generateUUID(), role: 'user', content: `/btw ${question}`, timestamp: new Date(), local: true, ephemeral: true, sideQuery: true },
        { id: messageId, role: 'agent', content: t('agent.btw_pending'), timestamp: new Date(), ephemeral: true, sideQuery: true },
      ]);
      return true;
    } catch {
      addNotice(t('agent.send_error'));
      return false;
    }
  }, [addNotice, settleSideQuery]);

  const settleSteering = useCallback((notice?: string) => {
    const pending = pendingSteerRef.current;
    if (!pending) return;
    if (pending.timer) clearTimeout(pending.timer);
    pendingSteerRef.current = null;
    setSteeringPending(false);
    if (notice) addNotice(notice);
  }, [addNotice]);

  const finalizeActiveTurn = useCallback(() => {
    if (activeTurnRef.current?.holeTimer) {
      clearTimeout(activeTurnRef.current.holeTimer);
    }
    activeTurnRef.current = null;
    setSteeringAvailable(false);
    settleSteering(t('agent.steer_unknown'));
    setStopRequested(false);
    setTyping(false);
  }, [settleSteering]);

  /** Resolve (without waiting) any model-switch waiter parked on this turn. */
  const releaseTerminalWaiter = useCallback((turnId: string) => {
    terminalWaitersRef.current.get(turnId)?.();
    terminalWaitersRef.current.delete(turnId);
  }, []);

  const waitForTerminal = useCallback((turnId: string, ms: number) =>
    new Promise<boolean>((resolve) => {
      const timer = setTimeout(() => {
        terminalWaitersRef.current.delete(turnId);
        resolve(false);
      }, ms);
      terminalWaitersRef.current.set(turnId, () => {
        clearTimeout(timer);
        terminalWaitersRef.current.delete(turnId);
        resolve(true);
      });
    }), []);

  // Centralised WebSocket message handler — reused across initial connect and
  // reconnects, and re-entered for S2-wrapped display frames (#549): a
  // `turn_event.event` payload is an ordinary display frame, so it recurses
  // through this same handler rather than a parallel display path.
  const handleWsMessage = useCallback(function handleWsMessage(msg: WsMessage) {
    // /btw is not a main turn: neither its ACK nor any result/error may bind,
    // finalize, resume or clear the main stream/Stop state. Dedicated side
    // frames echo `id`, including on an attachment negotiated for S2 turns.
    const sideRequestId = msg.request_id ?? msg.id;
    if (msg.type === 'btw_response' || msg.type === 'btw_error') {
      if (sideRequestId) {
        settleSideQuery(sideRequestId, msg.type === 'btw_response'
          ? (msg.full_response ?? msg.content ?? '')
          : t('agent.btw_failed').replace('{message}', msg.message ?? msg.code ?? t('agent.unknown_error')),
        msg.type === 'btw_error');
      }
      return;
    }
    // Only receipts/errors use this request-id namespace. Ordinary display
    // frames can carry an unrelated tool-call `id` with the same prefix.
    if (isSideQueryRequestId(sideRequestId)
      && ['turn_ack', 'ack', 'turn_error', 'error'].includes(msg.type)) {
      if (msg.type === 'error' || msg.type === 'turn_error'
        || (msg.type === 'turn_ack' && msg.status !== 'btw')
        || (msg.type === 'ack' && msg.status !== 'accepted')) {
        settleSideQuery(sideRequestId, t('agent.btw_failed').replace('{message}',
          msg.message ?? msg.code ?? msg.status ?? t('agent.unknown_error')), true);
      }
      // Includes late errors after timeout/disconnect, which must stay inert
      // even after their pending entry has been retired.
      return;
    }
    if (msg.type === 'turn_ack' && msg.status === 'btw') return;

    /** Apply ONE ordered `turn_event` wrapper (its seq is next in order). */
    const applyTurnEvent = (active: ActiveTurn, wrapper: WsMessage) => {
      const inner = wrapper.event;
      if (!inner) return;
      if (inner.type === 'turn_terminal') {
        if (inner.outcome === 'aborted') {
          // Mirror the legacy `aborted` cleanup; the server publishes the
          // legacy frame too, but the marker can arrive as the sole witness
          // on a replayed terminal.
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          capturedThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          setPendingApproval(null);
        }
        releaseTerminalWaiter(wrapper.turn_id ?? active.turnId ?? '');
        // Ordered delivery guarantees every lower seq was applied (or
        // declared lost) before the terminal: settling now drops nothing.
        finalizeActiveTurn();
        return;
      }
      handleWsMessage(inner);
    };

    /** Apply a sequencer release in order, then re-evaluate the hole timer. */
    const applyRelease = (active: ActiveTurn, release: SequencedRelease<WsMessage>) => {
      if (release.skipped.length > 0) {
        addNotice(t('agent.replay_gap'));
      }
      for (const { event } of release.ready) {
        if (activeTurnRef.current !== active) return; // settled mid-release
        applyTurnEvent(active, event);
      }
      if (activeTurnRef.current !== active) return;
      if (!active.sequencer.hasHole) {
        if (active.holeTimer) clearTimeout(active.holeTimer);
        active.holeTimer = null;
        active.holeTimerStage = 0;
        return;
      }
      if (active.holeTimer) return;
      const arm = () => {
        active.holeTimer = setTimeout(() => {
          active.holeTimer = null;
          if (activeTurnRef.current !== active || !active.sequencer.hasHole) return;
          if (active.holeTimerStage === 0) {
            // First expiry: the replay that fills the hole may never have
            // been requested (e.g. a silently dropped live frame) — ask.
            active.holeTimerStage = 1;
            wsRef.current?.resumeTurn(active.requestId, active.sequencer.resumeSeq);
            arm();
            return;
          }
          // Second expiry: declare the hole unrecoverable, apply the rest
          // in order (a buffered terminal settles the turn).
          active.holeTimerStage = 0;
          applyRelease(active, active.sequencer.flush());
        }, HOLE_GRACE_MS);
      };
      arm();
    };

    /** Route one sequenced `turn_event` wrapper (live or replayed). */
    const offerTurnEvent = (active: ActiveTurn, wrapper: WsMessage) => {
      if (wrapper.event?.type === 'turn_terminal') {
        active.terminalSeen = true;
        setSteeringAvailable(false);
      }
      if (typeof wrapper.seq !== 'number') {
        // Unsequenced (legacy) frame: no ordering contract to honour.
        applyTurnEvent(active, wrapper);
        return;
      }
      applyRelease(active, active.sequencer.offer(wrapper.seq, wrapper));
    };

    const bindTurn = (active: ActiveTurn, turnId: string | null | undefined) => {
      if (active.turnId !== null || typeof turnId !== 'string') return;
      active.turnId = turnId;
      setSteeringAvailable(wsRef.current?.canSteer === true && !active.cancelRequested);
      if (active.cancelRequested) {
        wsRef.current?.cancelTurn(active.requestId, turnId);
      }
    };

    switch (msg.type) {
      case 'session_start':
      case 'connected':
        break;

      case 'turn_state':
        // Turn-control frames from #457. The PWA renders no turn-control UI
        // yet (#379), so these stay intentionally inert; typed here so the
        // frame shape is part of the contract instead of silently unknown.
        break;

      case 'turn_ready': {
        // Negotiation completed on this socket. Reconcile a request still
        // pending from a previous socket: same process epoch → resume from
        // the contiguous anchor (a held-open terminal resumes too — the
        // replay answer settles it); a changed epoch means the gateway
        // restarted and a still-unknown outcome is surfaced, never resent
        // (the server journal is gone with the process).
        const epoch = typeof msg.epoch === 'string' ? msg.epoch : null;
        const previous = lastEpochRef.current;
        lastEpochRef.current = epoch;
        const active = activeTurnRef.current;
        setSteeringAvailable(wsRef.current?.canSteer === true && !!active?.turnId && !active.cancelRequested && !active.terminalSeen);
        // The transport calls us before flushing unsent messages. A request
        // queued on this attachment has no outcome in the previous epoch.
        if (active && wsRef.current?.isMessageQueued(active.requestId)) break;
        if (epoch && previous === epoch && active) {
          if (active.cancelRequested && active.turnId) {
            wsRef.current?.cancelTurn(active.requestId, active.turnId);
          }
          wsRef.current?.resumeTurn(active.requestId, active.sequencer.resumeSeq);
        } else if (epoch && previous && previous !== epoch && active) {
          if (!active.terminalSeen) {
            addNotice(t('agent.turn_outcome_unknown'));
          }
          finalizeActiveTurn();
        }
        break;
      }

      case 'turn_steer_ack': {
        const pending = pendingSteerRef.current;
        const active = activeTurnRef.current;
        if (!pending || !active || active.requestId !== pending.parentRequestId
          || active.turnId !== pending.turnId || active.terminalSeen
          || msg.request_id !== pending.requestId || msg.parent_request_id !== pending.parentRequestId
          || msg.turn_id !== pending.turnId || msg.epoch !== pending.epoch) break;
        settleSteering(msg.status === 'accepted' || msg.status === 'duplicate'
          ? t('agent.steer_accepted')
          : t('agent.steer_refused').replace('{status}', msg.status ?? 'unknown'));
        break;
      }

      case 'turn_ack': {
        const active = activeTurnRef.current;
        if (!active || msg.request_id !== active.requestId) break;
        switch (msg.status) {
          case 'accepted':
            bindTurn(active, msg.turn_id);
            break;
          case 'duplicate':
            // The request already ran (or is running) under this identity;
            // reconcile by replaying everything past our applied sequence.
            bindTurn(active, msg.turn_id);
            wsRef.current?.resumeTurn(active.requestId, active.sequencer.resumeSeq);
            break;
          case 'conflict':
            addNotice(t('agent.turn_conflict'));
            finalizeActiveTurn();
            break;
          case 'busy':
            addNotice(t('agent.turn_busy'));
            finalizeActiveTurn();
            break;
          default:
            // `unknown` (and anything unrecognized): the effect of this
            // request identity cannot be determined here — never resend.
            addNotice(t('agent.turn_outcome_unknown'));
            finalizeActiveTurn();
        }
        break;
      }

      case 'turn_event': {
        const active = activeTurnRef.current;
        if (!active || msg.request_id !== active.requestId) break;
        // Late-ACK recovery: events and replays carry the authoritative turn
        // identity even when the accepted ACK was never observed.
        bindTurn(active, msg.turn_id);
        offerTurnEvent(active, msg);
        break;
      }

      case 'turn_replay': {
        const active = activeTurnRef.current;
        if (!active || msg.request_id !== active.requestId) break;
        bindTurn(active, msg.turn_id);
        if (msg.gap) {
          addNotice(t('agent.replay_gap'));
        }
        for (const event of msg.events ?? []) {
          if (activeTurnRef.current !== active) break;
          if (event.type === 'turn_event') {
            offerTurnEvent(active, event);
          } else {
            handleWsMessage(event);
          }
        }
        if (activeTurnRef.current !== active) break;
        // Only after feeding the entire batch can missing events be declared
        // lost. A gap in this snapshot says nothing about later live holes.
        // The replay carried everything the server still holds after our
        // anchor: any seq <= last_seq it did not carry is unrecoverable, so
        // skip those holes and release what is buffered above them in order.
        if (typeof msg.last_seq === 'number') {
          const release = active.sequencer.settleThrough(msg.last_seq);
          // The gap notice was already shown for a declared gap.
          applyRelease(active, msg.gap ? { ...release, skipped: [] } : release);
        }
        if (activeTurnRef.current === active && msg.terminal === true) {
          // Defensive: the replay says the request finished but its marker
          // was not (re)delivered past our anchor — settle anyway.
          applyRelease(active, active.sequencer.flush());
          if (activeTurnRef.current === active) {
            releaseTerminalWaiter(active.turnId ?? '');
            finalizeActiveTurn();
          }
        }
        break;
      }

      case 'turn_cancel_ack': {
        // Correlate to the observed turn: a late ack for an old turn must not
        // rewrite the current turn's Stop state.
        const active = activeTurnRef.current;
        if (!active || msg.request_id !== active.requestId || msg.turn_id !== active.turnId) {
          break;
        }
        if (msg.status === 'requested') {
          // Requested ≠ confirmed: the badge stays until the terminal marker.
          setStopRequested(true);
        } else {
          // stale / no_active / unknown — nothing was cancelled; leave any
          // running turn's UI state to its own frames.
          setStopRequested(false);
        }
        break;
      }

      case 'turn_error': {
        if (msg.op === 'steer') {
          const pending = pendingSteerRef.current;
          if (pending && msg.request_id === pending.requestId && msg.epoch === pending.epoch
            && msg.parent_request_id === pending.parentRequestId && msg.turn_id === pending.turnId) {
            settleSteering(t('agent.steer_refused').replace('{status}', msg.code ?? 'UNKNOWN'));
          }
          break;
        }
        // The error names its operation: an admission failure (`op: message`)
        // means our turn will never start and must settle now; a failed
        // cancel/resume control leaves any running turn to its own frames.
        // Pre-parse errors carry no `op`; they can only precede any admission
        // this client made, so while one is pending (no turn bound yet) they
        // settle it too instead of spinning forever.
        const active = activeTurnRef.current;
        const refused = t('agent.turn_refused').replace('{code}', msg.code ?? 'UNKNOWN');
        const admissionPending = !!active && !active.terminalSeen && active.turnId === null;
        if (
          active
          && !active.terminalSeen
          && (msg.op === 'message' || (msg.op === undefined && admissionPending))
        ) {
          addNotice(refused);
          finalizeActiveTurn();
        } else if (msg.op !== 'message' && msg.op !== undefined && msg.code !== 'EPOCH_MISMATCH') {
          // A cancel/resume control failed; EPOCH_MISMATCH self-heals on
          // reconnect, everything else deserves a visible trace.
          addNotice(refused);
        }
        break;
      }

      case 'turn_gap': {
        const active = activeTurnRef.current;
        addNotice(t('agent.replay_gap'));
        if (active) {
          wsRef.current?.resumeTurn(active.requestId, active.sequencer.resumeSeq);
        }
        break;
      }

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
        // Extract context window info from "done" frame (sent by gateway). See #7311.
        if (msg.type === 'done') {
          if (typeof msg.max_context_tokens === 'number') {
            setContextMaxTokens(msg.max_context_tokens);
          }
          // Prefer last_input_tokens (accurate per-turn prompt size) over
          // accumulated input_tokens for context-bar rendering.
          if (typeof msg.last_input_tokens === 'number') {
            setContextInputTokens(msg.last_input_tokens);
          } else if (typeof msg.input_tokens === 'number') {
            setContextInputTokens(msg.input_tokens);
          }
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
            // Dedup re-delivered frames by gateway tool_call_id. The redacted
            // args must not serve as identity: full credential redaction maps
            // every secret to the same mask, so two parallel calls that differ
            // only in their credential would otherwise collapse into one card.
            // Only id-less legacy frames fall back to the name+args key.
            const isDuplicate = prev.some(
              (m) => m.toolCall
                && m.toolCall.output === undefined
                && m.toolCall.name === toolName
                && (msg.id !== undefined
                  ? m.toolCall.id === msg.id
                  : JSON.stringify(m.toolCall.args ?? {}) === argsKey),
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

      case 'history_trimmed': {
        const reason = msg.reason || t('agent.history_trimmed_unknown_reason');
        const content = t('agent.history_trimmed')
          .replace('{reason}', reason)
          .replace('{dropped}', String(msg.dropped_messages ?? 0))
          .replace('{kept}', String(msg.kept_turns ?? 0));
        localMessageMutationVersionRef.current += 1;
        setMessages((prev) => [
          ...prev,
          {
            id: generateUUID(),
            role: 'agent' as const,
            content,
            timestamp: new Date(),
            ephemeral: true,
            notice: true,
          },
        ]);
        break;
      }

      case 'approval_request': {
        // Supervised-mode tool consent prompt. Backend parks on a oneshot
        // until we send `approval_response` or the original timeout elapses.
        // Reconnect reprojects the same request with only its remaining time.
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
  }, [addNotice, finalizeActiveTurn, releaseTerminalWaiter, settleSteering, settleSideQuery]);

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
      setSteeringAvailable(false);
      settleSteering(t('agent.steer_unknown'));
      clearSideQueries(t('agent.btw_unknown'));

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

    ws.onNegotiationError = (code, unsentRequestIds) => {
      if (version !== wsVersionRef.current) return;
      clearSideQueries(t('agent.btw_unknown'));
      const active = activeTurnRef.current;
      if (active && !unsentRequestIds.includes(active.requestId) && !active.terminalSeen) {
        addNotice(t('agent.turn_outcome_unknown'));
      }
      finalizeActiveTurn();
      setConnected(false);
      setError(t('agent.turn_refused').replace('{code}', code));
    };

    ws.onMessage = (msg: WsMessage) => {
      if (version !== wsVersionRef.current) return;
      handleWsMessage(msg);
    };
  }, [handleWsMessage, addNotice, finalizeActiveTurn, settleSteering, clearSideQueries]);

  // WebSocket bound to the configured agent. Re-keys (via the outer
  // <AgentProvider key={alias}>) when the alias changes.
  useEffect(() => {
    const ws = new WebSocketClient({ agentAlias });
    attachSocketCallbacks(ws);
    ws.connect();
    wsRef.current = ws;

    return () => {
      const pending = pendingSteerRef.current;
      if (pending?.timer) clearTimeout(pending.timer);
      pendingSteerRef.current = null;
      clearSideQueries();
      ws.disconnect();
    };
  }, [attachSocketCallbacks, agentAlias, clearSideQueries]);

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

  const sendMessage = useCallback((content: string, attachments?: string[]) => {
    const question = sideQueryQuestion(content);
    if (question !== null) {
      sendSideQuery(question, attachments);
      return;
    }
    if (!wsRef.current?.connected) return;
    // A new main message must never replace an active turn's event/cancel correlation.
    if (activeTurnRef.current) return;
    try {
      // S2 (#549): admission carries a client-minted request_id; every later
      // turn frame for this bubble correlates against it.
      const requestId = wsRef.current.sendTurnMessage(content, attachments);
      activeTurnRef.current = wsRef.current.isLegacy ? null : {
        requestId,
        turnId: null,
        sequencer: new TurnSequencer<WsMessage>(),
        terminalSeen: false,
        cancelRequested: false,
        holeTimer: null,
        holeTimerStage: 0,
      };
      setStopRequested(false);
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
  }, [sendSideQuery]);

  const steerMessage = useCallback((content: string, attachments?: string[]): boolean => {
    const question = sideQueryQuestion(content);
    if (question !== null) return sendSideQuery(question, attachments);
    const ws = wsRef.current;
    const active = activeTurnRef.current;
    if (!ws?.canSteer || !ws.epoch || !active?.turnId || active.terminalSeen
      || active.cancelRequested || pendingSteerRef.current) return false;
    const epoch = ws.epoch;
    try {
      const requestId = ws.steerTurn(active.requestId, active.turnId, epoch, content, attachments);
      if (!requestId) return false;
      const pending = { requestId, parentRequestId: active.requestId, turnId: active.turnId, epoch, timer: null as ReturnType<typeof setTimeout> | null };
      pendingSteerRef.current = pending;
      pending.timer = setTimeout(() => {
        if (pendingSteerRef.current === pending) settleSteering(t('agent.steer_unknown'));
      }, 5_000);
      setSteeringPending(true);
      localMessageMutationVersionRef.current += 1;
      setMessages((prev) => [...prev, { id: generateUUID(), role: 'user', content, timestamp: new Date(), local: true }]);
      return true;
    } catch {
      setError(t('agent.send_error'));
      return false;
    }
  }, [settleSteering, sendSideQuery]);

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
      // Snapshot the turn to cancel BEFORE any await: a later turn may start
      // while the config write is in flight and must not become the target
      // (#549 async-cancel correlation).
      const switchTarget = activeTurnRef.current;
      // Per-agent switch: write THIS agent's own model_provider ref (multi-agent
      // / schema V3). `model` here is a provider ref (e.g. "kilo.minimax_m3").
      // The global `model`/`default_model` keys were removed in V3.
      await putProp(`agents.${agentAlias}.model_provider`, model);
      // The write-phase watchdog may have fired (or a newer switch may have
      // superseded this one) while the request was in flight. Bail before
      // touching the live socket so we never tear it down after giving up.
      if (pendingModelSwitchRef.current !== model) return;

      // If a turn is actively streaming, cancel it on the backend before we
      // tear down the socket. S2 (#549): bind the cancellation to the observed
      // turn so a late/duplicate control can never reach a newer turn. On a
      // negotiated socket there is no REST fallback (a session-level abort
      // cancels whatever turn is current server-side); the legacy fallback
      // only exists for an explicitly legacy attachment.
      if (typing) {
        try {
          const ws = wsRef.current;
          if (switchTarget && ws?.cancelQueuedMessage(switchTarget.requestId)) {
            if (activeTurnRef.current === switchTarget) finalizeActiveTurn();
          } else if (ws?.isLegacy) {
            await Promise.race([
              abortSession(sessionIdRef.current),
              new Promise((_, reject) =>
                setTimeout(() => reject(new Error('abort-timeout')), 1_500),
              ),
            ]);
          } else if (switchTarget) {
            switchTarget.cancelRequested = true;
            if (switchTarget.turnId) {
              ws?.cancelTurn(switchTarget.requestId, switchTarget.turnId);
              await Promise.race([
                waitForTerminal(switchTarget.turnId, 1_500),
                new Promise((_, reject) =>
                  setTimeout(() => reject(new Error('abort-timeout')), 1_500),
                ),
              ]);
            }
          }
        } catch {
          // Best-effort: if cancel fails or times out we still proceed with the
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
      clearSideQueries(t('agent.btw_unknown'));
      const oldWs = wsRef.current;
      if (oldWs) {
        oldWs.onOpen = null;
        oldWs.onClose = null;
        oldWs.onError = null;
        oldWs.onMessage = null;
        oldWs.onNegotiationError = null;
        oldWs.disconnect();
      }

      const ws = new WebSocketClient({ agentAlias, requireTurnControl: oldWs?.requiresTurnControl });
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
  }, [attachSocketCallbacks, modelLoading, typing, agentAlias, waitForTerminal, finalizeActiveTurn, clearSideQueries]);

  const deleteMessage = useCallback((id: string) => {
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => prev.filter((m) => m.id !== id));
  }, []);

  const clearAllMessages = useCallback(() => {
    // Retire this pane's request/control correlation before rebuilding its
    // attachment. A late receipt or timer cannot bind to the cleared view.
    finalizeActiveTurn();
    clearSideQueries();
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
        clearSideQueries(t('agent.btw_unknown'));
        oldWs.onOpen = null;
        oldWs.onClose = null;
        oldWs.onError = null;
        oldWs.onMessage = null;
        oldWs.onNegotiationError = null;
        oldWs.disconnect();

        const ws = new WebSocketClient({ agentAlias, requireTurnControl: oldWs.requiresTurnControl });
        // Assign wsRef before connect() so a synchronous throw can't strand the
        // page on the old intentionally-closed socket (see switchModel).
        wsRef.current = ws;
        attachSocketCallbacks(ws);
        ws.connect();
      }
    })();
  }, [agentAlias, attachSocketCallbacks, finalizeActiveTurn, clearSideQueries]);

  const addLocalMessage = useCallback((content: string) => {
    localMessageMutationVersionRef.current += 1;
    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'agent',
        content,
        markdown: true,
        timestamp: new Date(),
        ephemeral: true,
      },
    ]);
  }, []);

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
    sendSideQuery,
    steerMessage,
    canSteer: connected && steeringAvailable && !stopRequested,
    steeringPending,
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
    addLocalMessage,
    abortSession: async () => {
      // Clear local approval state immediately — the in-flight request_id
      // belongs to the turn we're cancelling and will be rejected by the
      // backend on a late click anyway. Don't wait for the `aborted` frame
      // to round-trip; the user clicked Stop and expects the UI to follow.
      setPendingApproval(null);
      // S2 (#549): scoped cancellation of the observed turn; "requested" is
      // shown until the terminal marker confirms aborted. Once ANY socket of
      // this client has selected S2 there is deliberately NO fallback —
      // including during a reconnect window (epoch is null there, but a
      // session-level REST abort cancels whatever turn is current server-side
      // and could kill a newer turn). A sent request without a turn id keeps
      // its cancellation intent until an ACK/event binds the scoped target.
      const active = activeTurnRef.current;
      const ws = wsRef.current;
      if (active && ws?.cancelQueuedMessage(active.requestId)) {
        finalizeActiveTurn();
        return;
      }
      if (ws?.isLegacy) {
        try {
          await abortSession(sessionIdRef.current);
        } catch {
          // Best-effort abort
        }
        return;
      }
      if (active) {
        active.cancelRequested = true;
        setStopRequested(true);
        if (active.turnId) ws?.cancelTurn(active.requestId, active.turnId);
      }
    },
    stopRequested,
    pendingApproval,
    respondToApproval,
    // Context window tracking (from "done" WS frames). See #7311.
    contextMaxTokens,
    contextInputTokens,
  };

  return <AgentContext.Provider value={value}>{children}</AgentContext.Provider>;
}
