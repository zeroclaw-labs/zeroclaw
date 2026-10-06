import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import { CornerDownLeft, FolderTree, History, Search, SlidersHorizontal, type LucideIcon } from 'lucide-react';
import { destinations, featureSettingsPath } from '@/lib/navigation';
import { useWorkspaceSettings } from '@/components/WorkspaceSettings';
import { useCodeSessions } from '@/hooks/useCodeSessions';
import { getSessions, getWorkspaceAvailability, listProps } from '@/lib/api';
import { referenceConfigPrefix } from '@/lib/configReferences';
import { sessionTarget } from '@/lib/sessionNavigation';
import type { Session } from '@/types/api';
import { t } from '@/lib/i18n';
import { loadConfigSearchItems, type ConfigSearchItem } from '@/lib/configSearch';

// Navigation, session history, and schema-derived settings share one search.
type ResultKind = 'page' | 'session' | 'section' | 'entry' | 'field';

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
  icon: LucideIcon;
  path?: string;
}

// Cap on rendered rows so a large config tree (100s of entities) stays snappy.
// Excess matches collapse into a "+N more — keep typing" hint.
const MAX_RESULTS = 50;

// Section headers + the bucket order they render in.
const KIND_ORDER: ResultKind[] = ['field', 'entry', 'section', 'page', 'session'];
// Resolved at render time so the locale catalog is consulted on each render.
function kindHeader(kind: ResultKind): string {
  switch (kind) {
    case 'page':
      return t('nav.cmdk.header.pages');
    case 'session': return t('home.sessions');
    case 'field': return t('nav.cmdk.header.fields');
    case 'section':
      return t('nav.cmdk.header.sections');
    case 'entry':
      return t('nav.cmdk.header.entries');
  }
}
const KIND_ICON: Record<Exclude<ResultKind, 'page'>, LucideIcon> = {
  session: History,
  field: SlidersHorizontal,
  section: FolderTree,
  entry: SlidersHorizontal,
};

// Map a configSearch item into a PaletteItem. Config sections and entries get
// distinct icons + buckets; the section/owning-section label is the sublabel.
function toPaletteItem(c: ConfigSearchItem): PaletteItem {
  const kind: ResultKind = c.group === 'Config section' ? 'section' : c.group === 'Config field' ? 'field' : 'entry';
  return {
    kind,
    path: c.path,
    to: c.url,
    label: c.label,
    sublabel: c.sublabel,
    searchExtra: `${c.path ?? ''} ${c.url}`,
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
  const words = q.split(/\s+/);
  if (words.every((word) => `${label} ${sub} ${extra}`.replace(/[_./]/g, ' ').includes(word))) return 0;
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
  const location = useLocation();
  const openSettings = useWorkspaceSettings();
  const codeAgent = location.pathname === '/code' ? new URLSearchParams(location.search).get('agent') : null;
  const scope = codeAgent ? `/config/agents/${encodeURIComponent(codeAgent)}` : featureSettingsPath(location.pathname);
  const [currentOnly, setCurrentOnly] = useState(true);
  const [relatedPrefixes, setRelatedPrefixes] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [sessions, setSessions] = useState<Session[]>([]);
  const [codeEnabled, setCodeEnabled] = useState(false);
  const code = useCodeSessions(open && codeEnabled);
  const [selected, setSelected] = useState(0);
  // Config search items refresh on open. `loadingConfig` drives the subtle
  // "loading settings…" hint;
  // nav destinations are usable the whole time regardless.
  const [configItems, setConfigItems] = useState<ConfigSearchItem[]>([]);
  const [loadingConfig, setLoadingConfig] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    setRelatedPrefixes([]);
    if (!open || !scope?.startsWith('/config/agents/')) return;
    let cancelled = false;
    const alias = decodeURIComponent(scope.slice('/config/agents/'.length));
    void listProps(`agents.${alias}`).then(({ entries }) => {
      if (!cancelled) setRelatedPrefixes(entries.map(referenceConfigPrefix).filter((path): path is string => path !== null));
    }).catch(() => { /* Direct agent settings remain searchable. */ });
    return () => { cancelled = true; };
  }, [open, scope]);

  // Load config search items on open. Nav destinations render immediately;
  // config items fold in once resolved. Errors are already swallowed by the
  // loader (resolves to []), so the palette never breaks on a config failure.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoadingConfig(true);
    void getWorkspaceAvailability().then((value) => { if (!cancelled) setCodeEnabled(value.code); }).catch(() => { if (!cancelled) setCodeEnabled(false); });
    void getSessions().then((items) => {
      if (!cancelled) setSessions(items.sort((a, b) => b.last_activity.localeCompare(a.last_activity)));
    }).catch(() => { if (!cancelled) setSessions([]); });
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
    const pages: PaletteItem[] = destinations.map((d) => ({
      kind: 'page',
      to: d.to,
      label: t(d.labelKey),
      sublabel: t(d.groupKey),
      searchExtra: d.to,
      icon: d.icon,
    }));
    const history: PaletteItem[] = [...sessions, ...code.sessions].map((session) => ({
      kind: 'session', to: sessionTarget(session), label: session.name || ('surface' in session ? `${t('nav.code')} · ${session.agent_alias}` : session.session_id),
      sublabel: session.agent_alias ?? session.channel_id ?? t('home.sessions'),
      searchExtra: `${session.session_key} ${session.channel_id ?? ''}`, icon: History,
    }));
    return [...pages, ...history, ...configItems.map(toPaletteItem)];
  }, [configItems, sessions, code.sessions]);

  // Filter + sort + bucket + cap. The flat `results` list (header rows
  // interleaved) is what we render; `items` (no headers) is the keyboard-
  // navigable subset and `extraCount` feeds the "+N more" hint.
  const { rows, items: flatItems, extraCount } = useMemo(() => {
    const q = query.trim().toLowerCase();

    // Score + filter, preserving each item's natural order as a tiebreak.
    const candidates = currentOnly && scope
      ? allItems.filter((item) => item.to === scope || item.to.startsWith(`${scope}?`) || item.to.startsWith(`${scope}/`) || relatedPrefixes.some((prefix) => item.path === prefix || item.path?.startsWith(`${prefix}.`)))
      : allItems;
    const scored = candidates
      .map((item, idx) => ({ item, idx, score: q ? matchScore(item, q) : 0 }))
      .filter((s): s is { item: PaletteItem; idx: number; score: number } => s.score !== null);
    const localRank = (item: PaletteItem) => scope && (item.to === scope || item.to.startsWith(`${scope}?`) || item.to.startsWith(`${scope}/`)) ? 1 : 0;
    scored.sort((a, b) => (b.score - a.score) || (localRank(b.item) - localRank(a.item)) || (a.idx - b.idx));

    const matched = scored.map((s) => s.item);
    const capped = matched.slice(0, q ? MAX_RESULTS : 12);
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
  }, [allItems, query, scope, currentOnly, relatedPrefixes]);

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
      setCurrentOnly(true);
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
      if (to.startsWith('/config')) openSettings(to);
      else navigate(to);
    },
    [navigate, onClose, openSettings],
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
            placeholder={t(scope && currentOnly ? 'workspace.search_settings' : 'nav.cmdk.placeholder')}
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

        {scope && <div className="flex items-center gap-2 border-b border-pc-border px-3.5 py-2 text-xs">
          <button type="button" aria-pressed={currentOnly} onClick={() => { setCurrentOnly(true); setSelected(0); }} className={`rounded-md px-2 py-1 ${currentOnly ? 'bg-pc-elevated text-pc-text' : 'text-pc-text-muted'}`}>{t('workspace.current_settings')} · {scope === '/config' ? t('workspace.admin') : decodeURIComponent(scope.split('/').slice(-1)[0] ?? '')}</button>
          <button type="button" aria-pressed={!currentOnly} onClick={() => { setCurrentOnly(false); setSelected(0); }} className={`rounded-md px-2 py-1 ${!currentOnly ? 'bg-pc-elevated text-pc-text' : 'text-pc-text-muted'}`}>{t('workspace.search_all')}</button>
        </div>}
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
        if (document.querySelector('dialog[open]')) return;
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
