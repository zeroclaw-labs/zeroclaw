import type { ApprovalDecision, WsMessage } from '../types/api';
import { AUTH_EVENTS, getToken, isReauthPending } from './auth';
import { apiOrigin, basePath } from './basePath';
import { isTauri } from './tauri';

export type WsMessageHandler = (msg: WsMessage) => void;
export type WsOpenHandler = () => void;
export type WsCloseHandler = (ev: CloseEvent) => void;
export type WsErrorHandler = (ev: Event) => void;

export interface WebSocketClientOptions {
  /** Agent alias to bind this socket to (required by the gateway). */
  agentAlias: string;
  /** Conversation to resume or create. The gateway keys persisted history by
   * this id, so the caller owns it (see `lib/chatSessions`) rather than the
   * socket inventing one — that is what lets one agent hold several
   * independent conversations. */
  sessionId: string;
  /** Base URL override. Defaults to current host with ws(s) protocol. */
  baseUrl?: string;
  /** Delay in ms before attempting reconnect. Doubles on each failure up to maxReconnectDelay. */
  reconnectDelay?: number;
  /** Maximum reconnect delay in ms. */
  maxReconnectDelay?: number;
  /** Set to false to disable auto-reconnect, on a timer and after a
   * re-authentication alike: a socket that is one conversation ends for good
   * when it closes. Default true. */
  autoReconnect?: boolean;
}

const DEFAULT_RECONNECT_DELAY = 1000;
const MAX_RECONNECT_DELAY = 30000;

/** The gateway closed a signed-in session's socket: the session expired or was signed out. */
export const WS_CLOSE_SESSION_ENDED = 4401;
/** The gateway closed a signed-in session's socket: its access was removed. */
export const WS_CLOSE_ACCESS_LOST = 4403;
/** The gateway refused a new conversation: the account's open conversations
 * are all busy. The socket waits for the user to try again. */
export const WS_CLOSE_CONVERSATION_LIMIT = 4409;
/** The gateway closed this conversation under the socket. Reconnecting on the
 * same id starts a fresh one, so this reconnects like any other close. */
export const WS_CLOSE_CONVERSATION_CLOSED = 4410;

/**
 * Route a socket close under dashboard sign-in. Returns true when the socket
 * must wait for `AUTH_EVENTS.resumed` instead of reconnecting on a timer:
 *
 * - 4401: the session the socket was opened under ended. The auth layer
 *   re-reads the current session first, since an early re-auth may already
 *   have replaced it, and prompts only if there is none.
 * - 4403: the session lost access to this socket.
 * - any code while a re-auth is pending: every attempt would be refused.
 *
 * Other closes (1001 when the gateway reloads, network drops) reconnect.
 */
export function routeSessionClose(code: number): boolean {
  if (code === WS_CLOSE_SESSION_ENDED) {
    window.dispatchEvent(new Event(AUTH_EVENTS.sessionEnded));
    return true;
  }
  if (code === WS_CLOSE_ACCESS_LOST) {
    window.dispatchEvent(new Event(AUTH_EVENTS.accessLost));
    return true;
  }
  return isReauthPending();
}

export class WebSocketClient {
  private ws: WebSocket | null = null;
  private currentDelay: number;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private intentionallyClosed = false;
  // Stopped by a session close code. Reconnecting would only be refused, so
  // the socket waits for the auth layer to report a usable session again.
  private haltedForAuth = false;

  public onMessage: WsMessageHandler | null = null;
  public onOpen: WsOpenHandler | null = null;
  public onClose: WsCloseHandler | null = null;
  public onError: WsErrorHandler | null = null;

  private readonly agentAlias: string;
  private readonly sessionId: string;
  private readonly baseUrl: string;
  private readonly reconnectDelay: number;
  private readonly maxReconnectDelay: number;
  private readonly autoReconnect: boolean;

  constructor(options: WebSocketClientOptions) {
    this.agentAlias = options.agentAlias;
    this.sessionId = options.sessionId;
    let defaultBase: string;
    if (isTauri() && apiOrigin) {
      // In Tauri, derive ws URL from the gateway origin.
      defaultBase = apiOrigin.replace(/^http/, 'ws');
    } else {
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      defaultBase = `${protocol}//${window.location.host}`;
    }
    this.baseUrl = options.baseUrl ?? defaultBase;
    this.reconnectDelay = options.reconnectDelay ?? DEFAULT_RECONNECT_DELAY;
    this.maxReconnectDelay = options.maxReconnectDelay ?? MAX_RECONNECT_DELAY;
    this.autoReconnect = options.autoReconnect ?? true;
    this.currentDelay = this.reconnectDelay;
  }

  /** Open the WebSocket connection. */
  connect(): void {
    this.intentionallyClosed = false;
    this.haltedForAuth = false;
    this.clearReconnectTimer();
    window.addEventListener(AUTH_EVENTS.resumed, this.handleAuthResumed);

    const token = getToken();
    const params = new URLSearchParams();
    if (token) params.set('token', token);
    params.set('session_id', this.sessionId);
    params.set('agent', this.agentAlias);
    const url = `${this.baseUrl}${basePath}/ws/chat?${params.toString()}`;

    const protocols: string[] = ['zeroclaw.v1'];
    if (token) protocols.push(`bearer.${token}`);
    this.ws = new WebSocket(url, protocols);

    this.ws.onopen = () => {
      this.currentDelay = this.reconnectDelay;
      this.onOpen?.();
    };

    this.ws.onmessage = (ev: MessageEvent) => {
      try {
        const msg = JSON.parse(ev.data) as WsMessage;
        this.onMessage?.(msg);
      } catch {
        // Ignore non-JSON frames
      }
    };

    this.ws.onclose = (ev: CloseEvent) => {
      this.onClose?.(ev);
      if (this.intentionallyClosed) return;
      // Retrying on a timer would only hit the limit again; the owner calls
      // connect() when the user asks to try again.
      if (ev.code === WS_CLOSE_CONVERSATION_LIMIT) return;
      if (routeSessionClose(ev.code)) {
        this.haltedForAuth = true;
        return;
      }
      this.scheduleReconnect();
    };

    this.ws.onerror = (ev: Event) => {
      this.onError?.(ev);
    };
  }

  /** Send a chat message to the agent. */
  sendMessage(content: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('WebSocket is not connected');
    }
    this.ws.send(JSON.stringify({ type: 'message', content }));
  }

  /**
   * Reply to a supervised-mode tool `approval_request`. The backend matches
   * the response by `request_id` and resolves the parked approval oneshot.
   * If the socket is closed the request will auto-deny on the server side
   * after the timeout, so we silently no-op rather than throwing.
   */
  sendApprovalResponse(requestId: string, decision: ApprovalDecision): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;
    this.ws.send(
      JSON.stringify({ type: 'approval_response', request_id: requestId, decision }),
    );
  }

  /** Close the connection without auto-reconnecting. */
  disconnect(): void {
    this.intentionallyClosed = true;
    this.haltedForAuth = false;
    this.clearReconnectTimer();
    window.removeEventListener(AUTH_EVENTS.resumed, this.handleAuthResumed);
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  /** Returns true if the socket is open. */
  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  // ---------------------------------------------------------------------------
  // Reconnection logic
  // ---------------------------------------------------------------------------

  private readonly handleAuthResumed = (): void => {
    if (!this.haltedForAuth || this.intentionallyClosed || !this.autoReconnect) return;
    this.currentDelay = this.reconnectDelay;
    this.connect();
  };

  private scheduleReconnect(): void {
    if (this.intentionallyClosed || !this.autoReconnect) return;

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
