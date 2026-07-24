import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  Activity,
  Bot,
  Clock,
  CornerDownLeft,
  FolderTree,
  LayoutDashboard,
  MessageSquare,
  Monitor,
  Puzzle,
  Search,
  Settings,
  SlidersHorizontal,
  Sparkles,
  Stethoscope,
  Terminal,
  Wrench,
} from 'lucide-react';
import { t } from '@/lib/i18n';
import { loadConfigSearchItems, type ConfigSearchItem } from '@/lib/configSearch';

// Navigation destinations mirror the Sidebar's grouped nav. They're
// re-declared locally (rather than imported from Sidebar) to keep the palette
// self-contained and avoid coupling the two files — the destination list is a
// flat projection of the same routes/labels the sidebar renders.
interface Destination {
  to: string;
  icon: typeof LayoutDashboard;
  labelKey: string;
  groupKey: string;
}

const DESTINATIONS: Destination[] = [
  { to: '/', icon: LayoutDashboard, labelKey: 'nav.dashboard', groupKey: 'nav.group.home' },
  { to: '/agents', icon: MessageSquare, labelKey: 'nav.agents', groupKey: 'nav.group.chat' },
  { to: '/config', icon: Settings, labelKey: 'nav.config', groupKey: 'nav.group.configure' },
  { to: '/config/agents', icon: Bot, labelKey: 'nav.agent', groupKey: 'nav.group.configure' },
  { to: '/tools', icon: Wrench, labelKey: 'nav.tools', groupKey: 'nav.group.configure' },
  { to: '/skills', icon: Sparkles, labelKey: 'nav.skills', groupKey: 'nav.group.configure' },
  { to: '/integrations', icon: Puzzle, labelKey: 'nav.integrations', groupKey: 'nav.group.configure' },
  { to: '/cron', icon: Clock, labelKey: 'nav.cron', groupKey: 'nav.group.configure' },
  { to: '/logs', icon: Activity, labelKey: 'nav.logs', groupKey: 'nav.group.operations' },
  { to: '/doctor', icon: Stethoscope, labelKey: 'nav.doctor', groupKey: 'nav.group.operations' },
  { to: '/canvas', icon: Monitor, labelKey: 'nav.canvas', groupKey: 'nav.group.operations' },
  { to: '/acp-console', icon: Terminal, labelKey: 'nav.acp', groupKey: 'nav.group.operations' },
];

// The three result buckets, rendered in this order with their own headers.
// "page" = static nav destinations; "section"/"entry" come from configSearch.
type ResultKind = 'page' | 'section' | 'entry';

// A unified, keyboard-navigable result row. Nav destinations and config items
// are normalized into this single shape so the filter / selection / render
// pipeline treats them identically.
interface PaletteItem {
  kind: ResultKind;
  /** Navigated to on select. */
  to: string;
  /** Primary display + match text. */
  label: string;
  /** Secondary context shown on the right (group / owning section). */
  sublabel: string;
  /** Extra match text (the url/path) — searched but not displayed. */
  searchExtra: string;
  icon: typeof LayoutDashboard;
}

// Cap on rendered rows so a large config tree (100s of entities) stays snappy.
// Excess matches collapse into a "+N more — keep typing" hint.
const MAX_RESULTS = 50;

// Section headers + the bucket order they render in.
const KIND_ORDER: ResultKind[] = ['page', 'section', 'entry'];
// Resolved at render time so the locale catalog is consulted on each render.
function kindHeader(kind: ResultKind): string {
  switch (kind) {
    case 'page':
      return t('nav.cmdk.header.pages');
    case 'section':
      return t('nav.cmdk.header.sections');
    case 'entry':
      return t('nav.cmdk.header.entries');
  }
}
const KIND_ICON: Record<Exclude<ResultKind, 'page'>, typeof LayoutDashboard> = {
  section: FolderTree,
  entry: SlidersHorizontal,
};

// Map a configSearch item into a PaletteItem. Config sections and entries get
// distinct icons + buckets; the section/owning-section label is the sublabel.
function toPaletteItem(c: ConfigSearchItem): PaletteItem {
  const kind: ResultKind = c.group === 'Config section' ? 'section' : 'entry';
  return {
    kind,
    to: c.url,
    label: c.label,
    sublabel: c.sublabel,
    searchExtra: c.url,
    icon: KIND_ICON[kind],
  };
}

// Substring (case-insensitive) match across label + sublabel + path. Returns a
// small score so exact-prefix / label hits sort above incidental path hits;
// null when there's no match at all.
function matchScore(item: PaletteItem, q: string): number | null {
  const label = item.label.toLowerCase();
  const sub = item.sublabel.toLowerCase();
  const extra = item.searchExtra.toLowerCase();
  if (label.startsWith(q)) return 3;
  if (label.includes(q)) return 2;
  if (sub.includes(q)) return 1;
  if (extra.includes(q)) return 0;
  return null;
}

// Focusable selector for the simple focus trap.
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])';

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
}

/**
 * Operator Console command palette.
 *
 * Keyboard-first navigation launcher. Opens via ⌘K / Ctrl+K (a global keydown
 * listener installed by the parent — see useCommandPalette) or the Header
 * trigger; closes on Esc or backdrop click. Modal dialog with a focus trap:
 * focuses the search input on open and restores focus to the previously active
 * element on close. Arrow keys move the selection; Enter navigates and closes.
 */
export default function CommandPalette({ open, onClose }: CommandPaletteProps) {
  const navigate = useNavigate();
  const [query, setQuery] = useState('');
  const [selected, setSelected] = useState(0);
  // Config search items, loaded lazily on open (cached for the session by
  // configSearch). `loadingConfig` drives the subtle "loading settings…" hint;
  // nav destinations are usable the whole time regardless.
  const [configItems, setConfigItems] = useState<ConfigSearchItem[]>([]);
  const [loadingConfig, setLoadingConfig] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // Load config search items on open. Nav destinations render immediately;
  // config items fold in once resolved. Errors are already swallowed by the
  // loader (resolves to []), so the palette never breaks on a config failure.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoadingConfig(true);
    void loadConfigSearchItems()
      .then((items) => {
        if (!cancelled) setConfigItems(items);
      })
      .finally(() => {
        if (!cancelled) setLoadingConfig(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  // All searchable items in one flat list: static pages first, then config
  // sections, then config entries (toPaletteItem assigns the bucket/icon).
  const allItems = useMemo<PaletteItem[]>(() => {
    const pages: PaletteItem[] = DESTINATIONS.map((d) => ({
      kind: 'page',
      to: d.to,
      label: t(d.labelKey),
      sublabel: t(d.groupKey),
      searchExtra: d.to,
      icon: d.icon,
    }));
    return [...pages, ...configItems.map(toPaletteItem)];
  }, [configItems]);

  // Filter + sort + bucket + cap. The flat `results` list (header rows
  // interleaved) is what we render; `items` (no headers) is the keyboard-
  // navigable subset and `extraCount` feeds the "+N more" hint.
  const { rows, items: flatItems, extraCount } = useMemo(() => {
    const q = query.trim().toLowerCase();

    // Score + filter, preserving each item's natural order as a tiebreak.
    const scored = allItems
      .map((item, idx) => ({ item, idx, score: q ? matchScore(item, q) : 0 }))
      .filter((s): s is { item: PaletteItem; idx: number; score: number } => s.score !== null);
    scored.sort((a, b) => (b.score - a.score) || (a.idx - b.idx));

    const matched = scored.map((s) => s.item);
    const capped = matched.slice(0, MAX_RESULTS);
    const extra = matched.length - capped.length;

    // Interleave bucket headers. `rows` carries either a header or an item with
    // its index into the (capped) keyboard-navigable `items` list.
    type Row =
      | { type: 'header'; kind: ResultKind; key: string }
      | { type: 'item'; item: PaletteItem; index: number };
    const out: Row[] = [];
    let navIdx = 0;
    for (const kind of KIND_ORDER) {
      const group = capped.filter((it) => it.kind === kind);
      if (group.length === 0) continue;
      out.push({ type: 'header', kind, key: `h-${kind}` });
      for (const it of group) {
        out.push({ type: 'item', item: it, index: navIdx });
        navIdx += 1;
      }
    }
    // `items` must match the navIdx order used above (group-by-kind), so rebuild
    // it from the same traversal rather than from `capped` directly.
    const ordered = out.flatMap((r) => (r.type === 'item' ? [r.item] : []));
    return { rows: out, items: ordered, extraCount: Math.max(0, extra) };
  }, [allItems, query]);

  const results = flatItems;

  // Keep the selection in range whenever the result set changes.
  useEffect(() => {
    setSelected(0);
  }, [query]);

  // If config items arrive (or otherwise shrink the list), clamp the selection.
  useEffect(() => {
    setSelected((s) => (results.length === 0 ? 0 : Math.min(s, results.length - 1)));
  }, [results.length]);

  // On open: remember the focused element, focus the input. On close: restore.
  useEffect(() => {
    if (open) {
      restoreFocusRef.current = document.activeElement as HTMLElement | null;
      setQuery('');
      setSelected(0);
      // Defer to ensure the input is mounted before focusing.
      const id = window.setTimeout(() => inputRef.current?.focus(), 0);
      return () => window.clearTimeout(id);
    }
    const toRestore = restoreFocusRef.current;
    if (toRestore && typeof toRestore.focus === 'function') {
      toRestore.focus();
    }
    return undefined;
  }, [open]);

  const commit = useCallback(
    (to: string) => {
      onClose();
      navigate(to);
    },
    [navigate, onClose],
  );

  // Keep the highlighted row scrolled into view.
  useEffect(() => {
    if (!open) return;
    const el = listRef.current?.querySelector<HTMLElement>(`[data-cmdk-index="${selected}"]`);
    el?.scrollIntoView({ block: 'nearest' });
  }, [selected, open, results.length]);

  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setSelected((s) => (results.length ? (s + 1) % results.length : 0));
      return;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      setSelected((s) => (results.length ? (s - 1 + results.length) % results.length : 0));
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      const target = results[selected];
      if (target) commit(target.to);
      return;
    }
    // Minimal focus trap: keep Tab cycling within the dialog.
    if (e.key === 'Tab') {
      const root = dialogRef.current;
      if (!root) return;
      const focusable = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (n) => n.offsetParent !== null || n === document.activeElement,
      );
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) {
        e.preventDefault();
        return;
      }
      const active = document.activeElement as HTMLElement | null;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[200] flex items-start justify-center px-4 pt-[12vh] animate-fade-in"
      onKeyDown={onKeyDown}
    >
      {/* Backdrop */}
      <button
        type="button"
        aria-label={t('nav.cmdk.close')}
        onClick={onClose}
        className="absolute inset-0 bg-black/50 backdrop-blur-sm cursor-default"
        tabIndex={-1}
      />

      {/* Dialog */}
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={t('nav.cmdk.title')}
        className="relative w-full max-w-xl overflow-hidden rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface shadow-[var(--pc-shadow-md)]"
      >
        {/* Search input */}
        <div className="flex items-center gap-2.5 border-b border-pc-border px-3.5">
          <Search className="h-4 w-4 shrink-0 text-pc-text-muted" aria-hidden="true" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={t('nav.cmdk.placeholder')}
            aria-label={t('nav.cmdk.placeholder')}
            autoComplete="off"
            spellCheck={false}
            className="h-12 w-full bg-transparent text-sm text-pc-text placeholder:text-pc-text-faint outline-none border-none"
          />
          {loadingConfig && (
            <span className="shrink-0 text-[11px] text-pc-text-faint whitespace-nowrap">
              {t('nav.cmdk.loading_settings')}
            </span>
          )}
        </div>

        {/* Results */}
        <div
          ref={listRef}
          role="listbox"
          aria-label={t('nav.cmdk.title')}
          className="max-h-[min(50vh,360px)] overflow-y-auto p-1.5"
        >
          {results.length === 0 ? (
            <div className="px-3 py-6 text-center text-sm text-pc-text-muted">
              {t('nav.cmdk.empty')}
            </div>
          ) : (
            rows.map((row) => {
              if (row.type === 'header') {
                return (
                  <div
                    key={row.key}
                    role="presentation"
                    className="px-3 pt-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-pc-text-faint"
                  >
                    {kindHeader(row.kind)}
                  </div>
                );
              }
              const d = row.item;
              const i = row.index;
              const Icon = d.icon;
              const isSel = i === selected;
              return (
                <button
                  key={`${d.kind}-${d.to}-${i}`}
                  type="button"
                  role="option"
                  aria-selected={isSel}
                  data-cmdk-index={i}
                  onClick={() => commit(d.to)}
                  onMouseMove={() => setSelected(i)}
                  className={[
                    'flex w-full items-center gap-3 rounded-[var(--radius-md)] px-3 py-2 text-left text-sm transition-colors',
                    isSel ? 'bg-pc-accent/10 text-pc-text' : 'text-pc-text-secondary',
                  ].join(' ')}
                >
                  <Icon
                    className={`h-4 w-4 shrink-0 ${isSel ? 'text-pc-accent' : 'text-pc-text-muted'}`}
                    aria-hidden="true"
                  />
                  <span className="flex-1 truncate">{d.label}</span>
                  <span className="max-w-[40%] truncate text-[11px] uppercase tracking-wider text-pc-text-faint">
                    {d.sublabel}
                  </span>
                  {isSel && (
                    <CornerDownLeft className="h-3.5 w-3.5 shrink-0 text-pc-text-muted" aria-hidden="true" />
                  )}
                </button>
              );
            })
          )}
          {extraCount > 0 && (
            <div className="px-3 py-2 text-center text-[11px] text-pc-text-faint">
              {t('nav.cmdk.more_prefix')}{extraCount} {t('nav.cmdk.more_suffix')}
            </div>
          )}
        </div>

        {/* Footer hint */}
        <div className="flex items-center gap-3 border-t border-pc-border px-3.5 py-2 text-[11px] text-pc-text-faint">
          <span className="flex items-center gap-1">
            <kbd className="rounded-[var(--radius-sm)] border border-pc-border bg-pc-elevated px-1.5 py-0.5 font-mono">↑</kbd>
            <kbd className="rounded-[var(--radius-sm)] border border-pc-border bg-pc-elevated px-1.5 py-0.5 font-mono">↓</kbd>
            {t('nav.cmdk.hint.navigate')}
          </span>
          <span className="flex items-center gap-1">
            <kbd className="rounded-[var(--radius-sm)] border border-pc-border bg-pc-elevated px-1.5 py-0.5 font-mono">↵</kbd>
            {t('nav.cmdk.hint.open')}
          </span>
          <span className="flex items-center gap-1">
            <kbd className="rounded-[var(--radius-sm)] border border-pc-border bg-pc-elevated px-1.5 py-0.5 font-mono">esc</kbd>
            {t('nav.cmdk.hint.dismiss')}
          </span>
        </div>
      </div>
    </div>
  );
}

/**
 * Hook that wires the global ⌘K / Ctrl+K toggle and exposes open state.
 * Mount the returned <CommandPalette> once (Layout owns it). The keydown
 * listener is registered on mount and cleaned up on unmount.
 */
export function useCommandPalette() {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === 'k' || e.key === 'K')) {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  return {
    open,
    openPalette: useCallback(() => setOpen(true), []),
    closePalette: useCallback(() => setOpen(false), []),
  };
}
