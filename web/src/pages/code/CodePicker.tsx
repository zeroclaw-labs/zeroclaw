import { useEffect, useId, useRef, useState } from "react";
import { Check, Search, X } from "lucide-react";
import { t } from "@/lib/i18n";

export interface CodeOption {
  id: string;
  label: string;
  description?: string;
}

export default function CodePicker({
  label, options, selected, onSelect, onClose,
}: {
  label: string;
  options: CodeOption[];
  selected?: string;
  onSelect: (id: string) => void;
  onClose: () => void;
}) {
  const id = useId();
  const ref = useRef<HTMLDialogElement>(null);
  const search = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [index, setIndex] = useState(0);
  const filtered = options.filter(({ label, description }) =>
    `${label} ${description ?? ""}`.toLowerCase().includes(query.trim().toLowerCase()));
  const active = Math.min(index, filtered.length - 1);
  useEffect(() => {
    const dialog = ref.current;
    const trigger = document.activeElement as HTMLElement | null;
    dialog?.showModal();
    search.current?.focus();
    return () => {
      dialog?.close();
      if (trigger?.isConnected) trigger.focus({ preventScroll: true });
    };
  }, []);
  useEffect(() => {
    if (active >= 0) document.getElementById(`${id}-option-${active}`)?.scrollIntoView({ block: "nearest" });
  }, [id, active]);

  return (
    <dialog ref={ref} aria-labelledby={`${id}-title`} onCancel={(event) => {
      event.preventDefault();
      onClose();
    }} className="m-auto w-[min(28rem,calc(100%-2rem))] rounded-xl border border-pc-border bg-pc-surface p-0 text-pc-text shadow-xl backdrop:bg-black/50">
      <div className="flex items-center justify-between border-b border-pc-border px-4 py-3">
        <h2 id={`${id}-title`} className="text-sm font-semibold">{label}</h2>
        <button type="button" onClick={onClose} aria-label={t("common.close")} className="rounded p-1.5 hover:bg-pc-elevated">
          <X className="h-4 w-4" aria-hidden="true" />
        </button>
      </div>
      <label className="mx-3 my-2 flex items-center gap-2 rounded-md bg-pc-elevated px-3 py-2 text-sm">
        <Search className="h-4 w-4 text-pc-text-muted" aria-hidden="true" />
        <input ref={search} type="text" value={query} className="w-full bg-transparent"
          role="combobox" aria-expanded="true" aria-controls={`${id}-options`}
          aria-activedescendant={active >= 0 ? `${id}-option-${active}` : undefined}
          placeholder={t("code.filter_options")} aria-label={t("code.filter_options")}
          onChange={(event) => { setQuery(event.target.value); setIndex(0); }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              setIndex(Math.max(0, Math.min(filtered.length - 1, active + (event.key === "ArrowDown" ? 1 : -1))));
            } else if (event.key === "Enter" && filtered[active]) {
              event.preventDefault();
              onSelect(filtered[active].id);
            }
          }} />
      </label>
      <div id={`${id}-options`} role="listbox" aria-label={label} className="max-h-[45vh] overflow-auto px-2 pb-2">
        {filtered.map((option, offset) => (
          <button key={option.id} type="button" role="option" id={`${id}-option-${offset}`}
            aria-selected={option.id === selected}
            onClick={() => onSelect(option.id)} onMouseEnter={() => setIndex(offset)}
            className={`flex w-full items-center justify-between gap-2 rounded-md px-3 py-2 text-left text-sm ${offset === active ? "bg-pc-elevated" : ""}`}>
            <span className="min-w-0 break-words">{option.label}{option.description && (
              <span className="block text-xs text-pc-text-muted">{option.description}</span>
            )}</span>
            {option.id === selected && <Check className="h-4 w-4 shrink-0 text-pc-accent" aria-hidden="true" />}
          </button>
        ))}
        {!filtered.length && <p className="px-3 py-2 text-sm text-pc-text-muted">{t("nav.cmdk.empty")}</p>}
      </div>
    </dialog>
  );
}
