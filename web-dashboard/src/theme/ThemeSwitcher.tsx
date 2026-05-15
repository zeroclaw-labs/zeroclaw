/**
 * ThemeSwitcher (M3, US-004).
 *
 * A small popover trigger that exposes the three named themes
 * (default / monochrome / contrast) and the two modes (light / dark)
 * defined in `web-dashboard/src/index.css`.
 *
 * Closes on outside click and on Esc. The trigger button surfaces the
 * current selection in its `aria-label` so screen readers can read
 * "Theme: default / Mode: dark" without entering the popover.
 */
import { useEffect, useRef, useState } from "react";
import { Palette } from "lucide-react";
import {
  THEMES,
  MODES,
  useTheme,
  type Theme,
  type Mode,
} from "@/theme/useTheme";

export function ThemeSwitcher() {
  const { theme, mode, setTheme, setMode } = useTheme();
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Close on outside click or Escape.
  useEffect(() => {
    if (!open) return;
    const onPointer = (e: PointerEvent) => {
      const node = containerRef.current;
      if (node && !node.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        title="Theme settings"
        aria-label={`Theme: ${theme}, mode: ${mode}`}
        aria-expanded={open}
        aria-haspopup="dialog"
        className="p-1.5 rounded hover:bg-[color:var(--color-surface-muted)]"
      >
        <Palette size={14} aria-hidden="true" />
      </button>
      {open ? (
        <div
          role="dialog"
          aria-label="Theme settings"
          className="absolute right-0 top-full mt-1 z-10 w-44 rounded-md border shadow-md p-2 text-sm"
          style={{
            background: "var(--color-surface)",
            borderColor: "var(--color-border)",
            color: "var(--color-text)",
          }}
        >
          <SettingsGroup label="Theme">
            {THEMES.map((t) => (
              <ChoiceButton
                key={t}
                active={t === theme}
                onClick={() => setTheme(t)}
                testId={`theme-${t}`}
              >
                {labelForTheme(t)}
              </ChoiceButton>
            ))}
          </SettingsGroup>
          <SettingsGroup label="Mode">
            {MODES.map((m) => (
              <ChoiceButton
                key={m}
                active={m === mode}
                onClick={() => setMode(m)}
                testId={`mode-${m}`}
              >
                {labelForMode(m)}
              </ChoiceButton>
            ))}
          </SettingsGroup>
        </div>
      ) : null}
    </div>
  );
}

function SettingsGroup({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div className="mb-2 last:mb-0">
      <div className="text-[10px] uppercase tracking-wider opacity-60 px-1 mb-1">
        {label}
      </div>
      <div className="flex flex-col gap-0.5">{children}</div>
    </div>
  );
}

function ChoiceButton({
  active,
  onClick,
  testId,
  children,
}: {
  active: boolean;
  onClick: () => void;
  testId: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={testId}
      aria-pressed={active}
      className="text-left px-2 py-1 rounded text-xs hover:bg-[color:var(--color-surface-muted)]"
      style={{
        background: active ? "var(--color-surface-muted)" : undefined,
        fontWeight: active ? 600 : 400,
      }}
    >
      {children}
    </button>
  );
}

function labelForTheme(t: Theme): string {
  switch (t) {
    case "default":
      return "Default";
    case "monochrome":
      return "Monochrome";
    case "contrast":
      return "High contrast";
  }
}

function labelForMode(m: Mode): string {
  return m === "light" ? "Light" : "Dark";
}
