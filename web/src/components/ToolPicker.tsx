// Reusable multi-select tool picker. Loads the built-in agent tools
// (GET /api/tools) and the discovered CLI tools (GET /api/cli-tools),
// groups them, and lets the operator toggle individual tools on/off.
//
// Tool identity is the tool `name` — exactly the strings that land in an
// `allowed_tools` list (e.g. "shell", "file_read", "web_search_tool",
// "nnet_tools__finding_record"). The component is controlled: it owns no
// selection state, it just reflects `value` and fires `onChange(next)`
// with the updated, de-duplicated, order-preserving array.
//
// Used by:
//  * FieldForm — for schema-driven `*.allowed_tools` string-array fields.
//  * Cron — for the Add/Edit job `allowed_tools` field.
//
// i18n: user-facing copy is routed through t() under the `tool_picker.`
// namespace (plus shared `common.` keys); see @/lib/i18n.

import { useEffect, useMemo, useRef, useState } from 'react';
import { Search, X, Wrench, Terminal } from 'lucide-react';
import type { ToolSpec, CliTool } from '@/types/api';
import { getTools, getCliTools } from '@/lib/api';
import { t } from '@/lib/i18n';

export interface ToolPickerProps {
  /** Currently-selected tool names. Order is preserved on toggle. */
  value: string[];
  /** Fired with the next selection (deduped, order-preserving). */
  onChange: (next: string[]) => void;
  /** When true, all toggles/chips are inert. */
  disabled?: boolean;
  /** DOM id for the search input so a `<label htmlFor>` can target it. */
  id?: string;
  /** Scope the agent-tools catalog to this agent (its built-ins plus its
   * `mcp_bundles` MCP tools) via `/api/tools?agent=`. Omit for the gateway's
   * default-agent listing. CLI tools are always included (not agent-scoped). */
  agent?: string;
}

/** A flattened, group-tagged catalog entry. */
interface CatalogEntry {
  name: string;
  description: string;
  group: 'agent' | 'cli';
}

// Process-wide cache so re-mounting the picker (e.g. reopening the Cron
// modal, or switching config sections) doesn't re-hit the network. Keyed by
// agent alias (`'' `= the gateway's default-agent listing): the agent-tools
// half is `getTools(agent)`, so a picker bound to a specific agent (e.g. a
// channel's owning agent) caches that agent's real scoped catalog separately
// from the default. Each per-agent catalog is effectively static for the
// daemon's lifetime.
const catalogCache = new Map<string, CatalogEntry[]>();
const catalogInflight = new Map<string, Promise<CatalogEntry[]>>();

function cliDescription(tool: CliTool): string {
  // CliTool has no `description`; synthesize a short one from category/path
  // so the row still says something useful.
  const parts = [tool.category, tool.version ? `v${tool.version}` : null, tool.path]
    .filter(Boolean)
    .join(' · ');
  return parts || tool.path;
}

function loadCatalog(agent?: string): Promise<CatalogEntry[]> {
  const key = agent ?? '';
  const cached = catalogCache.get(key);
  if (cached) return Promise.resolve(cached);
  const inflight = catalogInflight.get(key);
  if (inflight) return inflight;
  const promise = Promise.all([getTools(agent), getCliTools()])
    .then(([tools, cliTools]) => {
      const agentEntries: CatalogEntry[] = tools.map((tnt: ToolSpec) => ({
        name: tnt.name,
        description: tnt.description,
        group: 'agent' as const,
      }));
      const cli: CatalogEntry[] = cliTools.map((c: CliTool) => ({
        name: c.name,
        description: cliDescription(c),
        group: 'cli' as const,
      }));
      const entries = [...agentEntries, ...cli];
      catalogCache.set(key, entries);
      return entries;
    })
    .finally(() => {
      catalogInflight.delete(key);
    });
  catalogInflight.set(key, promise);
  return promise;
}

function truncate(text: string, max = 110): string {
  if (text.length <= max) return text;
  return `${text.slice(0, max - 1)}…`;
}

export default function ToolPicker({
  value,
  onChange,
  disabled = false,
  id,
  agent,
}: ToolPickerProps) {
  const cacheKey = agent ?? '';
  const [catalog, setCatalog] = useState<CatalogEntry[] | null>(
    () => catalogCache.get(cacheKey) ?? null,
  );
  const [loading, setLoading] = useState(() => !catalogCache.has(cacheKey));
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');

  // Reload when the bound agent changes so the catalog reflects that agent's
  // scoped tools (cached per agent, so switching back is instant).
  useEffect(() => {
    const cached = catalogCache.get(cacheKey);
    if (cached) {
      setCatalog(cached);
      setLoading(false);
      setError(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);
    setCatalog(null);
    loadCatalog(agent)
      .then((entries) => {
        if (!cancelled) {
          setCatalog(entries);
          setLoading(false);
        }
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : t('tool_picker.load_failed'));
          setLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [agent, cacheKey]);

  // Fast membership lookups for the catalog and the current selection.
  const byName = useMemo(() => {
    const map = new Map<string, CatalogEntry>();
    for (const e of catalog ?? []) map.set(e.name, e);
    return map;
  }, [catalog]);

  const selectedSet = useMemo(() => new Set(value), [value]);

  // Selection toggle. Preserves order, dedupes, never mutates `value`.
  const toggle = (name: string) => {
    if (disabled) return;
    if (selectedSet.has(name)) {
      onChange(value.filter((n) => n !== name));
    } else {
      // Dedupe defensively in case `value` arrived with repeats.
      const next = value.includes(name) ? value : [...value, name];
      onChange(next);
    }
  };

  const removeChip = (name: string) => {
    if (disabled) return;
    onChange(value.filter((n) => n !== name));
  };

  // Search filter over name + description (case-insensitive).
  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    const list = catalog ?? [];
    if (!q) return list;
    return list.filter(
      (e) =>
        e.name.toLowerCase().includes(q) ||
        e.description.toLowerCase().includes(q),
    );
  }, [catalog, search]);

  const agentEntries = useMemo(
    () => filtered.filter((e) => e.group === 'agent'),
    [filtered],
  );
  const cliEntries = useMemo(
    () => filtered.filter((e) => e.group === 'cli'),
    [filtered],
  );

  // Selected names that aren't in the catalog (unknown / removed tools).
  // Surface them as chips so nothing is silently dropped on save.
  const unknownSelected = useMemo(
    () => value.filter((n) => !byName.has(n)),
    [value, byName],
  );

  const listboxId = id ? `${id}-listbox` : undefined;
  const listboxRef = useRef<HTMLDivElement>(null);

  // Roving keyboard navigation across the visible option rows. Arrow Up/Down
  // (and Home/End) move DOM focus between rows; Enter/Space toggle is handled
  // per-row. We re-query on each keypress so the set always reflects the
  // current filter without tracking indices in state.
  const moveFocus = (from: HTMLElement, delta: number, toEnd?: 'first' | 'last') => {
    const container = listboxRef.current;
    if (!container) return;
    const options = Array.from(
      container.querySelectorAll<HTMLElement>('[role="option"]:not([aria-disabled="true"])'),
    );
    if (options.length === 0) return;
    if (toEnd === 'first') {
      options[0]!.focus();
      return;
    }
    if (toEnd === 'last') {
      options[options.length - 1]!.focus();
      return;
    }
    const idx = options.indexOf(from);
    const nextIdx = idx === -1 ? 0 : (idx + delta + options.length) % options.length;
    options[nextIdx]!.focus();
  };

  const onListboxKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement;
    if (!target.matches('[role="option"]')) return;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        moveFocus(target, 1);
        break;
      case 'ArrowUp':
        e.preventDefault();
        moveFocus(target, -1);
        break;
      case 'Home':
        e.preventDefault();
        moveFocus(target, 0, 'first');
        break;
      case 'End':
        e.preventDefault();
        moveFocus(target, 0, 'last');
        break;
      default:
        break;
    }
  };

  return (
    <div className="space-y-2">
      {/* Selected chips */}
      <div className="flex flex-wrap gap-1.5" aria-label={t('tool_picker.selected_tools')}>
        {value.length === 0 ? (
          <span className="text-xs text-pc-text-faint py-0.5">
            {t('tool_picker.no_tools_selected')}
          </span>
        ) : (
          value.map((name) => {
            const known = byName.has(name);
            return (
              <span
                key={name}
                className={[
                  // min-h keeps the whole chip a comfortable touch target.
                  'inline-flex min-h-[44px] items-center gap-1 rounded-[var(--radius-md)] pl-2.5 pr-1 text-xs font-medium border',
                  known
                    ? 'border-pc-accent/30 bg-pc-accent/10 text-pc-accent'
                    : 'border-status-warning/30 bg-status-warning/10 text-status-warning',
                ].join(' ')}
                title={
                  known
                    ? name
                    : `${name}${t('tool_picker.not_in_catalog_suffix')}`
                }
              >
                <span className="font-mono truncate max-w-[16rem]">{name}</span>
                <button
                  type="button"
                  onClick={() => removeChip(name)}
                  disabled={disabled}
                  aria-label={`${t('tool_picker.remove_prefix')}${name}`}
                  className="inline-flex h-9 w-9 flex-shrink-0 items-center justify-center self-stretch rounded-full hover:bg-pc-accent/20 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed cursor-pointer"
                >
                  <X className="h-3.5 w-3.5" />
                </button>
              </span>
            );
          })
        )}
      </div>

      {/* Search box */}
      <div className="relative">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-pc-text-faint pointer-events-none" />
        <input
          id={id}
          type="text"
          role="combobox"
          aria-expanded={!loading && error === null}
          aria-controls={listboxId}
          autoComplete="off"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          disabled={disabled || loading || error !== null}
          placeholder={t('tool_picker.search_placeholder')}
          className="w-full h-9 pl-9 pr-3 text-sm rounded-[var(--radius-md)] border border-pc-border bg-pc-input text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:border-pc-border-strong focus:ring-2 focus:ring-[var(--pc-focus)]/30 disabled:opacity-50 disabled:cursor-not-allowed"
        />
      </div>

      {/* Catalog list */}
      {loading ? (
        <div className="flex items-center gap-2 px-3 py-4 text-xs text-pc-text-muted">
          <div
            className="h-4 w-4 border-2 rounded-full animate-spin border-pc-border"
            style={{ borderTopColor: 'var(--pc-accent)' }}
          />
          {t('tool_picker.loading')}
        </div>
      ) : error ? (
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 px-3 py-2 text-xs text-status-error">
          {t('tool_picker.load_failed_prefix')}{error}
        </div>
      ) : (
        <div
          id={listboxId}
          ref={listboxRef}
          role="listbox"
          aria-multiselectable="true"
          aria-label={t('tool_picker.available_tools')}
          onKeyDown={onListboxKeyDown}
          className="max-h-64 overflow-y-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface divide-y divide-pc-border/60"
        >
          {/* Unknown-but-selected entries float to the top so the operator
              can see (and keep or drop) tools the catalog no longer lists. */}
          {unknownSelected.length > 0 && (
            <ToolGroup
              icon={<X className="h-3.5 w-3.5 text-status-warning" />}
              label={t('tool_picker.group_unknown')}
              count={unknownSelected.length}
            >
              {unknownSelected.map((name) => (
                <ToolRow
                  key={name}
                  name={name}
                  description={t('tool_picker.unknown_tool_desc')}
                  selected
                  disabled={disabled}
                  unknown
                  onToggle={() => toggle(name)}
                />
              ))}
            </ToolGroup>
          )}

          {agentEntries.length > 0 && (
            <ToolGroup
              icon={<Wrench className="h-3.5 w-3.5 text-pc-accent" />}
              label={t('tool_picker.group_agent')}
              count={agentEntries.length}
            >
              {agentEntries.map((e) => (
                <ToolRow
                  key={e.name}
                  name={e.name}
                  description={e.description}
                  selected={selectedSet.has(e.name)}
                  disabled={disabled}
                  onToggle={() => toggle(e.name)}
                />
              ))}
            </ToolGroup>
          )}

          {cliEntries.length > 0 && (
            <ToolGroup
              icon={<Terminal className="h-3.5 w-3.5 text-pc-text-muted" />}
              label={t('tool_picker.group_cli')}
              count={cliEntries.length}
            >
              {cliEntries.map((e) => (
                <ToolRow
                  key={e.name}
                  name={e.name}
                  description={e.description}
                  selected={selectedSet.has(e.name)}
                  disabled={disabled}
                  onToggle={() => toggle(e.name)}
                />
              ))}
            </ToolGroup>
          )}

          {agentEntries.length === 0 &&
            cliEntries.length === 0 &&
            unknownSelected.length === 0 && (
              <p className="px-3 py-4 text-xs text-center text-pc-text-muted">
                {search.trim()
                  ? `${t('tool_picker.no_match_prefix')}"${search.trim()}"${t('tool_picker.no_match_suffix')}`
                  : t('tool_picker.no_tools_available')}
              </p>
            )}
        </div>
      )}
    </div>
  );
}

function ToolGroup({
  icon,
  label,
  count,
  children,
}: {
  icon: React.ReactNode;
  label: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div className="sticky top-0 z-10 flex items-center gap-1.5 px-3 py-1.5 bg-pc-elevated border-b border-pc-border/60">
        {icon}
        <span className="text-[10px] font-semibold uppercase tracking-wider text-pc-text-faint">
          {label}
        </span>
        <span className="text-[10px] text-pc-text-faint">({count})</span>
      </div>
      <div className="divide-y divide-pc-border/40">{children}</div>
    </div>
  );
}

function ToolRow({
  name,
  description,
  selected,
  disabled,
  unknown,
  onToggle,
}: {
  name: string;
  description: string;
  selected: boolean;
  disabled?: boolean;
  unknown?: boolean;
  onToggle: () => void;
}) {
  return (
    <div
      role="option"
      aria-selected={selected}
      aria-disabled={disabled || undefined}
      tabIndex={disabled ? -1 : 0}
      onClick={() => {
        if (!disabled) onToggle();
      }}
      onKeyDown={(e) => {
        if (disabled) return;
        // Arrow/Home/End bubble to the listbox handler for roving focus.
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onToggle();
        }
      }}
      className={[
        'flex min-h-[44px] items-start gap-2.5 px-3 py-2.5 transition-colors focus:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-[var(--pc-focus)]/40',
        disabled ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer',
        selected ? 'bg-pc-accent/10' : 'hover:bg-pc-elevated/60',
      ].join(' ')}
    >
      <input
        type="checkbox"
        checked={selected}
        readOnly
        disabled={disabled}
        tabIndex={-1}
        aria-hidden="true"
        className="mt-0.5 accent-pc-accent pointer-events-none"
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span
            className={[
              'font-mono text-xs truncate',
              selected ? 'text-pc-accent' : 'text-pc-text',
            ].join(' ')}
          >
            {name}
          </span>
          {unknown && (
            <span className="text-[10px] uppercase tracking-wide text-status-warning">
              {t('tool_picker.unknown_badge')}
            </span>
          )}
        </div>
        <p className="text-xs mt-0.5 text-pc-text-muted">
          {truncate(description)}
        </p>
      </div>
    </div>
  );
}
