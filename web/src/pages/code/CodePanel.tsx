import { useEffect, useRef, useState, type ReactNode } from "react";
import { X } from "lucide-react";
import { FOCUSABLE_SELECTOR_FORM, useFocusTrap } from "@/hooks/useFocusTrap";
import { t } from "@/lib/i18n";

export interface PanePreference {
  open: boolean;
  width: number;
}

export function useCodeLayout(account: string) {
  const key = JSON.stringify(["zeroclaw-code-layout", account]);
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      // Hidden workspaces remain mounted and keep their measured layout.
      if (element.clientWidth) setWidth(element.clientWidth);
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  const narrow = width ? width < 860 : window.innerWidth < 920;
  const [layout, setLayout] = useState(() => {
    const defaults = { sessions: { open: !narrow, width: 192 }, files: { open: false, width: 0 } };
    try {
      const stored = JSON.parse(localStorage.getItem(key) ?? "null");
      for (const side of ["sessions", "files"] as const) {
        if (typeof stored?.[side]?.open === "boolean" && Number.isFinite(stored[side].width)) {
          defaults[side] = { open: stored[side].open, width: Math.max(0, stored[side].width) };
        }
      }
    } catch { /* Defaults apply when layout storage is unavailable. */ }
    return defaults;
  });
  useEffect(() => {
    try { localStorage.setItem(key, JSON.stringify(layout)); }
    catch { /* Layout storage is optional. */ }
  }, [key, layout]);
  function setPane(side: keyof typeof layout, change: Partial<PanePreference>) {
    setLayout((current) => ({ ...current, [side]: { ...current[side], ...change } }));
  }
  const total = width || window.innerWidth;
  const sessionsMax = Math.max(154, Math.min(320, total - 430 - (layout.files.open ? 276 : 0)));
  const sessionsWidth = Math.max(154, Math.min(sessionsMax, layout.sessions.width));
  const filesMax = Math.max(276, total - 430 - (layout.sessions.open ? sessionsWidth : 0));
  const filesWidth = Math.max(276, Math.min(filesMax, layout.files.width || Math.round(total / 3)));
  return { ref, narrow, layout, setPane, sessionsWidth, sessionsMax, filesWidth, filesMax };
}

export default function CodePanel({
  side, id, label, narrow, visible, width, minimum, maximum, onResize, onClose, actions, children,
}: {
  side: "left" | "right";
  id: string;
  label: string;
  narrow: boolean;
  visible: boolean;
  width: number;
  minimum: number;
  maximum: number;
  onResize: (width: number) => void;
  onClose: () => void;
  actions?: ReactNode;
  children: ReactNode;
}) {
  const ref = useRef<HTMLElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  useFocusTrap(ref, { enabled: narrow && visible, onClose, filterVisible: true,
    focusableSelector: FOCUSABLE_SELECTOR_FORM, preventDefaultOnEscape: true });
  useEffect(() => {
    if (narrow && visible) close.current?.focus();
  }, [narrow, visible]);

  const drag = useRef<{ x: number; width: number } | undefined>(undefined);
  const resize = (value: number) => onResize(Math.max(minimum, Math.min(maximum, value)));
  return (
    <>
      {narrow && (
        <div className="absolute inset-0 z-20 bg-black/40" aria-hidden="true" onClick={onClose} />
      )}
      <aside
        ref={ref}
        id={id}
        role={narrow ? "dialog" : undefined}
        aria-modal={narrow || undefined}
        aria-label={label}
        className={`relative flex min-h-0 shrink-0 flex-col bg-pc-surface ${narrow
          ? `absolute inset-y-0 z-30 shadow-xl ${side === "left" ? "left-0 w-[min(19rem,85%)]" : "right-0 w-full"}`
          : side === "left" ? "border-r border-pc-border" : "border-l border-pc-border"}`}
        style={narrow ? undefined : { width }}
      >
        <div className="flex min-h-12 items-center justify-between gap-1 border-b border-pc-border px-3">
          <h2 className="truncate text-sm font-semibold">{label}</h2>
          <div className="flex shrink-0 items-center gap-1">
            {actions}
            <button type="button" ref={close} onClick={onClose}
              aria-label={t(side === "left" ? "code.close_sessions" : "code.close_files")}
              className="rounded-md p-1.5 text-pc-text-secondary hover:bg-pc-elevated">
              <X className="h-4 w-4" aria-hidden="true" />
            </button>
          </div>
        </div>
        {children}
        {!narrow && (
          <div role="separator" tabIndex={0} aria-controls={id}
            aria-label={t(side === "left" ? "code.resize_sessions" : "code.resize_files")}
            aria-orientation="vertical" aria-valuemin={minimum} aria-valuemax={maximum} aria-valuenow={width}
            className={`group absolute inset-y-0 z-10 w-3 cursor-col-resize touch-none select-none outline-none ${side === "left" ? "-right-1.5" : "-left-1.5"}`}
            onPointerDown={(event) => {
              if (event.button !== 0) return;
              drag.current = { x: event.clientX, width };
              event.currentTarget.setPointerCapture(event.pointerId);
            }}
            onPointerMove={(event) => {
              const start = drag.current;
              if (start) resize(start.width + (event.clientX - start.x) * (side === "left" ? 1 : -1));
            }}
            onLostPointerCapture={() => { drag.current = undefined; }}
            onKeyDown={(event) => {
              const direction = side === "left" ? 1 : -1;
              const values: Record<string, number> = {
                ArrowLeft: width - 16 * direction, ArrowRight: width + 16 * direction,
                Home: minimum, End: maximum,
              };
              const value = values[event.key];
              if (value === undefined) return;
              event.preventDefault();
              resize(value);
            }}>
            <span className="pointer-events-none absolute left-1/2 top-1/2 h-8 w-0.5 -translate-x-1/2 rounded-full bg-pc-text-faint group-hover:bg-pc-accent group-focus-visible:bg-pc-accent" />
          </div>
        )}
      </aside>
    </>
  );
}
