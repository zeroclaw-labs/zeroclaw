import type { SSEEvent } from '../types/api';
import { getToken } from './auth';

export type SSEEventHandler = (event: SSEEvent) => void;
export type SSEErrorHandler = (error: Event | Error) => void;

export interface SSEClientOptions {
  /** Endpoint path. Defaults to "/api/events". */
  path?: string;
  /** Delay in ms before attempting reconnect. Doubles on each failure up to maxReconnectDelay. */
  reconnectDelay?: number;
  /** Maximum reconnect delay in ms. */
  maxReconnectDelay?: number;
  /** Set to false to disable auto-reconnect. Default true. */
  autoReconnect?: boolean;
}

const DEFAULT_RECONNECT_DELAY = 1000;
const MAX_RECONNECT_DELAY = 30000;

/**
 * SSE client that connects to the ZeroClaw event stream.
 *
 * Because the native EventSource API does not support custom headers, we use
 * the fetch API with a ReadableStream to consume the text/event-stream
 * response, allowing us to pass the Authorization bearer token.
 */
export class SSEClient {
  private controller: AbortController | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private currentDelay: number;
  private intentionallyClosed = false;

  public onEvent: SSEEventHandler | null = null;
  public onError: SSEErrorHandler | null = null;
  public onConnect: (() => void) | null = null;

  private readonly path: string;
  private readonly reconnectDelay: number;
  private readonly maxReconnectDelay: number;
  private readonly autoReconnect: boolean;

  constructor(options: SSEClientOptions = {}) {
    this.path = options.path ?? '/api/events';
    this.reconnectDelay = options.reconnectDelay ?? DEFAULT_RECONNECT_DELAY;
    this.maxReconnectDelay = options.maxReconnectDelay ?? MAX_RECONNECT_DELAY;
    this.autoReconnect = options.autoReconnect ?? true;
    this.currentDelay = this.reconnectDelay;
  }

  /** Start consuming the event stream. */
  connect(): void {
    this.intentionallyClosed = false;
    this.clearReconnectTimer();
    this.controller = new AbortController();

    const token = getToken();
    const headers: Record<string, string> = {
      Accept: 'text/event-stream',
    };
    if (token) {
      headers['Authorization'] = `Bearer ${token}`;
    }

    fetch(this.path, {
      headers,
      signal: this.controller.signal,
    })
      .then((response) => {
        if (!response.ok) {
          throw new Error(`SSE connection failed: ${response.status}`);
        }
        if (!response.body) {
          throw new Error('SSE response has no body');
        }

        this.currentDelay = this.reconnectDelay;
        this.onConnect?.();

        return this.consumeStream(response.body);
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') {
          return; // intentional disconnect
        }
        this.onError?.(err instanceof Error ? err : new Error(String(err)));
        this.scheduleReconnect();
      });
  }

  /** Stop consuming events without auto-reconnecting. */
  disconnect(): void {
    this.intentionallyClosed = true;
    this.clearReconnectTimer();
    if (this.controller) {
      this.controller.abort();
      this.controller = null;
    }
  }

  // ---------------------------------------------------------------------------
  // Stream consumption
  // ---------------------------------------------------------------------------

  private async consumeStream(body: ReadableStream<Uint8Array>): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';

    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });

        // SSE events are separated by double newlines
        const parts = buffer.split('\n\n');
        buffer = parts.pop() ?? '';

        for (const part of parts) {
          this.parseEvent(part);
        }
      }
    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === 'AbortError') {
        return;
      }
      this.onError?.(err instanceof Error ? err : new Error(String(err)));
    } finally {
      reader.releaseLock();
    }

    // Stream ended – schedule reconnect
    this.scheduleReconnect();
  }

  private parseEvent(raw: string): void {
    let eventType = 'message';
    const dataLines: string[] = [];

    for (const line of raw.split('\n')) {
      if (line.startsWith('event:')) {
        eventType = line.slice(6).trim();
      } else if (line.startsWith('data:')) {
        dataLines.push(line.slice(5).trim());
      }
      // Ignore comments (lines starting with ':') and other fields
    }

    if (dataLines.length === 0) return;

    const dataStr = dataLines.join('\n');
    let parsed: SSEEvent;

    try {
      parsed = JSON.parse(dataStr) as SSEEvent;
      parsed.type = parsed.type ?? eventType;
    } catch {
      parsed = { type: eventType, data: dataStr };
    }

    this.onEvent?.(parsed);
  }

  // ---------------------------------------------------------------------------
  // Reconnection logic
  // ---------------------------------------------------------------------------

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
