import { useState, useEffect, useRef, useCallback } from 'react';
import { WebSocketClient, type WebSocketClientOptions } from '../lib/ws';
import type { WsMessage } from '../types/api';

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected';

export interface UseWebSocketResult {
  /** Send a chat message to the agent. */
  sendMessage: (content: string) => void;
  /** Array of all messages received during this session. */
  messages: WsMessage[];
  /** Current connection status. */
  status: ConnectionStatus;
  /** Manually connect (called automatically on mount). */
  connect: () => void;
  /** Manually disconnect. */
  disconnect: () => void;
  /** Clear the message history. */
  clearMessages: () => void;
}

export interface UseWebSocketOptions extends WebSocketClientOptions {
  /** If false, do not connect automatically on mount. Default true. */
  autoConnect?: boolean;
}

/**
 * React hook that wraps the WebSocketClient for agent chat.
 *
 * Connects on mount (unless `autoConnect` is false), accumulates incoming
 * messages, and cleans up on unmount.
 */
export function useWebSocket(
  options: UseWebSocketOptions = {},
): UseWebSocketResult {
  const { autoConnect = true, ...wsOptions } = options;

  const clientRef = useRef<WebSocketClient | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>('disconnected');
  const [messages, setMessages] = useState<WsMessage[]>([]);

  // Stable reference to the client across renders
  const getClient = useCallback((): WebSocketClient => {
    if (!clientRef.current) {
      clientRef.current = new WebSocketClient(wsOptions);
    }
    return clientRef.current;
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  // Setup handlers and optionally connect on mount
  useEffect(() => {
    const client = getClient();

    client.onOpen = () => {
      setStatus('connected');
    };

    client.onClose = () => {
      setStatus('disconnected');
    };

    client.onMessage = (msg: WsMessage) => {
      setMessages((prev) => [...prev, msg]);
    };

    client.onError = () => {
      // Status will be set by onClose which fires after onError
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

  const sendMessage = useCallback(
    (content: string) => {
      const client = getClient();
      client.sendMessage(content);
      // Optimistically add the user message to the local list
      setMessages((prev) => [
        ...prev,
        { type: 'message', content } as WsMessage,
      ]);
    },
    [getClient],
  );

  const clearMessages = useCallback(() => {
    setMessages([]);
  }, []);

  return {
    sendMessage,
    messages,
    status,
    connect,
    disconnect,
    clearMessages,
  };
}
