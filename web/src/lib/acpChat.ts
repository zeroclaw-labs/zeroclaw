import type { ApprovalDecision, WsMessage } from '@/types/api';
import {
  AcpError,
  AcpWebSocketClient,
  type AcpNotification,
  type AcpInitializeResult,
  type AcpRequest,
  type AcpSessionNewResult,
  type AcpSessionPromptResult,
  type AcpSessionResumeResult,
  type JsonRpcId,
} from '@/lib/acp';
import {
  AUTH_EVENTS,
  isReauthPending,
} from '@/lib/auth';
import {
  routeSessionClose,
  WS_CLOSE_CONVERSATION_LIMIT,
  type WebSocketClientOptions,
} from '@/lib/ws';

const ACP_SESSION_NOT_FOUND = -32000;
const ACP_PROTOCOL_VERSION = 1;
const ACP_SESSION_LIMIT_REACHED = -32001;
const DEFAULT_RECONNECT_DELAY = 1_000;
const MAX_RECONNECT_DELAY = 30_000;
const DEFAULT_APPROVAL_TIMEOUT_SECS = 120;

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function text(value: unknown): string {
  if (typeof value === 'string') return value;
  const item = record(value);
  if (!item) return '';
  if (typeof item.text === 'string') return item.text;
  if (typeof item.content === 'string') return item.content;
  if (Array.isArray(item.content)) return item.content.map(text).join('');
  return text(item.content);
}

function numberValue(value: unknown): number | undefined {
  return typeof value === 'number' ? value : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined;
}

function sessionIdOf(params: unknown): string | undefined {
  return stringValue(record(params)?.sessionId);
}

export function acpSessionUpdateToWsMessage(params: unknown): WsMessage | null {
  const update = record(record(params)?.update);
  if (!update) return null;
  switch (update.sessionUpdate) {
    case 'agent_message_chunk':
      return { type: 'chunk', content: text(update.content) };
    case 'agent_thought_chunk':
      return { type: 'thinking', content: text(update.content) };
    case 'tool_call':
      return {
        type: 'tool_call',
        id: stringValue(update.toolCallId),
        name: stringValue(update.name) ?? stringValue(update.title),
        args: update.rawInput,
      };
    case 'tool_call_update':
      return {
        type: 'tool_result',
        id: stringValue(update.toolCallId),
        name: stringValue(update.name) ?? stringValue(update.title) ?? 'unknown',
        output: stringValue(update.rawOutput) ?? stringValue(update.body) ?? text(update.content),
      };
    default:
      return null;
  }
}

export function acpNotificationToWsMessage(notification: AcpNotification): WsMessage | null {
  if (notification.method === 'session/update') {
    return acpSessionUpdateToWsMessage(notification.params);
  }
  const params = record(notification.params);
  if (!params) return null;
  if (notification.method === '_zeroclaw/history_trimmed') {
    return {
      type: 'history_trimmed',
      dropped_messages: numberValue(params.droppedMessages),
      dropped_turns: numberValue(params.droppedTurns),
      kept_turns: numberValue(params.keptTurns),
      reason: stringValue(params.reason),
      token_budget: numberValue(params.tokenBudget),
      tokens_before: numberValue(params.tokensBefore),
      tokens_after: numberValue(params.tokensAfter),
      tokens_before_source: stringValue(params.tokensBeforeSource),
      tokens_after_source: stringValue(params.tokensAfterSource),
      unsatisfiable_floor: typeof params.unsatisfiableFloor === 'boolean'
        ? params.unsatisfiableFloor
        : undefined,
    };
  }
  if (notification.method === '_zeroclaw/safeguard_fallback') {
    const kind = params.fallbackKind;
    return {
      type: 'safeguard_fallback',
      requested_model: stringValue(params.requestedModel),
      served_model: stringValue(params.servedModel),
      fallback_kind: kind === 'server' || kind === 'client' || kind === 'client_server'
        ? kind
        : undefined,
    };
  }
  return null;
}

export function acpPromptResultToWsMessage(result: AcpSessionPromptResult): WsMessage {
  const meta = record(record(result._meta)?.zeroclaw) ?? {};
  const message: WsMessage = {
    type: 'done',
    content: result.content,
    full_response: result.content,
    input_tokens: numberValue(meta.inputTokens),
    output_tokens: numberValue(meta.outputTokens),
    max_context_tokens: numberValue(meta.maxContextTokens),
    model_context_window: typeof meta.modelContextWindow === 'number'
      ? meta.modelContextWindow
      : null,
    context_input_tokens: numberValue(meta.contextInputTokens),
    context_input_tokens_source: stringValue(meta.contextInputTokensSource),
  };
  if (Object.prototype.hasOwnProperty.call(meta, 'lastInputTokens')) {
    message.last_input_tokens = typeof meta.lastInputTokens === 'number'
      ? meta.lastInputTokens
      : null;
  }
  return message;
}

interface PendingPermission {
  id: JsonRpcId;
  approval: boolean;
}

export class AcpChatSocket {
  onMessage: ((msg: WsMessage) => void) | null = null;
  onOpen: (() => void) | null = null;
  onClose: ((ev: CloseEvent) => void) | null = null;
  onError: ((ev: Event) => void) | null = null;

  private client: AcpWebSocketClient | null = null;
  private ready = false;
  private intentionallyClosed = false;
  private haltedForAuth = false;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private currentDelay: number;
  private generation = 0;
  private activePrompt = false;
  private interruptedPrompt = false;
  private readonly permissions = new Map<string, PendingPermission>();
  private readonly agentAlias: string;
  private readonly sessionId: string;
  private readonly reconnectDelay: number;
  private readonly maxReconnectDelay: number;
  private readonly autoReconnect: boolean;

  constructor(options: WebSocketClientOptions) {
    this.agentAlias = options.agentAlias;
    this.sessionId = options.sessionId;
    this.reconnectDelay = options.reconnectDelay ?? DEFAULT_RECONNECT_DELAY;
    this.maxReconnectDelay = options.maxReconnectDelay ?? MAX_RECONNECT_DELAY;
    this.autoReconnect = options.autoReconnect ?? true;
    this.currentDelay = this.reconnectDelay;
  }

  connect(): void {
    this.intentionallyClosed = false;
    this.haltedForAuth = false;
    this.clearReconnectTimer();
    window.addEventListener(AUTH_EVENTS.resumed, this.handleAuthResumed);
    const generation = ++this.generation;
    this.ready = false;
    this.activePrompt = false;
    this.permissions.clear();

    let client: AcpWebSocketClient;
    client = new AcpWebSocketClient({
      onOpen: () => {
        if (this.generation !== generation) return;
        this.currentDelay = this.reconnectDelay;
        void this.openSession(client, generation);
      },
      onClose: (event) => {
        if (this.generation !== generation) return;
        this.interruptedPrompt ||= this.activePrompt;
        this.ready = false;
        this.permissions.clear();
        this.onClose?.(event);
        if (this.intentionallyClosed) return;
        if (event.code === WS_CLOSE_CONVERSATION_LIMIT) return;
        if (routeSessionClose(event.code)) {
          this.haltedForAuth = true;
          return;
        }
        this.scheduleReconnect();
      },
      onError: (event) => {
        if (this.generation === generation) this.onError?.(event);
      },
      onNotification: (notification) => {
        if (this.generation !== generation || !this.matchesSession(notification.params)) return;
        if (notification.method === '_zeroclaw/turn_complete') {
          this.activePrompt = false;
          const params = record(notification.params);
          if (params?.error) {
            this.onMessage?.({
              type: 'error',
              code: 'PROVIDER_ERROR',
              message: stringValue(record(params.error)?.message),
            });
          } else if (params?.result) {
            const result = params.result as AcpSessionPromptResult;
            if (result.stopReason === 'cancelled') this.onMessage?.({ type: 'aborted' });
            this.onMessage?.(acpPromptResultToWsMessage(result));
          }
          return;
        }
        if (notification.method === '_zeroclaw/replay_gap') {
          client.disconnect();
          return;
        }
        const message = acpNotificationToWsMessage(notification);
        if (message) this.onMessage?.(message);
      },
      onRequest: (request) => {
        if (this.generation === generation) this.handleRequest(client, request);
      },
    });
    this.client = client;
    client.connect(this.agentAlias);
  }

  disconnect(): void {
    this.intentionallyClosed = true;
    this.haltedForAuth = false;
    this.clearReconnectTimer();
    window.removeEventListener(AUTH_EVENTS.resumed, this.handleAuthResumed);
    this.generation += 1;
    this.ready = false;
    this.permissions.clear();
    this.client?.disconnect();
    this.client = null;
  }

  get connected(): boolean {
    return this.ready && Boolean(this.client?.connected);
  }

  sendMessage(content: string): void {
    const client = this.client;
    if (!client || !this.connected) {
      throw new Error('ACP session is not ready');
    }
    const generation = this.generation;
    if (this.activePrompt) {
      void client.request('_zeroclaw/session/steer', {
        sessionId: this.sessionId,
        content,
      }).catch((error: unknown) => {
        if (this.generation !== generation) return;
        const data = error instanceof AcpError ? record(error.data) : null;
        this.onMessage?.({
          type: 'error',
          code: stringValue(data?.code) ?? 'STEERING_FAILED',
          message: error instanceof Error ? error.message : String(error),
        });
      });
      return;
    }
    this.activePrompt = true;
    void client.request<AcpSessionPromptResult>('session/prompt', {
      sessionId: this.sessionId,
      prompt: content,
    }, 0).then((result) => {
      if (this.generation !== generation) return;
      this.activePrompt = false;
      if (result.stopReason === 'cancelled') this.onMessage?.({ type: 'aborted' });
      this.onMessage?.(acpPromptResultToWsMessage(result));
    }).catch((error: unknown) => {
      if (this.generation !== generation) return;
      this.activePrompt = false;
      if (!client.connected) return;
      const code = error instanceof AcpError && error.code === ACP_SESSION_LIMIT_REACHED
        ? 'CONVERSATION_LIMIT'
        : 'PROVIDER_ERROR';
      this.onMessage?.({
        type: 'error',
        code,
        message: error instanceof Error ? error.message : String(error),
      });
    });
  }

  sendApprovalResponse(requestId: string, decision: ApprovalDecision): void {
    const pending = this.permissions.get(requestId);
    const client = this.client;
    if (!pending || !client?.connected) return;
    this.permissions.delete(requestId);
    if (!pending.approval && decision === 'deny') {
      client.respond(pending.id, { outcome: { outcome: 'cancelled' } });
      return;
    }
    const optionId = pending.approval
      ? decision === 'approve'
        ? 'allow-once'
        : decision === 'always'
          ? 'allow-always'
          : 'reject-once'
      : decision.startsWith('choice:')
        ? decision.slice('choice:'.length)
        : 'reject-once';
    client.respond(pending.id, {
      outcome: { outcome: 'selected', optionId },
    });
  }

  async abortSession(): Promise<void> {
    const client = this.client;
    if (!client?.connected) return;
    await client.request('session/cancel', { sessionId: this.sessionId });
  }

  private async openSession(client: AcpWebSocketClient, generation: number): Promise<void> {
    try {
      const initialized = await client.request<AcpInitializeResult>('initialize', {
        protocolVersion: ACP_PROTOCOL_VERSION,
        clientCapabilities: { elicitation: { form: false } },
      });
      if (initialized.protocolVersion !== ACP_PROTOCOL_VERSION) {
        throw new Error(`Expected ACP protocol version ${ACP_PROTOCOL_VERSION}`);
      }
      let messageCount = 0;
      let resumedExisting = false;
      try {
        const resumed = await client.request<AcpSessionResumeResult>(
          'session/resume',
          { sessionId: this.sessionId },
        );
        messageCount = resumed.messageCount ?? 0;
        resumedExisting = true;
      } catch (error) {
        if (!(error instanceof AcpError) || error.code !== ACP_SESSION_NOT_FOUND) throw error;
        const created = await client.request<AcpSessionNewResult>('session/new', {
          agentAlias: this.agentAlias,
          interactionSurface: 'webchat',
          _meta: { zeroclaw: { sessionId: this.sessionId } },
        });
        if (created.sessionId !== this.sessionId) {
          throw new Error('ACP server did not preserve the requested WebChat session id');
        }
      }
      if (this.generation !== generation) return;
      if (resumedExisting) {
        const attachment = await client.request<{ inFlight: boolean }>('_zeroclaw/session/reattach', {
          sessionId: this.sessionId,
          afterSeq: 0,
          activeOnly: !this.interruptedPrompt,
          acceptPartial: true,
        });
        if (this.generation !== generation) return;
        this.activePrompt = attachment.inFlight;
        this.interruptedPrompt = false;
      }
      this.ready = true;
      this.onOpen?.();
      this.onMessage?.({
        type: 'session_start',
        session_id: this.sessionId,
        resumed: messageCount > 0,
        message_count: messageCount,
      });
    } catch (error) {
      if (this.generation !== generation) return;
      if (error instanceof AcpError && error.code === ACP_SESSION_LIMIT_REACHED) {
        this.onMessage?.({ type: 'error', code: 'CONVERSATION_LIMIT', message: error.message });
        this.generation += 1;
        this.intentionallyClosed = true;
        this.client?.disconnect();
        this.onClose?.({ code: WS_CLOSE_CONVERSATION_LIMIT } as CloseEvent);
        return;
      }
      const data = error instanceof AcpError ? record(error.data) : null;
      this.onMessage?.({
        type: 'error',
        code: stringValue(data?.code) ?? 'AGENT_INIT_FAILED',
        message: error instanceof Error ? error.message : String(error),
      });
      this.client?.disconnect();
    }
  }

  private matchesSession(params: unknown): boolean {
    const sessionId = sessionIdOf(params);
    return sessionId === undefined || sessionId === this.sessionId;
  }

  private handleRequest(client: AcpWebSocketClient, request: AcpRequest): void {
    if (request.method !== 'session/request_permission' || !this.matchesSession(request.params)) {
      client.respondError(request.id, {
        code: -32601,
        message: `Unsupported ACP request: ${request.method}`,
      });
      return;
    }
    const params = record(request.params);
    const toolCall = record(params?.toolCall) ?? {};
    const rawOptions = Array.isArray(params?.options) ? params.options : [];
    const options = rawOptions.map(record).filter((option) => option !== null).map((option) => ({
      optionId: stringValue(option.optionId) ?? '',
      name: stringValue(option.name),
      kind: stringValue(option.kind),
    })).filter((option) => option.optionId);
    const requestId = String(request.id);
    const approval = options.some((option) => option.optionId === 'allow-once');
    this.permissions.set(requestId, { id: request.id, approval });
    this.onMessage?.({
      type: 'approval_request',
      request_id: requestId,
      tool: stringValue(toolCall.name) ?? stringValue(toolCall.title) ?? 'unknown',
      arguments_summary: stringValue(toolCall.argumentsSummary)
        ?? (text(toolCall.content) || JSON.stringify(toolCall.rawInput ?? {})),
      timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
      approval_options: options,
    });
  }

  private readonly handleAuthResumed = (): void => {
    if (!this.haltedForAuth || this.intentionallyClosed || !this.autoReconnect) return;
    this.currentDelay = this.reconnectDelay;
    this.connect();
  };

  private scheduleReconnect(): void {
    if (this.intentionallyClosed || !this.autoReconnect || isReauthPending()) return;
    this.reconnectTimer = setTimeout(() => {
      this.currentDelay = Math.min(this.currentDelay * 2, this.maxReconnectDelay);
      this.connect();
    }, this.currentDelay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }
}
