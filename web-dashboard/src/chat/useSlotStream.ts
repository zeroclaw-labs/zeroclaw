/**
 * `useSlotStream` (M3, US-003).
 *
 * Bridges `POST /api/slots/:id/messages` (SSE) into React state.
 *
 * Why not `EventSource`?
 *   The browser's `EventSource` API does not let callers set request
 *   headers, which means it cannot send the `Authorization: Bearer …`
 *   our gateway requires on token-paired deployments. Rolling our own
 *   reader on top of `fetch` + `ReadableStreamDefaultReader` gives us
 *   header control and an `AbortController` we can wire to a `stop()`
 *   action.
 *
 * Wire format (mirrors `slot_events::chat_delta` in
 * `crates/zeroclaw-gateway/src/slot_events.rs`):
 *
 *   data: {"type":"chat","slot_id":"…","data":{"role":"assistant","content":"…","done":false}}\n\n
 *   data: {"type":"chat","slot_id":"…","data":{"role":"assistant","content":"","done":true}}\n\n
 *
 * The terminal frame has `done: true` and empty content. Tool-call
 * frames (`role:"tool_call"`) are accepted-but-ignored for M3; M4b
 * renders them as approval cards.
 */
import { useCallback, useRef, useState } from "react";
import { getToken } from "@/lib/auth";

export interface ChatMessage {
  /** Stable id used as React key. */
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  /** True while still receiving deltas; flips to false on `done` or stop. */
  streaming?: boolean;
}

interface ChatFrame {
  type: string;
  slot_id?: string;
  data?: {
    role?: string;
    content?: string;
    done?: boolean;
  };
}

export interface UseSlotStream {
  messages: ChatMessage[];
  isStreaming: boolean;
  error: string | null;
  send: (text: string) => Promise<void>;
  stop: () => Promise<void>;
  /**
   * Reset local message buffer (e.g. when switching slots). Does not
   * cancel any in-flight stream — call `stop()` first if needed.
   */
  reset: () => void;
}

export function useSlotStream(slotId: string | undefined): UseSlotStream {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [isStreaming, setIsStreaming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const reset = useCallback(() => {
    setMessages([]);
    setError(null);
  }, []);

  const stop = useCallback(async () => {
    abortRef.current?.abort();
    abortRef.current = null;
    if (!slotId) return;
    try {
      const headers: Record<string, string> = {};
      const token = getToken();
      if (token) headers.Authorization = `Bearer ${token}`;
      await fetch(`/api/slots/${encodeURIComponent(slotId)}/stop`, {
        method: "POST",
        headers,
      });
    } catch {
      // /stop is best-effort — the local AbortController already tore
      // down our stream consumer.
    } finally {
      setIsStreaming(false);
    }
  }, [slotId]);

  const send = useCallback(
    async (text: string) => {
      if (!slotId) {
        setError("No slot selected");
        return;
      }
      const trimmed = text.trim();
      if (trimmed.length === 0) return;

      // Tear down any leftover reader before starting a new turn.
      abortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;

      // Optimistic append: the user's message + a placeholder assistant
      // message that the SSE consumer fills in delta-by-delta.
      const userId = `u_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      const asstId = `a_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      setMessages((prev) => [
        ...prev,
        { id: userId, role: "user", content: trimmed },
        { id: asstId, role: "assistant", content: "", streaming: true },
      ]);
      setIsStreaming(true);
      setError(null);

      const headers: Record<string, string> = {
        "Content-Type": "application/json",
        Accept: "text/event-stream",
      };
      const token = getToken();
      if (token) headers.Authorization = `Bearer ${token}`;

      try {
        const res = await fetch(
          `/api/slots/${encodeURIComponent(slotId)}/messages`,
          {
            method: "POST",
            headers,
            body: JSON.stringify({ content: trimmed }),
            signal: controller.signal,
          },
        );

        if (!res.ok) {
          const errMsg = await safeText(res);
          throw new Error(
            `${res.status} ${res.statusText}${errMsg ? `: ${errMsg}` : ""}`,
          );
        }
        if (!res.body) {
          throw new Error("Response body missing — gateway returned no SSE stream");
        }

        await consumeSseStream(res.body, (frame) => {
          applyFrame(setMessages, asstId, frame);
        });

        // Stream ended (EOS) — finalise the assistant message in case
        // the server didn't send an explicit `done` (rare but possible
        // on transport-level disconnect).
        finaliseAssistant(setMessages, asstId);
      } catch (e) {
        if ((e as Error).name === "AbortError") {
          // Caller invoked `stop()` — leave the partial message in
          // place so users see what was streamed before they aborted.
          finaliseAssistant(setMessages, asstId);
        } else {
          const msg = e instanceof Error ? e.message : String(e);
          setError(msg);
          appendSystemMessage(setMessages, `[stream error] ${msg}`);
          finaliseAssistant(setMessages, asstId);
        }
      } finally {
        if (abortRef.current === controller) abortRef.current = null;
        setIsStreaming(false);
      }
    },
    [slotId],
  );

  return { messages, isStreaming, error, send, stop, reset };
}

/**
 * Consume an SSE stream: read chunks, decode UTF-8, accumulate buffer,
 * split on `\n\n`, parse each event's `data:` lines into JSON, hand the
 * resulting frame to the caller.
 */
async function consumeSseStream(
  body: ReadableStream<Uint8Array>,
  onFrame: (frame: ChatFrame) => void,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new TextDecoder("utf-8");
  let buffer = "";

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });

    let boundary = buffer.indexOf("\n\n");
    while (boundary !== -1) {
      const rawEvent = buffer.slice(0, boundary);
      buffer = buffer.slice(boundary + 2);
      const dataLines: string[] = [];
      for (const line of rawEvent.split("\n")) {
        if (line.startsWith("data:")) {
          dataLines.push(line.slice(5).trimStart());
        }
        // Other SSE fields (`event:`, `id:`, `retry:`) are not used by
        // the slot stream protocol; ignore them silently.
      }
      if (dataLines.length > 0) {
        const payload = dataLines.join("\n");
        try {
          const frame = JSON.parse(payload) as ChatFrame;
          onFrame(frame);
        } catch {
          // Malformed frame — skip rather than tear down the whole
          // stream. The terminal `done` event will still arrive.
        }
      }
      boundary = buffer.indexOf("\n\n");
    }
  }
}

function applyFrame(
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  assistantId: string,
  frame: ChatFrame,
): void {
  if (frame.type !== "chat" || !frame.data) return;
  const role = frame.data.role;
  // M3 only renders assistant content. `thinking` and `tool_call`
  // arrive on the same stream — accept-but-ignore until M4b lands the
  // dedicated UI for them. `user` echoes are not expected here (the
  // user message was appended optimistically by `send()`).
  if (role !== "assistant") return;
  const content = frame.data.content ?? "";
  const done = frame.data.done === true;
  setMessages((prev) =>
    prev.map((m) =>
      m.id === assistantId
        ? {
            ...m,
            content: m.content + content,
            streaming: !done,
          }
        : m,
    ),
  );
}

function finaliseAssistant(
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  assistantId: string,
): void {
  setMessages((prev) =>
    prev.map((m) =>
      m.id === assistantId ? { ...m, streaming: false } : m,
    ),
  );
}

function appendSystemMessage(
  setMessages: React.Dispatch<React.SetStateAction<ChatMessage[]>>,
  content: string,
): void {
  setMessages((prev) => [
    ...prev,
    {
      id: `s_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`,
      role: "system",
      content,
    },
  ]);
}

async function safeText(res: Response): Promise<string> {
  try {
    return await res.text();
  } catch {
    return "";
  }
}
