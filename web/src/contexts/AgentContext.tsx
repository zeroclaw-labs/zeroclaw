import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  useCallback,
  type ReactNode,
} from 'react';
import { WebSocketClient } from '@/lib/ws';
import type { WsMessage } from '@/types/api';
import { generateUUID } from '@/lib/uuid';
import { t } from '@/lib/i18n';
import { getConfig, putConfig } from '@/lib/api';

export interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  thinking?: string;
  markdown?: boolean;
  toolCall?: {
    name: string;
    args?: unknown;
    output?: string;
  };
  timestamp: Date;
}

interface AgentContextType {
  // Connection state
  connected: boolean;
  error: string | null;
  connecting: boolean;

  // Messages
  messages: ChatMessage[];
  typing: boolean;
  streamingContent: string;
  streamingThinking: string;

  // Actions
  sendMessage: (content: string) => void;
  clearMessages: () => void;
  reconnect: () => void;

  // Model switching
  currentModel: string;
  availableModels: string[];
  switchModel: (model: string) => Promise<void>;
  loadingModels: boolean;
}

const AgentContext = createContext<AgentContextType | null>(null);

export function useAgent() {
  const ctx = useContext(AgentContext);
  if (!ctx) {
    throw new Error('useAgent must be used within AgentProvider');
  }
  return ctx;
}

interface Props {
  children: ReactNode;
}

export function AgentProvider({ children }: Props) {
  const wsRef = useRef<WebSocketClient | null>(null);

  // Connection state
  const [connected, setConnected] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Messages state
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [typing, setTyping] = useState(false);
  const [streamingContent, setStreamingContent] = useState('');
  const [streamingThinking, setStreamingThinking] = useState('');

  // Model state
  const [currentModel, setCurrentModel] = useState('');
  const [availableModels, setAvailableModels] = useState<string[]>([]);
  const [loadingModels, setLoadingModels] = useState(false);

  // Refs for streaming
  const pendingContentRef = useRef('');
  const pendingThinkingRef = useRef('');
  const capturedThinkingRef = useRef('');

  // Fetch available models from config
  const fetchModels = useCallback(async () => {
    setLoadingModels(true);
    try {
      const configText = await getConfig();

      // Parse TOML to extract model list
      const modelMatch = configText.match(/models\s*=\s*\[([^\]]+)\]/s);
      if (modelMatch && modelMatch[1]) {
        const modelsStr = modelMatch[1];
        const models = modelsStr
          .split(',')
          .map((m: string) => m.trim().replace(/["']/g, ''))
          .filter((m: string) => m.length > 0);
        setAvailableModels(models);
      }

      // Extract current model
      const currentMatch = configText.match(/model\s*=\s*["']([^"']+)["']/);
      if (currentMatch && currentMatch[1]) {
        setCurrentModel(currentMatch[1]);
      }
    } catch (err) {
      console.error('Failed to fetch models:', err);
    } finally {
      setLoadingModels(false);
    }
  }, []);

  // Switch model
  const switchModel = useCallback(async (model: string) => {
    try {
      const configText = await getConfig();

      // Replace model in config
      const newConfigText = configText.replace(
        /model\s*=\s*["'][^"']+["']/,
        `model = "${model}"`
      );

      // Save config
      await putConfig(newConfigText);

      setCurrentModel(model);

      // Add system message about model switch
      setMessages((prev) => [
        ...prev,
        {
          id: generateUUID(),
          role: 'agent',
          content: `🔄 Model switched to **${model}**`,
          markdown: true,
          timestamp: new Date(),
        },
      ]);
    } catch (err) {
      console.error('Failed to switch model:', err);
      setError(`Failed to switch model: ${err instanceof Error ? err.message : 'Unknown error'}`);
    }
  }, []);

  // Connect WebSocket
  const connect = useCallback(() => {
    if (wsRef.current?.connected) return;

    setConnecting(true);
    setError(null);

    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setConnecting(false);
      setError(null);
    };

    ws.onClose = (ev: CloseEvent) => {
      setConnected(false);
      setConnecting(false);
      if (ev.code !== 1000 && ev.code !== 1001) {
        setError(`Connection closed unexpectedly (code: ${ev.code})`);
      }
    };

    ws.onError = () => {
      setError(t('agent.connection_error'));
      setConnecting(false);
    };

    ws.onMessage = (msg: WsMessage) => {
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
          capturedThinkingRef.current = pendingThinkingRef.current;
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          break;

        case 'message':
        case 'done': {
          const content = msg.full_response ?? msg.content ?? pendingContentRef.current;
          const thinking = capturedThinkingRef.current || pendingThinkingRef.current || undefined;
          if (content) {
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
          const toolName = msg.name ?? 'unknown';
          const toolArgs = msg.args;
          setMessages((prev) => {
            const argsKey = JSON.stringify(toolArgs ?? {});
            const isDuplicate = prev.some(
              (m) =>
                m.toolCall &&
                m.toolCall.output === undefined &&
                m.toolCall.name === toolName &&
                JSON.stringify(m.toolCall.args ?? {}) === argsKey
            );
            if (isDuplicate) return prev;

            return [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent',
                content: `${t('agent.tool_call_prefix')} ${toolName}(${argsKey})`,
                toolCall: { name: toolName, args: toolArgs },
                timestamp: new Date(),
              },
            ];
          });
          break;
        }

        case 'tool_result': {
          setMessages((prev) => {
            const idx = prev.findIndex((m) => m.toolCall && m.toolCall.output === undefined);
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
                role: 'agent',
                content: `${t('agent.tool_result_prefix')} ${msg.output ?? ''}`,
                toolCall: { name: msg.name ?? 'unknown', output: msg.output ?? '' },
                timestamp: new Date(),
              },
            ];
          });
          break;
        }

        case 'cron_result': {
          const cronOutput = msg.output ?? '';
          if (cronOutput) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: 'agent',
                content: cronOutput,
                markdown: true,
                timestamp: new Date(msg.timestamp ?? Date.now()),
              },
            ]);
          }
          break;
        }

        case 'error':
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: 'agent',
              content: `${t('agent.error_prefix')} ${msg.message ?? t('agent.unknown_error')}`,
              timestamp: new Date(),
            },
          ]);
          if (
            msg.code === 'AGENT_INIT_FAILED' ||
            msg.code === 'AUTH_ERROR' ||
            msg.code === 'PROVIDER_ERROR'
          ) {
            setError(`Configuration error: ${msg.message}. Please check your provider settings.`);
          }
          setTyping(false);
          pendingContentRef.current = '';
          pendingThinkingRef.current = '';
          setStreamingContent('');
          setStreamingThinking('');
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;
  }, []);

  // Disconnect WebSocket
  const disconnect = useCallback(() => {
    wsRef.current?.disconnect();
    wsRef.current = null;
    setConnected(false);
  }, []);

  // Send message
  const sendMessage = useCallback((content: string) => {
    if (!wsRef.current?.connected) return;

    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: 'user',
        content,
        timestamp: new Date(),
      },
    ]);

    try {
      wsRef.current.sendMessage(content);
      setTyping(true);
      pendingContentRef.current = '';
      pendingThinkingRef.current = '';
    } catch {
      setError(t('agent.send_error'));
    }
  }, []);

  // Clear messages
  const clearMessages = useCallback(() => {
    setMessages([]);
  }, []);

  // Reconnect
  const reconnect = useCallback(() => {
    disconnect();
    setTimeout(connect, 100);
  }, [disconnect, connect]);

  // Initial connection
  useEffect(() => {
    connect();
    fetchModels();

    return () => {
      // Note: We intentionally don't disconnect on unmount
      // to keep the connection alive across page switches
    };
  }, [connect, fetchModels]);

  const value: AgentContextType = {
    connected,
    error,
    connecting,
    messages,
    typing,
    streamingContent,
    streamingThinking,
    sendMessage,
    clearMessages,
    reconnect,
    currentModel,
    availableModels,
    switchModel,
    loadingModels,
  };

  return <AgentContext.Provider value={value}>{children}</AgentContext.Provider>;
}
