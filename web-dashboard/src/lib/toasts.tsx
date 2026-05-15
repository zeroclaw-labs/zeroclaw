// Global toast host. Approval toasts auto-fire on permission_request
// events for slots that aren't the active route, and auto-dismiss when
// the matching approval_response lands. Mounts inside the router so
// <Link> works but outside the route switch so toasts persist across
// navigation. Max 4 visible; oldest evicted.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Link, useLocation } from "react-router-dom";
import { Bell, X, AlertTriangle, Info } from "lucide-react";
import {
  getSlotEventBus,
  useSlotEvents,
  type SlotBusEvent,
} from "@/lib/slotEvents";
import { useSlotsQuery } from "@/chat/slotsQuery";

export type ToastKind = "approval" | "error" | "info";

export interface ToastApprovalLink {
  slot_id: string;
  request_id: string;
  tool_name: string;
}

export interface Toast {
  id: string;
  kind: ToastKind;
  title: string;
  body?: string;
  /** When set, an Open button navigates to this path. */
  link?: { to: string; label: string };
  /** Bus correlation: approval toasts auto-dismiss when the matching
   *  approval_response arrives. */
  approval?: ToastApprovalLink;
}

interface ToastContextValue {
  toasts: Toast[];
  push: (toast: Omit<Toast, "id">) => string;
  dismiss: (id: string) => void;
}

const ToastContext = createContext<ToastContextValue | null>(null);

const MAX_VISIBLE = 4;
const APPROVAL_TIMEOUT_MS = 8_000;
const INFO_TIMEOUT_MS = 8_000;
const ERROR_TIMEOUT_MS = 12_000;

interface ToastProviderProps {
  children: ReactNode;
}

export function ToastProvider({ children }: ToastProviderProps) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timerMap = useRef(new Map<string, ReturnType<typeof setTimeout>>());

  const dismiss = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timerMap.current.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      timerMap.current.delete(id);
    }
  }, []);

  const push = useCallback(
    (input: Omit<Toast, "id">) => {
      const id = `toast_${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
      const toast: Toast = { ...input, id };
      setToasts((prev) => {
        const next = [...prev, toast];
        while (next.length > MAX_VISIBLE) {
          const evicted = next.shift();
          if (evicted) {
            const t = timerMap.current.get(evicted.id);
            if (t !== undefined) {
              clearTimeout(t);
              timerMap.current.delete(evicted.id);
            }
          }
        }
        return next;
      });
      const ms =
        input.kind === "error"
          ? ERROR_TIMEOUT_MS
          : input.kind === "approval"
            ? APPROVAL_TIMEOUT_MS
            : INFO_TIMEOUT_MS;
      const timer = setTimeout(() => dismiss(id), ms);
      timerMap.current.set(id, timer);
      return id;
    },
    [dismiss],
  );

  // Auto-dismiss approval toasts when the matching response lands.
  useEffect(() => {
    const bus = getSlotEventBus();
    const handler = (event: SlotBusEvent) => {
      if (event.type !== "approval_response") return;
      setToasts((prev) =>
        prev.filter(
          (t) =>
            !(
              t.kind === "approval" &&
              t.approval?.slot_id === event.slot_id &&
              t.approval.request_id === event.data.request_id
            ),
        ),
      );
    };
    return bus.subscribeAll(handler);
  }, []);

  // Cleanup timers on unmount.
  useEffect(() => {
    const timers = timerMap.current;
    return () => {
      for (const t of timers.values()) clearTimeout(t);
      timers.clear();
    };
  }, []);

  const value = useMemo(
    () => ({ toasts, push, dismiss }),
    [toasts, push, dismiss],
  );
  return (
    <ToastContext.Provider value={value}>{children}</ToastContext.Provider>
  );
}

export function useToast() {
  const ctx = useContext(ToastContext);
  if (ctx === null) {
    throw new Error("useToast must be used within <ToastProvider>");
  }
  return ctx;
}

export function ToastHost() {
  const { toasts, push, dismiss } = useToast();
  const location = useLocation();
  // Latest-path ref keeps the bus listener identity stable.
  const locationRef = useRef(location.pathname);
  locationRef.current = location.pathname;

  // The bus only delivers events for channels someone has subscribed
  // to. Without this hook, toasts would only fire for slots whose
  // `chat:<id>` channel is currently open by another component (i.e.
  // the active chat view). Open `slots` plus `chat:<id>` for every
  // cached slot here so approvals for ANY slot reach the toast host
  // regardless of route. The handler is a no-op — the auto-toast
  // listener below uses `subscribeAll`.
  const { data: slotsData } = useSlotsQuery();
  const channels = [
    "slots",
    ...(slotsData?.slots.map((s) => `chat:${s.id}`) ?? []),
  ];
  useSlotEvents({ channels, onEvent: () => {} });

  useEffect(() => {
    const bus = getSlotEventBus();
    return bus.subscribeAll((event) => {
      if (event.type !== "permission_request") return;
      const path = locationRef.current;
      const isActiveSlot = path.startsWith(
        `/chat/${encodeURIComponent(event.slot_id)}`,
      );
      if (isActiveSlot) return;
      push({
        kind: "approval",
        title: "Tool approval needed",
        body: `${event.data.tool_name} on slot ${event.slot_id}`,
        link: {
          to: `/chat/${encodeURIComponent(event.slot_id)}`,
          label: "Open",
        },
        approval: {
          slot_id: event.slot_id,
          request_id: event.data.request_id,
          tool_name: event.data.tool_name,
        },
      });
    });
  }, [push]);

  // Split for ARIA: assertive for errors, polite for info/approval.
  const errors = toasts.filter((t) => t.kind === "error");
  const others = toasts.filter((t) => t.kind !== "error");

  return (
    <>
      <ToastRegion
        ariaLive="assertive"
        toasts={errors}
        onDismiss={dismiss}
      />
      <ToastRegion ariaLive="polite" toasts={others} onDismiss={dismiss} />
    </>
  );
}

interface ToastRegionProps {
  toasts: Toast[];
  ariaLive: "polite" | "assertive";
  onDismiss: (id: string) => void;
}

function ToastRegion({ toasts, ariaLive, onDismiss }: ToastRegionProps) {
  if (toasts.length === 0) return null;
  return (
    <div
      aria-live={ariaLive}
      aria-atomic="false"
      className="fixed bottom-4 right-4 z-50 flex flex-col gap-2 max-w-sm"
      style={{ pointerEvents: "none" }}
    >
      {toasts.map((toast) => (
        <ToastItem key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function ToastItem({
  toast,
  onDismiss,
}: {
  toast: Toast;
  onDismiss: (id: string) => void;
}) {
  const Icon =
    toast.kind === "error"
      ? AlertTriangle
      : toast.kind === "approval"
        ? Bell
        : Info;
  return (
    <div
      role={toast.kind === "error" ? "alert" : "status"}
      className="rounded border px-3 py-2 text-sm shadow"
      style={{
        background: "var(--color-surface)",
        borderColor: "var(--color-border)",
        color: "var(--color-text)",
        pointerEvents: "auto",
      }}
    >
      <div className="flex items-start gap-2">
        <Icon
          size={14}
          aria-hidden="true"
          className="mt-0.5"
          style={{
            color:
              toast.kind === "error"
                ? "var(--color-text)"
                : "var(--color-accent)",
          }}
        />
        <div className="flex-1 min-w-0">
          <div className="font-medium truncate">{toast.title}</div>
          {toast.body ? (
            <div
              className="text-xs mt-0.5"
              style={{ color: "var(--color-text-muted)" }}
            >
              {toast.body}
            </div>
          ) : null}
          {toast.link ? (
            <Link
              to={toast.link.to}
              onClick={() => onDismiss(toast.id)}
              className="text-xs underline mt-1 inline-block"
              style={{ color: "var(--color-accent)" }}
            >
              {toast.link.label}
            </Link>
          ) : null}
        </div>
        <button
          type="button"
          onClick={() => onDismiss(toast.id)}
          aria-label="Dismiss notification"
          className="p-0.5 rounded hover:bg-[color:var(--color-surface-muted)]"
        >
          <X size={12} aria-hidden="true" />
        </button>
      </div>
    </div>
  );
}
