// Picker view used by /config to mirror the TUI's
//   ZeroClaw Sections › Providers › [filter:_____] › <pickable list>
// flow. Items come from /api/config/sections/<section> (gateway derives
// them from list_providers / selectable_memory_backends / schema-walk).
//
// Fuzzy filter is inline (web/src/lib/fuzzy.ts), no npm dep.
//
// Click a row → calls onPick(item.key). Configured rows show a checkmark
// badge so the user can see what they've already set up. Advance/Finish
// buttons are owned by the parent (Config) so the picker stays
// a pure list view — no duplicate buttons.

import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Check } from "lucide-react";
import { fuzzyFilter } from "../../lib/fuzzy";
import { ApiError, getSectionPicker, type PickerItem } from "../../lib/api";
import { Badge, Button } from "@/components/ui";
import type { BadgeTone } from "@/components/ui";
import { t } from "@/lib/i18n";

interface SectionPickerProps {
  /** Section key, e.g. 'providers'. */
  sectionKey: string;
  /** Help text rendered above the filter input (verbatim from gateway). */
  help: string;
  /** Called when the user picks an item. */
  onPick: (item: PickerItem) => void;
  /** Esc key handler — typically the parent's "advance / next section"
   *  action, so keyboard-only users can skip the picker without picking. */
  onSkip?: () => void;
  /** Optional Back button (wizard: previous section; config: hide). */
  onBack?: () => void;
}

export default function SectionPicker({
  sectionKey,
  help,
  onPick,
  onSkip,
  onBack,
}: SectionPickerProps) {
  const [items, setItems] = useState<PickerItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [highlightIdx, setHighlightIdx] = useState(0);
  const filterRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setFilter("");
    setHighlightIdx(0);
    getSectionPicker(sectionKey)
      .then((resp) => {
        if (cancelled) return;
        setItems(resp.items);
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof ApiError) {
          setError(`[${e.envelope.code}] ${e.envelope.message}`);
        } else {
          setError(
            `${t("section_picker.load_failed_prefix")}${sectionKey}: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [sectionKey]);

  // Refocus the filter input on section change so keyboard-only users can
  // start typing immediately (matches the TUI's auto-focus behavior).
  useEffect(() => {
    filterRef.current?.focus();
  }, [sectionKey]);

  const filtered = useMemo(
    () => fuzzyFilter(items, filter, (i) => `${i.key} ${i.label}`),
    [items, filter],
  );

  const handleKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlightIdx((idx) => Math.min(idx + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlightIdx((idx) => Math.max(idx - 1, 0));
    } else if (e.key === "Enter" && filtered[highlightIdx]) {
      e.preventDefault();
      onPick(filtered[highlightIdx]);
    } else if (e.key === "Escape" && onSkip) {
      e.preventDefault();
      onSkip();
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: "var(--pc-border)",
            borderTopColor: "var(--pc-accent)",
          }}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {help && <p className="text-sm text-pc-text-secondary">{help}</p>}

      {error && (
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error animate-fade-in">
          {error}
        </div>
      )}

      <input
        ref={filterRef}
        type="text"
        value={filter}
        onChange={(e) => {
          setFilter(e.target.value);
          setHighlightIdx(0);
        }}
        onKeyDown={handleKey}
        placeholder={t("section_picker.filter_placeholder")}
        className="w-full px-3 py-2.5 text-sm rounded-[var(--radius-md)] bg-pc-input border border-pc-border text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:border-pc-border-strong focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
      />

      <div
        className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface divide-y divide-pc-border overflow-y-auto"
        style={{ maxHeight: "60vh" }}
      >
        {filtered.length === 0 ? (
          <div className="px-4 py-6 text-sm text-center text-pc-text-muted">
            {t("section_picker.no_matches")}
          </div>
        ) : (
          filtered.map((item, idx) => (
            <button
              key={item.key}
              type="button"
              onClick={() => onPick(item)}
              onMouseEnter={() => setHighlightIdx(idx)}
              className={[
                "w-full flex items-center justify-between gap-3 px-4 py-2.5 text-left transition-colors",
                idx === highlightIdx ? "bg-pc-accent/10" : "bg-transparent",
              ].join(" ")}
            >
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium text-pc-text">
                  {item.label}
                  {item.label !== item.key && (
                    <code className="ml-2 text-xs text-pc-text-faint">
                      {item.key}
                    </code>
                  )}
                </div>
                {item.description && (
                  <div className="text-xs mt-0.5 text-pc-text-muted">
                    {item.description}
                  </div>
                )}
              </div>
              {item.badge && (
                <Badge tone={badgeTone(item.badge)}>
                  {badgeIsGood(item.badge) && <Check className="h-3 w-3" />}
                  {item.badge}
                </Badge>
              )}
            </button>
          ))
        )}
      </div>

      {onBack && (
        <div>
          <Button variant="ghost" size="md" onClick={onBack}>
            <ArrowLeft className="h-4 w-4" />
            {t("common.back")}
          </Button>
        </div>
      )}
    </div>
  );
}

function badgeIsGood(badge: string | undefined): boolean {
  return badge === "configured" || badge === "active" || badge === "set";
}

// Map a schema-driven badge string to a calm Badge tone. The badge text
// itself is rendered verbatim — only the tint is chosen here, so no
// section/option names are hardcoded.
function badgeTone(badge: string): BadgeTone {
  if (badgeIsGood(badge)) return "ok";
  if (badge === "needs setup") return "warn";
  return "neutral";
}
