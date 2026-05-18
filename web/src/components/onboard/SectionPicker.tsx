// Picker view used by /onboard and /config to mirror the TUI's
//   ZeroClaw Onboard › Providers › [filter:_____] › <pickable list>
// flow. Items come from /api/onboard/sections/<section> (gateway derives
// them from list_providers / selectable_memory_backends / schema-walk).
//
// Fuzzy filter is inline (web/src/lib/fuzzy.ts), no npm dep.
//
// Click a row → calls onPick(item.key). Configured rows show a checkmark
// badge so the user can see what they've already set up. Advance/Finish
// buttons are owned by the parent (Onboard / Config) so the picker stays
// a pure list view — no duplicate buttons.

import { useEffect, useMemo, useRef, useState } from 'react';
import { ArrowLeft, Check } from 'lucide-react';
import { fuzzyFilter } from '../../lib/fuzzy';
import {
  ApiError,
  getSectionPicker,
  type PickerItem,
} from '../../lib/api';

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
  const [filter, setFilter] = useState('');
  const [highlightIdx, setHighlightIdx] = useState(0);
  const filterRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    setFilter('');
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
          setError(`Couldn't load picker for ${sectionKey}: ${e instanceof Error ? e.message : String(e)}`);
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
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setHighlightIdx((idx) => Math.min(idx + 1, filtered.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setHighlightIdx((idx) => Math.max(idx - 1, 0));
    } else if (e.key === 'Enter' && filtered[highlightIdx]) {
      e.preventDefault();
      onPick(filtered[highlightIdx]);
    } else if (e.key === 'Escape' && onSkip) {
      e.preventDefault();
      onSkip();
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {help && (
        <p
          className="text-sm"
          style={{ color: 'var(--pc-text-secondary)' }}
        >
          {help}
        </p>
      )}

      {error && (
        <div
          className="rounded-xl border p-3 text-sm animate-fade-in"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      )}

      <div className="relative">
        <input
          ref={filterRef}
          type="text"
          value={filter}
          onChange={(e) => {
            setFilter(e.target.value);
            setHighlightIdx(0);
          }}
          onKeyDown={handleKey}
          placeholder="Filter — fuzzy match. Enter to pick, Esc to skip."
          className="input-electric w-full px-3 py-2.5 text-sm"
        />
      </div>

      <div
        className="surface-panel divide-y overflow-y-auto"
        style={{ borderColor: 'var(--pc-border)', maxHeight: '60vh' }}
      >
        {filtered.length === 0 ? (
          <div
            className="px-4 py-6 text-sm text-center"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            No matches. Try a different filter.
          </div>
        ) : (
          filtered.map((item, idx) => (
            <button
              key={item.key}
              type="button"
              onClick={() => onPick(item)}
              onMouseEnter={() => setHighlightIdx(idx)}
              className="w-full flex items-center justify-between gap-3 px-4 py-2.5 text-left transition-colors"
              style={{
                background:
                  idx === highlightIdx ? 'var(--pc-hover)' : 'transparent',
              }}
            >
              <div className="flex-1 min-w-0">
                <div
                  className="text-sm font-medium"
                  style={{ color: 'var(--pc-text-primary)' }}
                >
                  {item.label}
                  {item.label !== item.key && (
                    <code
                      className="ml-2 text-xs"
                      style={{ color: 'var(--pc-text-faint)' }}
                    >
                      {item.key}
                    </code>
                  )}
                </div>
                {item.description && (
                  <div
                    className="text-xs mt-0.5"
                    style={{ color: 'var(--pc-text-muted)' }}
                  >
                    {item.description}
                  </div>
                )}
              </div>
              {item.badge && (
                <span
                  className="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full"
                  style={{
                    background:
                      item.badge === 'configured' || item.badge === 'active'
                        ? 'rgba(0, 230, 138, 0.12)'
                        : 'var(--pc-bg-elevated)',
                    color:
                      item.badge === 'configured' || item.badge === 'active'
                        ? 'var(--color-status-success)'
                        : 'var(--pc-text-secondary)',
                  }}
                >
                  {(item.badge === 'configured' || item.badge === 'active') && (
                    <Check className="h-3 w-3" />
                  )}
                  {item.badge}
                </span>
              )}
            </button>
          ))
        )}
      </div>

      {onBack && (
        <div>
          <button
            type="button"
            onClick={onBack}
            className="btn-secondary flex items-center gap-2 text-sm px-3 py-2"
          >
            <ArrowLeft className="h-4 w-4" />
            Back
          </button>
        </div>
      )}
    </div>
  );
}
