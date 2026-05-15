// Bridge for the gateway's /ws/chat subscribe-mode endpoint into a
// typed bus React can consume. One socket per tab; exponential-backoff
// reconnect; channel filter mirrors slot_events::event_channel.
// Auth uses ?token= because browsers can't set WS headers.

import { useEffect, useState } from "react";
import { getToken } from "@/lib/auth";
import type { SlotResponse } from "@/chat/slotMutations";

// ── Wire types ──────────────────────────────────────────────────────

export interface SlotsEvent {
  type: "slots";
  data: SlotResponse[];
}

export interface SlotEvent {
  type: "slot";
  slot_id: string;
  data: SlotResponse;
}

export interface ChatEvent {
  type: "chat";
  slot_id: string;
  data: {
    role: "user" | "assistant" | "thinking" | "tool_call" | "tool_result";
    content?: string;
    done?: boolean;
    /** Present on tool_call / tool_result frames. */
    id?: string;
    tool?: string;
    arguments?: unknown;
  };
}

export interface PermissionRequestEvent {
  type: "permission_request";
  slot_id: string;
  data: {
    request_id: string;
    tool_name: string;
    arguments_summary: string;
    timeout_secs: number;
  };
}

export interface ApprovalResponseEvent {
  type: "approval_response";
  slot_id: string;
  data: {
    request_id: string;
    decision: "approve" | "deny" | "always";
  };
}

export type SlotBusEvent =
  | SlotsEvent
  | SlotEvent
  | ChatEvent
  | PermissionRequestEvent
  | ApprovalResponseEvent;

// ── Bus implementation ──────────────────────────────────────────────

type Listener = (event: SlotBusEvent) => void;

interface ListenerEntry {
  /**
   * Channel filter. `null` means "every event regardless of channel" —
   * used by the approval store, which must hear permission events
   * across all slots without enumerating them up-front.
   */
  channels: ReadonlySet<string> | null;
  fn: Listener;
}

const RECONNECT_INITIAL_MS = 250;
const RECONNECT_MAX_MS = 8_000;

export class SlotEventBus {
  private socket: WebSocket | null = null;
  private listeners = new Set<ListenerEntry>();
  private reconnectDelay = RECONNECT_INITIAL_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  // Channels the server has ack'd. Re-set on reconnect so a fresh
  // subscribe frame goes out; lets `flushSubscribe` skip duplicates.
  private serverChannels = new Set<string>();

  constructor(
    /** Override for testing. Defaults to relative `/ws/chat` against
     * the same origin the page was served from. */
    private readonly url: string = defaultWsUrl(),
  ) {}

  /** Open the socket if not already opening/open. Idempotent. */
  connect(): void {
    if (this.closed) return;
    if (this.socket && this.socket.readyState <= WebSocket.OPEN) return;

    const token = getToken();
    const url = token
      ? `${this.url}${this.url.includes("?") ? "&" : "?"}token=${encodeURIComponent(token)}`
      : this.url;

    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch (e) {
      // Construction can throw on malformed URLs (rare in production but
      // possible in test environments). Schedule a backoff and retry.
      console.warn("[slotEvents] WebSocket construction failed:", e);
      this.scheduleReconnect();
      return;
    }
    this.socket = socket;

    socket.addEventListener("open", () => {
      this.reconnectDelay = RECONNECT_INITIAL_MS;
      this.serverChannels = new Set();
      this.flushSubscribe();
    });

    socket.addEventListener("message", (e) => {
      if (typeof e.data !== "string") return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(e.data);
      } catch {
        return;
      }
      if (!isObject(parsed) || typeof parsed.type !== "string") return;
      const t = parsed.type;
      if (t === "subscribed" || t === "unsubscribed") return; // server acks

      if (
        t === "slots" ||
        t === "slot" ||
        t === "chat" ||
        t === "permission_request" ||
        t === "approval_response"
      ) {
        const event = parsed as unknown as SlotBusEvent;
        const channel = channelForEvent(event);
        for (const entry of this.listeners) {
          if (entry.channels === null) entry.fn(event);
          else if (channel !== null && entry.channels.has(channel)) entry.fn(event);
        }
      }
    });

    const onClose = () => {
      this.socket = null;
      if (!this.closed) this.scheduleReconnect();
    };
    socket.addEventListener("close", onClose);
    socket.addEventListener("error", () => {
      // Errors precede a close; let `close` handle the reconnect to
      // avoid double-scheduling.
    });
  }

  /** Permanently shut the bus down. After `close()` no further reconnect
   *  attempts are made and the socket is dropped. */
  close(): void {
    this.closed = true;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.socket) {
      try {
        this.socket.close();
      } catch {
        // Already closed; ignore.
      }
      this.socket = null;
    }
  }

  subscribe(channels: readonly string[], fn: Listener): () => void {
    const entry: ListenerEntry = { channels: new Set(channels), fn };
    this.listeners.add(entry);
    this.connect();
    this.flushSubscribe();
    return () => {
      this.listeners.delete(entry);
    };
  }

  // Cross-cutting listener that hears every event. The bus only
  // dispatches events for channels SOMEONE has subscribed to via
  // `subscribe()`, so all-listeners depend on regular subscribers
  // opening the wire (e.g. sidebar's `slots`, chat view's `chat:<id>`).
  subscribeAll(fn: Listener): () => void {
    const entry: ListenerEntry = { channels: null, fn };
    this.listeners.add(entry);
    this.connect();
    return () => {
      this.listeners.delete(entry);
    };
  }

  private scheduleReconnect(): void {
    if (this.closed) return;
    if (this.reconnectTimer !== null) return;
    const delay = this.reconnectDelay;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this.connect();
    }, delay);
    this.reconnectDelay = Math.min(delay * 2, RECONNECT_MAX_MS);
  }

  private flushSubscribe(): void {
    if (!this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    const wanted = new Set<string>();
    for (const entry of this.listeners) {
      if (entry.channels === null) continue; // all-listener doesn't request channels
      for (const c of entry.channels) wanted.add(c);
    }
    const newChannels = [...wanted].filter((c) => !this.serverChannels.has(c));
    if (newChannels.length === 0) return;
    const frame = JSON.stringify({ type: "subscribe", channels: newChannels });
    try {
      this.socket.send(frame);
      for (const c of newChannels) this.serverChannels.add(c);
    } catch (e) {
      // If send fails the close handler will reconnect; nothing to do.
      console.warn("[slotEvents] subscribe send failed:", e);
    }
  }
}

// ── Module-level singleton ───────────────────────────────────────────

let singleton: SlotEventBus | null = null;

export function getSlotEventBus(): SlotEventBus {
  if (singleton === null) singleton = new SlotEventBus();
  return singleton;
}

// Test-only: dev/test bundles expose `window.__slotBusInject(event)` so
// Playwright can fan a synthetic event into every active listener
// without opening a real WebSocket. Production bundles skip this.
if (
  typeof window !== "undefined" &&
  import.meta.env.MODE !== "production"
) {
  (window as unknown as { __slotBusInject?: (e: SlotBusEvent) => void }).__slotBusInject =
    (event: SlotBusEvent) => {
      const bus = getSlotEventBus() as unknown as {
        listeners: Set<{
          channels: ReadonlySet<string> | null;
          fn: (e: SlotBusEvent) => void;
        }>;
      };
      const channel = channelForEvent(event);
      for (const entry of bus.listeners) {
        if (entry.channels === null) entry.fn(event);
        else if (channel !== null && entry.channels.has(channel)) entry.fn(event);
      }
    };
}

// ── Hook ────────────────────────────────────────────────────────────

export interface UseSlotEventsOptions {
  /** Channel set, e.g. `"slots"`, `"dashboard"`, or `"chat:<slot_id>"`. */
  channels: readonly string[];
  onEvent: (event: SlotBusEvent) => void;
  enabled?: boolean;
}

export function useSlotEvents({
  channels,
  onEvent,
  enabled = true,
}: UseSlotEventsOptions): void {
  // Latest-callback ref so re-renders don't tear down the subscription.
  const [latest] = useState<{ fn: (e: SlotBusEvent) => void }>(() => ({
    fn: onEvent,
  }));
  latest.fn = onEvent;

  // Re-subscribe only when the channel set changes by content.
  const channelKey = enabled ? [...channels].sort().join("|") : "";

  useEffect(() => {
    if (!enabled || channelKey.length === 0) return;
    const bus = getSlotEventBus();
    return bus.subscribe(channelKey.split("|"), (e) => latest.fn(e));
  }, [channelKey, enabled, latest]);
}

// ── Helpers ─────────────────────────────────────────────────────────

function channelForEvent(event: SlotBusEvent): string | null {
  switch (event.type) {
    case "slots":
    case "slot":
      return "slots";
    case "chat":
    case "permission_request":
    case "approval_response":
      return event.slot_id ? `chat:${event.slot_id}` : null;
    default:
      return null;
  }
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

function defaultWsUrl(): string {
  if (typeof window === "undefined") return "ws://localhost/ws/chat";
  const proto = window.location.protocol === "https:" ? "wss" : "ws";
  // Honour Vite's `base` so prod (`/dashboard/`) routes to the right path.
  const baseEl = document.querySelector("base");
  const base = (baseEl?.getAttribute("href") ?? "/").replace(/\/$/, "");
  return `${proto}://${window.location.host}${base}/ws/chat`;
}
