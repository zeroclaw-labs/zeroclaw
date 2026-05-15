/**
 * ChatView (M3, US-003).
 *
 * Renders a slot's running conversation:
 *  - scrolling message list (user/assistant alternating)
 *  - assistant content rendered via `react-markdown`; user content as
 *    plain text with `whitespace-pre-wrap` so manual line breaks survive
 *  - sticky composer at the bottom: Enter submits, Shift+Enter newlines
 *  - while streaming, the submit button morphs into a Stop button that
 *    aborts the in-flight turn via `POST /api/slots/:id/stop`
 *
 * Slot history rehydration (loading prior turns from
 * `GET /api/sessions/:id/messages`) is deferred to a follow-up — for
 * M3's acceptance criteria a fresh per-tab buffer is sufficient and
 * keeps the wire-up readable.
 */
import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";
import ReactMarkdown from "react-markdown";
import { Send, Square } from "lucide-react";
import { useSlotStream, type ChatMessage } from "@/chat/useSlotStream";

interface ChatViewProps {
  slotId: string;
  /** Display title shown in the header band. */
  title: string;
}

export function ChatView({ slotId, title }: ChatViewProps) {
  const { messages, isStreaming, error, send, stop, reset } =
    useSlotStream(slotId);
  const [draft, setDraft] = useState("");
  const scrollerRef = useRef<HTMLDivElement>(null);

  // Reset the local buffer whenever the user navigates between slots
  // so messages from slot A don't bleed into slot B's view. The hook
  // owns its own AbortController so any in-flight stream is cancelled
  // on unmount via React's cleanup.
  useEffect(() => {
    reset();
  }, [slotId, reset]);

  // Autoscroll to bottom on new messages or while streaming. Use
  // `useLayoutEffect` so the scroll happens before the browser paints
  // the new content — no momentary glimpse of older messages.
  useLayoutEffect(() => {
    const el = scrollerRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [messages]);

  const handleSubmit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (isStreaming) {
      void stop();
      return;
    }
    const text = draft;
    setDraft("");
    void send(text);
  };

  const handleKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      // Synthetically trigger form submit so the button-state branch
      // (send vs stop) runs in one place.
      (e.currentTarget.form as HTMLFormElement | null)?.requestSubmit();
    }
  };

  return (
    <>
      <header
        className="px-4 py-3 border-b text-sm font-medium truncate"
        style={{ borderColor: "var(--color-border)" }}
      >
        {title}
      </header>
      <div
        ref={scrollerRef}
        className="flex-1 overflow-y-auto px-4 py-3"
        data-testid="chat-scroller"
      >
        {messages.length === 0 ? (
          <p className="opacity-60 text-sm text-center mt-8">
            Send a message to start this conversation.
          </p>
        ) : (
          <ul className="flex flex-col gap-3">
            {messages.map((m) => (
              <li key={m.id}>
                <MessageBubble message={m} />
              </li>
            ))}
          </ul>
        )}
        {error ? (
          <p className="text-xs text-red-600 mt-2" role="alert">
            {error}
          </p>
        ) : null}
      </div>
      <form
        onSubmit={handleSubmit}
        className="flex items-end gap-2 border-t p-3"
        style={{ borderColor: "var(--color-border)" }}
      >
        <textarea
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Type a message — Enter to send, Shift+Enter for newline"
          rows={2}
          className="flex-1 resize-none rounded border px-2 py-1 text-sm focus:outline-none bg-transparent"
          style={{
            borderColor: "var(--color-border)",
            color: "var(--color-text)",
          }}
          aria-label="Chat input"
        />
        <button
          type="submit"
          disabled={!isStreaming && draft.trim().length === 0}
          className="inline-flex items-center gap-1 text-sm px-3 py-1.5 rounded border disabled:opacity-50"
          style={{
            borderColor: "var(--color-border)",
            background: isStreaming
              ? "var(--color-surface-muted)"
              : "var(--color-accent)",
            color: isStreaming ? "var(--color-text)" : "var(--color-surface)",
          }}
          aria-label={isStreaming ? "Stop generation" : "Send message"}
        >
          {isStreaming ? (
            <>
              <Square size={12} aria-hidden="true" /> Stop
            </>
          ) : (
            <>
              <Send size={12} aria-hidden="true" /> Send
            </>
          )}
        </button>
      </form>
    </>
  );
}

function MessageBubble({ message }: { message: ChatMessage }) {
  if (message.role === "user") {
    return (
      <div
        className="ml-auto max-w-[80%] rounded px-3 py-2 text-sm whitespace-pre-wrap"
        style={{
          background: "var(--color-surface-muted)",
          color: "var(--color-text)",
        }}
      >
        {message.content}
      </div>
    );
  }
  if (message.role === "system") {
    return (
      <div className="text-xs italic opacity-60" role="status">
        {message.content}
      </div>
    );
  }
  return (
    <div className="max-w-[80%]">
      <div
        className="rounded px-3 py-2 text-sm prose prose-sm max-w-none"
        style={{ color: "var(--color-text)" }}
      >
        {message.content.length === 0 && message.streaming ? (
          <span className="opacity-50">…</span>
        ) : (
          <ReactMarkdown>{message.content}</ReactMarkdown>
        )}
      </div>
    </div>
  );
}
