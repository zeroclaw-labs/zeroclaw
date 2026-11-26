import { getToken } from './auth';
import { apiOrigin, basePath } from './basePath';
import { isTauri } from './tauri';

export type JsonRpcId = number | string;

export interface JsonRpcError {
  code: number;
  message: string;
  data?: unknown;
}

export class AcpError extends Error {
  readonly code: number;
  readonly data?: unknown;

  constructor(error: JsonRpcError) {
    super(error.message);
    this.name = 'AcpError';
    this.code = error.code;
    this.data = error.data;
  }
}

export interface AcpRequest {
  jsonrpc: '2.0';
  method: string;
  params?: unknown;
  id: JsonRpcId;
}

export interface AcpNotification {
  jsonrpc: '2.0';
  method: string;
  params?: unknown;
}

export interface AcpResponse<T = unknown> {
  jsonrpc: '2.0';
  result?: T;
  error?: JsonRpcError;
  id: JsonRpcId;
}

export type AcpFrame = AcpRequest | AcpNotification | AcpResponse;

export type AcpConnectionStatus = 'disconnected' | 'connecting' | 'connected';

export interface AcpInitializeResult {
  protocolVersion?: number;
  agentInfo?: {
    title?: string;
    name?: string;
    version?: string;
  };
  agentCapabilities?: unknown;
  authMethods?: unknown[];
  _meta?: unknown;
}

export interface AcpSessionNewResult {
  sessionId?: string;
  workspaceDir?: string;
}

export interface AcpSessionResumeResult {
  messageCount?: number;
}

export interface AcpSessionPromptResult {
  sessionId?: string;
  stopReason?: string;
  content?: string;
  _meta?: {
    zeroclaw?: Record<string, unknown>;
  };
}

export interface AcpSessionUpdateParams {
  sessionId?: string;
  update?: Record<string, unknown>;
}

export interface AcpPermissionOption {
  optionId: string;
  name?: string;
  kind?: string;
}

export interface AcpClientHandlers {
  onOpen?: () => void;
  onClose?: (event: CloseEvent) => void;
  onError?: (event: Event) => void;
  onNotification?: (message: AcpNotification) => void;
  onRequest?: (message: AcpRequest) => void;
  onFrame?: (message: AcpFrame) => void;
}

interface PendingRequest {
  resolve: (value: unknown) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout> | null;
}

const ACP_PROTOCOL = 'zeroclaw.acp.v1';
const DEFAULT_REQUEST_TIMEOUT_MS = 120_000;

function acpWebSocketBaseUrl(): string {
  if (isTauri() && apiOrigin) {
    return apiOrigin.replace(/^http/, 'ws');
  }
  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${protocol}//${window.location.host}`;
}

function parseFrame(data: string): AcpFrame | null {
  try {
    const parsed = JSON.parse(data) as Partial<AcpFrame>;
    if (parsed && parsed.jsonrpc === '2.0') {
      return parsed as AcpFrame;
    }
  } catch {
    // Ignore malformed frames. The ACP server reports protocol errors for
    // frames it receives; the browser client treats unreadable inbound frames
    // as transport noise.
  }
  return null;
}

export class AcpWebSocketClient {
  private ws: WebSocket | null = null;
  private nextId = 1;
  private pending = new Map<JsonRpcId, PendingRequest>();

  constructor(private readonly handlers: AcpClientHandlers = {}) {}

  connect(agentAlias?: string, resumeSession?: string): void {
    if (this.ws?.readyState === WebSocket.OPEN || this.ws?.readyState === WebSocket.CONNECTING) {
      return;
    }

    const token = getToken();
    const query = new URLSearchParams();
    if (agentAlias) query.set('agent', agentAlias);
    if (resumeSession) query.set('resumeSession', resumeSession);
    const search = query.toString();
    const url = `${acpWebSocketBaseUrl()}${basePath}/acp${search ? `?${search}` : ''}`;
    const protocols = token ? [ACP_PROTOCOL, `bearer.${token}`] : [ACP_PROTOCOL];

    this.ws = new WebSocket(url, protocols);
    this.ws.onopen = () => this.handlers.onOpen?.();
    this.ws.onclose = (event) => {
      this.rejectPending(new Error('ACP WebSocket closed'));
      this.handlers.onClose?.(event);
    };
    this.ws.onerror = (event) => this.handlers.onError?.(event);
    this.ws.onmessage = (event) => {
      if (typeof event.data !== 'string') return;
      const frame = parseFrame(event.data);
      if (!frame) return;

      this.handlers.onFrame?.(frame);
      if ('id' in frame && ('result' in frame || 'error' in frame)) {
        this.resolveResponse(frame as AcpResponse);
      } else if ('id' in frame && 'method' in frame) {
        this.handlers.onRequest?.(frame as AcpRequest);
      } else if ('method' in frame) {
        this.handlers.onNotification?.(frame as AcpNotification);
      }
    };
  }

  request<T = unknown>(
    method: string,
    params?: unknown,
    timeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
  ): Promise<T> {
    if (!this.connected) {
      return Promise.reject(new Error('ACP WebSocket is not connected'));
    }

    const id = this.nextId++;
    const frame: AcpRequest = {
      jsonrpc: '2.0',
      method,
      id,
      ...(params === undefined ? {} : { params }),
    };

    return new Promise<T>((resolve, reject) => {
      const timeout = timeoutMs > 0
        ? setTimeout(() => {
            this.pending.delete(id);
            reject(new Error(`${method} timed out`));
          }, timeoutMs)
        : null;

      this.pending.set(id, {
        resolve: (value) => resolve(value as T),
        reject,
        timeout,
      });
      try {
        this.ws?.send(JSON.stringify(frame));
      } catch (err) {
        if (timeout !== null) clearTimeout(timeout);
        this.pending.delete(id);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  notify(method: string, params?: unknown): void {
    if (!this.connected) {
      throw new Error('ACP WebSocket is not connected');
    }
    const frame: AcpNotification = {
      jsonrpc: '2.0',
      method,
      ...(params === undefined ? {} : { params }),
    };
    this.ws?.send(JSON.stringify(frame));
  }

  respond(id: JsonRpcId, result: unknown): void {
    if (!this.connected) {
      throw new Error('ACP WebSocket is not connected');
    }
    const frame: AcpResponse = {
      jsonrpc: '2.0',
      id,
      result,
    };
    this.ws?.send(JSON.stringify(frame));
  }

  respondError(id: JsonRpcId, error: JsonRpcError): void {
    if (!this.connected) {
      throw new Error('ACP WebSocket is not connected');
    }
    const frame: AcpResponse = {
      jsonrpc: '2.0',
      id,
      error,
    };
    this.ws?.send(JSON.stringify(frame));
  }

  disconnect(): void {
    this.rejectPending(new Error('ACP WebSocket disconnected'));
    this.ws?.close();
    this.ws = null;
  }

  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  private resolveResponse(frame: AcpResponse): void {
    const pending = this.pending.get(frame.id);
    if (!pending) return;
    if (pending.timeout !== null) clearTimeout(pending.timeout);
    this.pending.delete(frame.id);

    if (frame.error) {
      pending.reject(new AcpError(frame.error));
    } else {
      pending.resolve(frame.result);
    }
  }

  private rejectPending(error: Error): void {
    for (const pending of this.pending.values()) {
      if (pending.timeout !== null) clearTimeout(pending.timeout);
      pending.reject(error);
    }
    this.pending.clear();
  }
}
