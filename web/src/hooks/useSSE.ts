import { useState, useEffect, useRef, useCallback } from 'react';
import { SSEClient, type SSEClientOptions } from '../lib/sse';
import type { SSEEvent } from '../types/api';

export type SSEConnectionStatus = 'disconnected' | 'connecting' | 'connected';

export interface UseSSEResult {
  /** Array of all events received during this session. */
  events: SSEEvent[];
  /** Current connection status. */
  status: SSEConnectionStatus;
  /** Manually connect (called automatically on mount). */
  connect: () => void;
  /** Manually disconnect. */
  disconnect: () => void;
  /** Clear the event history. */
  clearEvents: () => void;
}

export interface UseSSEOptions extends SSEClientOptions {
  /** If false, do not connect automatically on mount. Default true. */
  autoConnect?: boolean;
  /** Maximum number of events to keep in the buffer. Default 500. */
  maxEvents?: number;
  /** Optional filter: only keep events whose type matches. */
  filterTypes?: string[];
}

/**
 * React hook that wraps the SSEClient for live event streaming.
 *
 * Connects on mount (unless `autoConnect` is false), accumulates incoming
 * events, and cleans up on unmount.
 */
export function useSSE(options: UseSSEOptions = {}): UseSSEResult {
  const {
    autoConnect = true,
    maxEvents = 500,
    filterTypes,
    ...sseOptions
  } = options;

  const clientRef = useRef<SSEClient | null>(null);
  const [status, setStatus] = useState<SSEConnectionStatus>('disconnected');
  const [events, setEvents] = useState<SSEEvent[]>([]);

  // Keep filter in a ref so the callback doesn't need to be recreated
  const filterRef = useRef(filterTypes);
  filterRef.current = filterTypes;

  const maxRef = useRef(maxEvents);
  maxRef.current = maxEvents;

  // Stable reference to the client across renders
  const getClient = useCallback((): SSEClient => {
    if (!clientRef.current) {
      clientRef.current = new SSEClient(sseOptions);
    }
    return clientRef.current;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Setup handlers and optionally connect on mount
  useEffect(() => {
    const client = getClient();

    client.onConnect = () => {
      setStatus('connected');
    };

    client.onEvent = (event: SSEEvent) => {
      // Apply type filter if configured
      if (filterRef.current && filterRef.current.length > 0) {
        if (!filterRef.current.includes(event.type)) return;
      }

      setEvents((prev) => {
        const next = [...prev, event];
        // Trim to max buffer size
        if (next.length > maxRef.current) {
          return next.slice(next.length - maxRef.current);
        }
        return next;
      });
    };

    client.onError = () => {
      setStatus('disconnected');
    };

    if (autoConnect) {
      setStatus('connecting');
      client.connect();
    }

    return () => {
      client.disconnect();
      clientRef.current = null;
    };
  }, [getClient, autoConnect]);

  const connect = useCallback(() => {
    const client = getClient();
    setStatus('connecting');
    client.connect();
  }, [getClient]);

  const disconnect = useCallback(() => {
    const client = getClient();
    client.disconnect();
    setStatus('disconnected');
  }, [getClient]);

  const clearEvents = useCallback(() => {
    setEvents([]);
  }, []);

  return {
    events,
    status,
    connect,
    disconnect,
    clearEvents,
  };
}
