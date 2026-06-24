// Operator Console — Select primitive.
//
// A themed, LOCKED dropdown for fixed value sets (e.g. enum variants). Unlike
// ComboBox (free-text input + discoverable suggestions), the value is restricted
// to the provided options — there is no typing. It exists so locked selects stop
// rendering the browser's unstyled native `<option>` list (which ignores the
// dark theme); this renders the same themed, keyboard-navigable listbox the
// ComboBox uses. Click anywhere on the control opens it.

import { useEffect, useId, useRef, useState } from "react";
import { Check, ChevronsUpDown } from "lucide-react";

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectProps {
  /** Current value (must match an option's `value`, or be empty). */
  value: string;
  onChange: (value: string) => void;
  options: SelectOption[];
  id?: string;
  /** Shown when no option is selected. */
  placeholder?: string;
  /** Extra classes for the root wrapper. */
  className?: string;
  "aria-label"?: string;
}

export function Select({
  value,
  onChange,
  options,
  id,
  placeholder,
  className = "",
  "aria-label": ariaLabel,
}: SelectProps) {
  const [open, setOpen] = useState(false);
  // Highlighted option index (keyboard navigation).
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const btnRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  const reactId = useId();
  const listboxId = `${id ?? reactId}-listbox`;

  const selected = options.find((o) => o.value === value);

  // On open, highlight the currently-selected option (or the first).
  useEffect(() => {
    if (!open) return;
    const idx = options.findIndex((o) => o.value === value);
    setActive(idx >= 0 ? idx : 0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

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

  // Keep the highlighted option scrolled into view.
  useEffect(() => {
    if (!open) return;
    listRef.current
      ?.querySelector<HTMLElement>(`[data-idx="${active}"]`)
      ?.scrollIntoView({ block: "nearest" });
  }, [active, open]);

  const choose = (v: string) => {
    onChange(v);
    setOpen(false);
    btnRef.current?.focus();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (!open) return setOpen(true);
      setActive((a) => (options.length ? (a + 1) % options.length : 0));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (!open) return setOpen(true);
      setActive((a) =>
        options.length ? (a - 1 + options.length) % options.length : 0,
      );
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      if (!open) return setOpen(true);
      const opt = options[active];
      if (opt) choose(opt.value);
    } else if (e.key === "Escape") {
      if (open) {
        e.preventDefault();
        setOpen(false);
      }
    }
  };

  return (
    <div ref={rootRef} className={`relative ${className}`}>
      <button
        ref={btnRef}
        id={id}
        type="button"
        role="combobox"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={listboxId}
        aria-label={ariaLabel}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={onKeyDown}
        className="input-electric flex w-full cursor-pointer items-center justify-between gap-2 px-3 py-2 text-left text-sm"
      >
        <span className={selected ? "truncate" : "truncate text-pc-text-faint"}>
          {selected ? selected.label : (placeholder ?? "")}
        </span>
        <ChevronsUpDown className="h-4 w-4 shrink-0 text-pc-text-muted" />
      </button>
      {open && (
        <ul
          ref={listRef}
          id={listboxId}
          role="listbox"
          className="absolute z-30 mt-1 max-h-60 w-full overflow-y-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface p-1 shadow-[var(--pc-shadow-md)]"
        >
          {options.map((o, i) => {
            const sel = o.value === value;
            const act = i === active;
            return (
              <li key={`${i}-${o.value}`}>
                <button
                  type="button"
                  role="option"
                  aria-selected={sel}
                  data-idx={i}
                  onMouseMove={() => setActive(i)}
                  onClick={() => choose(o.value)}
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
                  <span className="truncate">{o.label}</span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
