const STORAGE_KEY_TOKEN = "zeroclaw_token";
const STORAGE_KEY_SERVER = "zeroclaw_server_url";
const STORAGE_KEY_MESSAGES = "zeroclaw_chat_messages";

export interface PairResponse {
  paired: boolean;
  token: string;
}

export interface ChatResponse {
  response: string;
  model: string;
}

export interface HealthResponse {
  status: string;
}

export interface ChatMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  model?: string;
  timestamp: number;
}

function generateId(): string {
  return Date.now().toString(36) + Math.random().toString(36).substring(2, 8);
}

export class ZeroClawClient {
  private serverUrl: string;
  private token: string | null;

  constructor() {
    if (typeof window === "undefined") {
      this.serverUrl =
        process.env.NEXT_PUBLIC_DEFAULT_SERVER_URL || "http://localhost:8080";
      this.token = null;
      return;
    }
    this.serverUrl =
      localStorage.getItem(STORAGE_KEY_SERVER) ||
      process.env.NEXT_PUBLIC_DEFAULT_SERVER_URL ||
      "http://localhost:8080";
    this.token = localStorage.getItem(STORAGE_KEY_TOKEN);
  }

  getServerUrl(): string {
    return this.serverUrl;
  }

  setServerUrl(url: string): void {
    this.serverUrl = url.replace(/\/+$/, "");
    if (typeof window !== "undefined") {
      localStorage.setItem(STORAGE_KEY_SERVER, this.serverUrl);
    }
  }

  getToken(): string | null {
    return this.token;
  }

  isConnected(): boolean {
    return this.token !== null && this.token.length > 0;
  }

  getMaskedToken(): string {
    if (!this.token) return "";
    if (this.token.length <= 8) return "****";
    return (
      this.token.substring(0, 4) +
      "..." +
      this.token.substring(this.token.length - 4)
    );
  }

  disconnect(): void {
    this.disconnectWebSocket();
    this.token = null;
    if (typeof window !== "undefined") {
      localStorage.removeItem(STORAGE_KEY_TOKEN);
    }
  }

  async pair(
    code: string,
    username?: string,
    password?: string,
  ): Promise<PairResponse> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      "X-Pairing-Code": code,
    };

    // Send credentials in body when provided
    const body: Record<string, string> = {};
    if (username) body.username = username;
    if (password) body.password = password;

    const res = await fetch(`${this.serverUrl}/pair`, {
      method: "POST",
      headers,
      body: Object.keys(body).length > 0 ? JSON.stringify(body) : undefined,
    });

    if (!res.ok) {
      const text = await res.text().catch(() => "Unknown error");
      throw new Error(`Pairing failed (${res.status}): ${text}`);
    }

    const data: PairResponse = await res.json();

    if (data.paired && data.token) {
      this.token = data.token;
      if (typeof window !== "undefined") {
        localStorage.setItem(STORAGE_KEY_TOKEN, data.token);
      }
    }

    return data;
  }

  async chat(message: string, model?: string): Promise<ChatResponse> {
    if (!this.token) {
      throw new Error(
        "Not authenticated. Please pair with the server first.",
      );
    }

    const body: Record<string, string> = { message };
    if (model) body.model = model;

    let res: Response;
    try {
      res = await fetch(`${this.serverUrl}/webhook`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${this.token}`,
        },
        body: JSON.stringify(body),
      });
    } catch (err) {
      if (err instanceof TypeError && err.message === "Failed to fetch") {
        throw new Error(
          "Cannot connect to server. Please check your network connection.",
        );
      }
      throw err;
    }

    if (!res.ok) {
      if (res.status === 401) {
        this.disconnect();
        throw new Error(
          "Authentication expired. Please re-pair with the server.",
        );
      }
      const text = await res.text().catch(() => "Unknown error");
      let errorMessage = text;
      try {
        const parsed = JSON.parse(text);
        if (parsed.error) {
          errorMessage = parsed.error;
        }
      } catch {
        // JSON parse failed, use raw text
      }
      throw new Error(errorMessage || `Chat request failed (${res.status})`);
    }

    return await res.json();
  }

  // ── WebSocket streaming chat ─────────────────────────────────

  private ws: WebSocket | null = null;
  private wsSessionId: string | null = null;
  private wsReconnectTimer: ReturnType<typeof setTimeout> | null = null;

  /** Build the WebSocket URL from the current server URL. */
  private getWsUrl(): string {
    const base = this.serverUrl.replace(/^http/, "ws");
    const params = new URLSearchParams();
    // Token sent via Sec-WebSocket-Protocol header, not query param (avoids log leakage)
    if (this.wsSessionId) params.set("session_id", this.wsSessionId);
    const qs = params.toString();
    return qs ? `${base}/ws/chat?${qs}` : `${base}/ws/chat`;
  }

  /** Connect to the /ws/chat WebSocket endpoint. */
  connectWebSocket(callbacks: {
    onChunk?: (text: string) => void;
    onDone?: (fullResponse: string) => void;
    onHistory?: (messages: { role: string; content: string }[]) => void;
    onError?: (error: string) => void;
    onOpen?: () => void;
    onClose?: () => void;
  }): void {
    if (this.ws && this.ws.readyState <= WebSocket.OPEN) {
      return; // Already connected or connecting
    }
    if (!this.token) {
      callbacks.onError?.("Not authenticated");
      return;
    }

    if (!this.wsSessionId) {
      this.wsSessionId = generateId();
    }

    const url = this.getWsUrl();
    // Send token via Sec-WebSocket-Protocol subprotocol to avoid URL/log exposure
    this.ws = new WebSocket(url, [`bearer.${this.token}`]);

    this.ws.onopen = () => {
      callbacks.onOpen?.();
    };

    this.ws.onmessage = (event) => {
      try {
        const msg = JSON.parse(event.data);
        switch (msg.type) {
          case "chunk":
            callbacks.onChunk?.(msg.content || "");
            break;
          case "done":
            callbacks.onDone?.(msg.full_response || "");
            break;
          case "history":
            if (msg.session_id) this.wsSessionId = msg.session_id;
            callbacks.onHistory?.(msg.messages || []);
            break;
          case "error":
            callbacks.onError?.(msg.message || "Unknown error");
            break;
        }
      } catch {
        // Non-JSON frame, ignore
      }
    };

    this.ws.onclose = () => {
      callbacks.onClose?.();
      // Auto-reconnect after 2 seconds
      this.wsReconnectTimer = setTimeout(() => {
        if (this.token) {
          this.connectWebSocket(callbacks);
        }
      }, 2000);
    };

    this.ws.onerror = () => {
      // onerror is always followed by onclose, so reconnect happens there
    };
  }

  /** Send a message over the WebSocket connection. */
  sendWsMessage(message: string): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error("WebSocket not connected");
    }
    this.ws.send(JSON.stringify({ type: "message", content: message }));
  }

  /** Disconnect the WebSocket. */
  disconnectWebSocket(): void {
    if (this.wsReconnectTimer) {
      clearTimeout(this.wsReconnectTimer);
      this.wsReconnectTimer = null;
    }
    if (this.ws) {
      this.ws.onclose = null; // Prevent auto-reconnect
      this.ws.close();
      this.ws = null;
    }
  }

  /** Whether the WebSocket is currently connected. */
  isWsConnected(): boolean {
    return this.ws !== null && this.ws.readyState === WebSocket.OPEN;
  }

  async healthCheck(): Promise<HealthResponse> {
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 5000);

    try {
      const res = await fetch(`${this.serverUrl}/health`, {
        method: "GET",
        signal: controller.signal,
      });

      if (!res.ok) {
        throw new Error(`Health check failed (${res.status})`);
      }

      return await res.json();
    } catch (err) {
      if (err instanceof DOMException && err.name === "AbortError") {
        throw new Error("Health check timed out");
      }
      throw err;
    } finally {
      clearTimeout(timeout);
    }
  }

  // Message history management
  static loadMessages(): ChatMessage[] {
    if (typeof window === "undefined") return [];
    try {
      const stored = localStorage.getItem(STORAGE_KEY_MESSAGES);
      return stored ? JSON.parse(stored) : [];
    } catch {
      return [];
    }
  }

  static saveMessages(messages: ChatMessage[]): void {
    if (typeof window === "undefined") return;
    try {
      localStorage.setItem(STORAGE_KEY_MESSAGES, JSON.stringify(messages));
    } catch {
      // Storage full or unavailable - silently ignore
    }
  }

  static clearMessages(): void {
    if (typeof window === "undefined") return;
    localStorage.removeItem(STORAGE_KEY_MESSAGES);
  }

  static createMessage(
    role: "user" | "assistant",
    content: string,
    model?: string,
  ): ChatMessage {
    return {
      id: generateId(),
      role,
      content,
      model,
      timestamp: Date.now(),
    };
  }
}

// Singleton for client-side use
let clientInstance: ZeroClawClient | null = null;

export function getClient(): ZeroClawClient {
  if (typeof window === "undefined") {
    return new ZeroClawClient();
  }
  if (!clientInstance) {
    clientInstance = new ZeroClawClient();
  }
  return clientInstance;
}

// Simple markdown-to-HTML renderer (no external dependency)
export function renderMarkdown(text: string): string {
  let html = text;

  // Escape HTML
  html = html
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");

  // Code blocks (``` ... ```)
  html = html.replace(
    /```(\w*)\n([\s\S]*?)```/g,
    (_match, _lang, code) => `<pre><code>${code.trim()}</code></pre>`,
  );

  // Inline code
  html = html.replace(
    /`([^`]+)`/g,
    "<code>$1</code>",
  );

  // Bold
  html = html.replace(/\*\*(.+?)\*\*/g, "<strong>$1</strong>");

  // Italic
  html = html.replace(/\*(.+?)\*/g, "<em>$1</em>");

  // Strikethrough
  html = html.replace(/~~(.+?)~~/g, "<del>$1</del>");

  // Headers
  html = html.replace(/^######\s+(.+)$/gm, "<h6>$1</h6>");
  html = html.replace(/^#####\s+(.+)$/gm, "<h5>$1</h5>");
  html = html.replace(/^####\s+(.+)$/gm, "<h4>$1</h4>");
  html = html.replace(/^###\s+(.+)$/gm, "<h3>$1</h3>");
  html = html.replace(/^##\s+(.+)$/gm, "<h2>$1</h2>");
  html = html.replace(/^#\s+(.+)$/gm, "<h1>$1</h1>");

  // Blockquotes
  html = html.replace(/^&gt;\s+(.+)$/gm, "<blockquote>$1</blockquote>");

  // Horizontal rules
  html = html.replace(/^---$/gm, "<hr>");

  // Unordered lists
  html = html.replace(/^[-*]\s+(.+)$/gm, "<li>$1</li>");
  html = html.replace(/((?:<li>.*<\/li>\n?)+)/g, "<ul>$1</ul>");

  // Ordered lists
  html = html.replace(/^\d+\.\s+(.+)$/gm, "<li>$1</li>");

  // Links (sanitize href: escape quotes + block javascript: protocol)
  html = html.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    (_match, label: string, url: string) => {
      const safeUrl = url.replace(/"/g, "&quot;").replace(/'/g, "&#x27;");
      // Block javascript:, data:, vbscript: protocols
      if (/^\s*(javascript|data|vbscript)\s*:/i.test(url.replace(/&amp;/g, "&"))) {
        return label;
      }
      return `<a href="${safeUrl}" target="_blank" rel="noopener noreferrer">${label}</a>`;
    },
  );

  // Paragraphs: wrap non-tag lines
  html = html
    .split("\n\n")
    .map((block) => {
      const trimmed = block.trim();
      if (!trimmed) return "";
      if (
        trimmed.startsWith("<h") ||
        trimmed.startsWith("<ul") ||
        trimmed.startsWith("<ol") ||
        trimmed.startsWith("<pre") ||
        trimmed.startsWith("<blockquote") ||
        trimmed.startsWith("<hr") ||
        trimmed.startsWith("<li")
      ) {
        return trimmed;
      }
      return `<p>${trimmed.replace(/\n/g, "<br>")}</p>`;
    })
    .join("\n");

  return html;
}
