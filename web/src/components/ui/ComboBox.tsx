// Operator Console — ComboBox primitive.
//
// A free-text input with a DISCOVERABLE dropdown of known options. It exists
// because the native `<input list>` / `<datalist>` combo renders no persistent
// affordance (no caret, no obvious "open the list" control) and is unreliable
// on mobile, so users editing e.g. a provider's `model` field never realised a
// list of known models was available. This always shows a caret that opens a
// filtered, keyboard-navigable listbox. Typing filters the list and is accepted
// verbatim (free text), so values not in the list still work.

import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Check, ChevronsUpDown } from "lucide-react";

export interface ComboBoxProps {
  /** Current value. Free text is allowed — not restricted to `options`. */
  value: string;
  /** Fired on each keystroke AND on option select. */
  onChange: (value: string) => void;
  /** Known options to suggest in the dropdown. */
  options: string[];
  id?: string;
  placeholder?: string;
  /** Shown in the dropdown when the typed text matches no option. */
  emptyText?: string;
  /** Extra classes for the root wrapper. */
  className?: string;
  "aria-label"?: string;
}

export function ComboBox({
  value,
  onChange,
  options,
  id,
  placeholder,
  emptyText = "No matches",
  className = "",
  "aria-label": ariaLabel,
}: ComboBoxProps) {
  const [open, setOpen] = useState(false);
  // Highlighted option index (keyboard navigation).
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const reactId = useId();
  const listboxId = `${id ?? reactId}-listbox`;

  // Filter by the current value (case-insensitive substring). When the value
  // exactly equals an option (i.e. just selected), show the full list so the
  // user can still browse / replace it.
  const filtered = useMemo(() => {
    const q = value.trim().toLowerCase();
    if (!q || options.includes(value)) return options;
    return options.filter((o) => o.toLowerCase().includes(q));
  }, [value, options]);

  // Close on outside click.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  // Keep the highlighted option in range and scrolled into view.
  useEffect(() => {
    setActive((a) => Math.min(a, Math.max(0, filtered.length - 1)));
  }, [filtered.length]);
  useEffect(() => {
    if (!open) return;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-cb-index="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  const select = (opt: string) => {
    onChange(opt);
    setOpen(false);
    inputRef.current?.focus();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        return;
      }
      setActive((a) => (filtered.length ? (a + 1) % filtered.length : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (!open) {
        setOpen(true);
        return;
      }
      setActive((a) =>
        filtered.length ? (a - 1 + filtered.length) % filtered.length : 0,
      );
    } else if (e.key === "Enter") {
      const opt = filtered[active];
      if (open && opt) {
        e.preventDefault();
        select(opt);
      }
    } else if (e.key === "Escape") {
      if (open) {
        e.preventDefault();
        setOpen(false);
      }
    }
  };

  return (
    <div ref={rootRef} className={`relative ${className}`}>
      <div className="relative">
        <input
          ref={inputRef}
          id={id}
          role="combobox"
          aria-expanded={open}
          aria-controls={listboxId}
          aria-autocomplete="list"
          aria-label={ariaLabel}
          autoComplete="off"
          spellCheck={false}
          value={value}
          placeholder={placeholder}
          onChange={(e) => {
            onChange(e.target.value);
            setOpen(true);
          }}
          onKeyDown={onKeyDown}
          className="input-electric w-full px-3 py-2 pr-9 text-sm"
        />
        <button
          type="button"
          tabIndex={-1}
          aria-label={open ? "Close list" : "Open list"}
          // mousedown (not click) + preventDefault keeps focus on the input so
          // there's no focus→reopen race when toggling closed.
          onMouseDown={(e) => {
            e.preventDefault();
            setOpen((o) => !o);
            inputRef.current?.focus();
          }}
          className="absolute right-1 top-1/2 -translate-y-1/2 p-1.5 text-pc-text-muted hover:text-pc-text"
        >
          <ChevronsUpDown className="h-4 w-4" />
        </button>
      </div>
      {open && (
        <ul
          ref={listRef}
          id={listboxId}
          role="listbox"
          className="absolute z-30 mt-1 max-h-60 w-full overflow-y-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface p-1 shadow-[var(--pc-shadow-md)]"
        >
          {filtered.length === 0 ? (
            <li className="px-3 py-2 text-xs text-pc-text-muted">{emptyText}</li>
          ) : (
            filtered.map((opt, i) => {
              const sel = opt === value;
              const act = i === active;
              return (
                <li key={opt}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={sel}
                    data-cb-index={i}
                    onMouseMove={() => setActive(i)}
                    onClick={() => select(opt)}
                    className={[
                      "flex w-full items-center gap-2 rounded-[var(--radius-sm)] px-2.5 py-1.5 text-left text-sm transition-colors",
                      act
                        ? "bg-pc-accent/10 text-pc-text"
                        : "text-pc-text-secondary hover:bg-pc-elevated/60",
                    ].join(" ")}
                  >
                    <Check
                      className={`h-3.5 w-3.5 shrink-0 ${sel ? "text-pc-accent" : "opacity-0"}`}
                    />
                    <span className="truncate">{opt}</span>
                  </button>
                </li>
              );
            })
          )}
        </ul>
      )}
    </div>
  );
}
