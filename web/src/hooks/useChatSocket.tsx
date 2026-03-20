import {
  createContext,
  useContext,
  useRef,
  useState,
  useEffect,
  useCallback,
  type ReactNode,
} from "react";
import { WebSocketClient } from "@/lib/ws";
import type { WsMessage } from "@/types/api";
import { generateUUID } from "@/lib/uuid";

// ---------------------------------------------------------------------------
// ChatMessage — serialisable (ISO string timestamps for sessionStorage)
// ---------------------------------------------------------------------------

export interface ChatMessage {
  id: string;
  role: "user" | "agent";
  content: string;
  timestamp: string; // ISO string
}

// ---------------------------------------------------------------------------
// Context shape
// ---------------------------------------------------------------------------

interface ChatSocketContextType {
  messages: ChatMessage[];
  connected: boolean;
  typing: boolean;
  error: string | null;
  sendMessage: (content: string) => void;
  clearMessages: () => void;
}

const ChatSocketContext = createContext<ChatSocketContextType | null>(null);

// ---------------------------------------------------------------------------
// Storage key
// ---------------------------------------------------------------------------

const MESSAGES_KEY = "jhedaiclaw_chat_messages";

function loadMessages(): ChatMessage[] {
  try {
    const raw = sessionStorage.getItem(MESSAGES_KEY);
    return raw ? JSON.parse(raw) : [];
  } catch {
    return [];
  }
}

function saveMessages(msgs: ChatMessage[]) {
  try {
    sessionStorage.setItem(MESSAGES_KEY, JSON.stringify(msgs));
  } catch {
    // sessionStorage full or unavailable
  }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

export function ChatSocketProvider({ children }: { children: ReactNode }) {
  const wsRef = useRef<WebSocketClient | null>(null);
  const pendingContentRef = useRef("");

  const [messages, setMessages] = useState<ChatMessage[]>(loadMessages);
  const [connected, setConnected] = useState(false);
  const [typing, setTyping] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Persist messages to sessionStorage on change
  useEffect(() => {
    saveMessages(messages);
  }, [messages]);

  // WebSocket lifecycle — lives for the lifetime of the provider (App)
  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };
    ws.onClose = () => {
      setConnected(false);
    };
    ws.onError = () => {
      setError("Connection error. Attempting to reconnect...");
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case "chunk":
          setTyping(true);
          pendingContentRef.current += msg.content ?? "";
          break;

        case "message":
        case "done": {
          const content =
            msg.full_response ?? msg.content ?? pendingContentRef.current;
          if (content) {
            setMessages((prev) => [
              ...prev,
              {
                id: generateUUID(),
                role: "agent",
                content,
                timestamp: new Date().toISOString(),
              },
            ]);
          }
          pendingContentRef.current = "";
          setTyping(false);
          break;
        }

        case "tool_call":
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: "agent",
              content: `[Tool Call] ${msg.name ?? "unknown"}(${JSON.stringify(msg.args ?? {})})`,
              timestamp: new Date().toISOString(),
            },
          ]);
          break;

        case "tool_result":
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: "agent",
              content: `[Tool Result] ${msg.output ?? ""}`,
              timestamp: new Date().toISOString(),
            },
          ]);
          break;

        case "error":
          setMessages((prev) => [
            ...prev,
            {
              id: generateUUID(),
              role: "agent",
              content: `[Error] ${msg.message ?? "Unknown error"}`,
              timestamp: new Date().toISOString(),
            },
          ]);
          setTyping(false);
          pendingContentRef.current = "";
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  const sendMessage = useCallback((content: string) => {
    const trimmed = content.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    setMessages((prev) => [
      ...prev,
      {
        id: generateUUID(),
        role: "user",
        content: trimmed,
        timestamp: new Date().toISOString(),
      },
    ]);
    wsRef.current.sendMessage(trimmed);
    setTyping(true);
    pendingContentRef.current = "";
  }, []);

  const clearMessages = useCallback(() => {
    setMessages([]);
    sessionStorage.removeItem(MESSAGES_KEY);
  }, []);

  return (
    <ChatSocketContext.Provider
      value={{ messages, connected, typing, error, sendMessage, clearMessages }}
    >
      {children}
    </ChatSocketContext.Provider>
  );
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

export function useChatSocket(): ChatSocketContextType {
  const ctx = useContext(ChatSocketContext);
  if (!ctx) {
    throw new Error("useChatSocket must be used within <ChatSocketProvider>");
  }
  return ctx;
}
