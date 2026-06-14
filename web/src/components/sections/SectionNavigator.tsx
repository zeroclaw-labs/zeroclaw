// Master navigator for the Config master-detail layout (#6175 follow-up).
//
// A single ~300px column that replaces the old section-list → overview →
// alias-list drill-down. It shows, top to bottom:
//   1. a search box that filters across every section AND every configured
//      entity (alias) at once;
//   2. the sections grouped by the caller-provided GROUP_ORDER, each a
//      collapsible row;
//   3. under an expanded section, its CONFIGURED entities (aliases) as
//      selectable rows, lazily fetched the first time the section opens.
//
// Selecting an entity NAVIGATES to that entity's existing form URL so the
// address bar stays the source of truth — deep-linking and the existing
// `mainContent` dispatch in Config.tsx keep working untouched. Flat
// (`direct_form`) and `backend_picker` sections have no children: the
// section row itself is the selectable entity (navigates to /config/<key>).
//
// NO hardcoded section keys, field names, or option lists. Entity
// enumeration is driven entirely by the server-emitted `shape` plus the
// same `getMapKeys` / `getSectionPicker` endpoints the old AliasListView
// and SectionOverview used.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDown, ChevronRight, Plus, Search } from "lucide-react";
import {
  getMapKeys,
  getSectionPicker,
  type SectionInfo,
} from "../../lib/api";
import { fuzzyFilter } from "../../lib/fuzzy";

// One selectable entity under a section. `url` is the entity's existing
// form URL; `mapPath` is the dotted map path it lives under (used so the
// caller can offer delete via the existing deleteMapKey flow if wanted).
export interface NavEntity {
  /** Display + match label (the alias, or for typed sections "type / alias"). */
  label: string;
  /** Stable identity within a section (used as React key / selection match). */
  id: string;
  /** Existing form URL — navigated to on select. */
  url: string;
}

interface SectionNavigatorProps {
  sections: SectionInfo[];
  /** Display order for the collapsible group headings. */
  groupOrder: readonly string[];
  /** Currently-active section key (drives auto-expand + highlight). */
  activeSectionKey: string | null;
  /** Pathname of the currently-selected entity, for highlight matching. */
  selectedPath: string;
  /** Navigate to an entity's form URL. */
  onNavigate: (url: string) => void;
  /** Select a section itself (flat / backend-picker sections, or section
   *  header click). Navigates to /config/<key>. */
  onSelectSection: (key: string) => void;
  /** Trigger the existing add-alias flow for a section (parent owns the
   *  modal/prompt + selectSectionItem call). */
  onAddToSection: (section: SectionInfo) => void;
  /** Bump to force expanded sections to re-fetch their entities (e.g. after
   *  an add/delete/reload). */
  refreshKey: number;
  /** Extra classes for the root (e.g. responsive visibility from the parent). */
  className?: string;
}

// A section's editor shape determines how (and whether) it has children.
function sectionHasChildren(s: SectionInfo): boolean {
  return (
    s.shape === "one_tier_alias_map" || s.shape === "typed_family_map"
  );
}

export default function SectionNavigator({
  sections,
  groupOrder,
  activeSectionKey,
  selectedPath,
  onNavigate,
  onSelectSection,
  onAddToSection,
  refreshKey,
  className = "",
}: SectionNavigatorProps) {
  const [query, setQuery] = useState("");
  // Which section keys are expanded. The active section auto-expands.
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  // Lazily-loaded entities, keyed by section key.
  const [entitiesBySection, setEntitiesBySection] = useState<
    Record<string, NavEntity[] | "loading" | { error: string }>
  >({});

  // Auto-expand the active section so its entities are visible on landing /
  // deep-link. (Doesn't collapse anything the user opened manually.)
  useEffect(() => {
    if (!activeSectionKey) return;
    const sec = sections.find((s) => s.key === activeSectionKey);
    if (sec && sectionHasChildren(sec)) {
      setExpanded((prev) => {
        if (prev.has(activeSectionKey)) return prev;
        const next = new Set(prev);
        next.add(activeSectionKey);
        return next;
      });
    }
  }, [activeSectionKey, sections]);

  // Enumerate the configured entities (aliases) for a section, by shape.
  // Reuses the SAME endpoints the old AliasListView / SectionOverview used:
  //   one_tier_alias_map → getMapKeys(section.key)
  //   typed_family_map   → configured types via getSectionPicker, then
  //                         getMapKeys(section.key + '.' + type) per type
  const loadEntities = useCallback(
    async (section: SectionInfo): Promise<NavEntity[]> => {
      if (section.shape === "one_tier_alias_map") {
        const { keys } = await getMapKeys(section.key);
        return keys.map((alias) => ({
          label: alias,
          id: alias,
          url: `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(alias)}`,
        }));
      }
      if (section.shape === "typed_family_map") {
        // Configured/active provider or channel types under this section.
        const picker = await getSectionPicker(section.key);
        const configuredTypes = picker.items
          .filter((i) => i.badge === "configured" || i.badge === "active")
          .map((i) => i.key);
        const out: NavEntity[] = [];
        for (const type of configuredTypes) {
          let keys: string[] = [];
          try {
            keys = (await getMapKeys(`${section.key}.${type}`)).keys;
          } catch {
            keys = [];
          }
          for (const alias of keys) {
            out.push({
              label: `${type} / ${alias}`,
              id: `${type}/${alias}`,
              url: `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(type)}/${encodeURIComponent(alias)}`,
            });
          }
        }
        return out;
      }
      return [];
    },
    [],
  );

  // Fetch entities for every expanded section that has children. Re-runs on
  // refreshKey so adds/deletes/reloads refresh the lists.
  const expandedKeysSig = useMemo(
    () => [...expanded].sort().join(","),
    [expanded],
  );
  // Track the latest request per section so a slow earlier fetch can't
  // clobber a newer one.
  const reqSeq = useRef<Record<string, number>>({});
  useEffect(() => {
    let cancelled = false;
    for (const key of expanded) {
      const section = sections.find((s) => s.key === key);
      if (!section || !sectionHasChildren(section)) continue;
      const seq = (reqSeq.current[key] ?? 0) + 1;
      reqSeq.current[key] = seq;
      setEntitiesBySection((prev) => ({ ...prev, [key]: "loading" }));
      void loadEntities(section)
        .then((list) => {
          if (cancelled || reqSeq.current[key] !== seq) return;
          setEntitiesBySection((prev) => ({ ...prev, [key]: list }));
        })
        .catch((e) => {
          if (cancelled || reqSeq.current[key] !== seq) return;
          setEntitiesBySection((prev) => ({
            ...prev,
            [key]: { error: e instanceof Error ? e.message : String(e) },
          }));
        });
    }
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedKeysSig, refreshKey, sections]);

  const toggle = (key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  // Keyboard navigation across the visible tree rows. The navigable rows are
  // marked with `data-nav-row` (the section's primary label button and each
  // entity/search-result button); Arrow Up/Down move DOM focus between them,
  // Home/End jump to the ends. Enter/Space keep their native <button> behavior
  // (select / expand) so selection logic is untouched.
  const navRef = useRef<HTMLElement>(null);
  const onTreeKeyDown = (e: React.KeyboardEvent<HTMLElement>) => {
    if (
      e.key !== 'ArrowDown'
      && e.key !== 'ArrowUp'
      && e.key !== 'Home'
      && e.key !== 'End'
    ) {
      return;
    }
    const container = navRef.current;
    if (!container) return;
    const rows = Array.from(
      container.querySelectorAll<HTMLElement>('[data-nav-row]'),
    ).filter((el) => el.offsetParent !== null);
    if (rows.length === 0) return;
    e.preventDefault();
    const current = document.activeElement as HTMLElement | null;
    const idx = current ? rows.indexOf(current) : -1;
    let nextIdx: number;
    if (e.key === 'Home') nextIdx = 0;
    else if (e.key === 'End') nextIdx = rows.length - 1;
    else if (e.key === 'ArrowDown') nextIdx = idx === -1 ? 0 : (idx + 1) % rows.length;
    else nextIdx = idx === -1 ? rows.length - 1 : (idx - 1 + rows.length) % rows.length;
    rows[nextIdx]!.focus();
  };

  // Grouped, ordered sections for rendering. Same logic the old aside used.
  const grouped = useMemo(() => {
    const known = new Set(groupOrder);
    return groupOrder
      .map((groupName) => {
        const items = sections
          .filter((s) =>
            groupName === "Other"
              ? s.group === "Other" ||
                !known.has(s.group as (typeof groupOrder)[number])
              : s.group === groupName,
          )
          .sort((a, b) => {
            if (groupName === "Foundation") {
              return sections.indexOf(a) - sections.indexOf(b);
            }
            return a.label.localeCompare(b.label);
          });
        return { groupName, items };
      })
      .filter((g) => g.items.length > 0);
  }, [sections, groupOrder]);

  // When a query is present, the navigator switches to a flat search-results
  // mode: every section whose label matches, plus every loaded entity whose
  // label matches. Entities only show for sections already expanded/loaded —
  // but we proactively load any section whose label matches so the user can
  // drill into it. Matching sections auto-expand below.
  const trimmed = query.trim();
  const searching = trimmed.length > 0;

  // Proactively load entities for sections that match the query OR whose
  // (loaded) entities might match, so search reaches into aliases too.
  useEffect(() => {
    if (!searching) return;
    for (const s of sections) {
      if (!sectionHasChildren(s)) continue;
      if (entitiesBySection[s.key] !== undefined) continue;
      // Load on demand for search reach.
      setExpanded((prev) => {
        if (prev.has(s.key)) return prev;
        const next = new Set(prev);
        next.add(s.key);
        return next;
      });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searching, trimmed]);

  // Flatten loaded entities for fuzzy search.
  const searchHits = useMemo(() => {
    if (!searching) return null;
    type Hit =
      | { kind: "section"; section: SectionInfo }
      | { kind: "entity"; section: SectionInfo; entity: NavEntity };
    const hits: Hit[] = [];
    for (const s of sections) {
      hits.push({ kind: "section", section: s });
      const ents = entitiesBySection[s.key];
      if (Array.isArray(ents)) {
        for (const e of ents) {
          hits.push({ kind: "entity", section: s, entity: e });
        }
      }
    }
    return fuzzyFilter(hits, trimmed, (h) =>
      h.kind === "section"
        ? `${h.section.key} ${h.section.label}`
        : `${h.section.key} ${h.section.label} ${h.entity.label} ${h.entity.id}`,
    );
  }, [searching, trimmed, sections, entitiesBySection]);

  const isEntitySelected = (url: string) => {
    // Compare the path portion only (ignore ?tab=) so a tab switch keeps the
    // entity highlighted.
    const path = url.split("?")[0];
    return selectedPath === path;
  };

  const renderEntities = (section: SectionInfo) => {
    const ents = entitiesBySection[section.key];
    if (ents === "loading" || ents === undefined) {
      return (
        <div className="pl-7 pr-3 py-1.5 text-xs text-pc-text-faint">
          Loading…
        </div>
      );
    }
    if (!Array.isArray(ents)) {
      return (
        <div className="pl-7 pr-3 py-1.5 text-xs text-status-error">
          {ents.error}
        </div>
      );
    }
    if (ents.length === 0) {
      return (
        <div className="pl-7 pr-3 py-1.5 text-xs text-pc-text-faint italic">
          Nothing configured yet
        </div>
      );
    }
    return ents.map((e) => {
      const sel = isEntitySelected(e.url);
      return (
        <button
          key={e.id}
          type="button"
          role="treeitem"
          data-nav-row
          onClick={() => onNavigate(e.url)}
          aria-current={sel ? "page" : undefined}
          className={[
            "w-full flex items-center gap-2 rounded-[var(--radius-sm)] mx-1.5 pl-5 pr-2.5 py-1.5",
            "text-sm text-left transition-colors truncate",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-inset",
            sel
              ? "bg-pc-accent/10 text-pc-accent font-medium"
              : "text-pc-text-secondary hover:bg-pc-elevated/60 hover:text-pc-text",
          ].join(" ")}
          title={e.label}
        >
          <span className="truncate">{e.label}</span>
        </button>
      );
    });
  };

  return (
    <aside className={`w-full md:w-[300px] flex-shrink-0 border-r border-pc-border flex flex-col min-h-0 ${className}`}>
      {/* Search box */}
      <div className="p-3 border-b border-pc-border">
        <div className="relative">
          <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-pc-text-faint pointer-events-none" />
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search settings…"
            className="w-full pl-8 pr-3 py-2 text-sm rounded-[var(--radius-md)] bg-pc-input border border-pc-border text-pc-text placeholder:text-pc-text-faint focus-visible:outline-none focus-visible:border-pc-border-strong focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
          />
        </div>
      </div>

      <nav
        ref={navRef}
        role="tree"
        aria-label="Settings sections"
        onKeyDown={onTreeKeyDown}
        className="flex-1 overflow-y-auto py-2"
      >
        {searching ? (
          // Flat search-results mode.
          searchHits && searchHits.length > 0 ? (
            <div role="group" className="flex flex-col">
              {searchHits.map((h, i) => {
                if (h.kind === "section") {
                  const active = h.section.key === activeSectionKey;
                  return (
                    <button
                      key={`s-${h.section.key}-${i}`}
                      type="button"
                      role="treeitem"
                      data-nav-row
                      onClick={() => onSelectSection(h.section.key)}
                      aria-current={active ? "page" : undefined}
                      className={[
                        "mx-1.5 flex items-center justify-between gap-2 rounded-[var(--radius-sm)]",
                        "px-2.5 py-1.5 text-sm text-left transition-colors",
                        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-inset",
                        active
                          ? "bg-pc-accent/10 text-pc-accent font-medium"
                          : "text-pc-text-secondary hover:bg-pc-elevated/60 hover:text-pc-text",
                      ].join(" ")}
                    >
                      <span className="truncate">{h.section.label}</span>
                      <span className="text-[10px] uppercase tracking-wider text-pc-text-faint flex-shrink-0">
                        {h.section.group}
                      </span>
                    </button>
                  );
                }
                const sel = isEntitySelected(h.entity.url);
                return (
                  <button
                    key={`e-${h.section.key}-${h.entity.id}-${i}`}
                    type="button"
                    role="treeitem"
                    data-nav-row
                    onClick={() => onNavigate(h.entity.url)}
                    aria-current={sel ? "page" : undefined}
                    className={[
                      "mx-1.5 flex items-center justify-between gap-2 rounded-[var(--radius-sm)]",
                      "px-2.5 py-1.5 text-sm text-left transition-colors",
                      "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-inset",
                      sel
                        ? "bg-pc-accent/10 text-pc-accent font-medium"
                        : "text-pc-text-secondary hover:bg-pc-elevated/60 hover:text-pc-text",
                    ].join(" ")}
                    title={`${h.section.label} · ${h.entity.label}`}
                  >
                    <span className="truncate">{h.entity.label}</span>
                    <span className="text-[10px] text-pc-text-faint flex-shrink-0 truncate max-w-[40%]">
                      {h.section.label}
                    </span>
                  </button>
                );
              })}
            </div>
          ) : (
            <div className="px-3 py-6 text-sm text-center text-pc-text-muted">
              No matches.
            </div>
          )
        ) : (
          // Grouped collapsible mode.
          grouped.map(({ groupName, items }) => (
            <div key={groupName} role="group" aria-label={groupName} className="mb-1">
              <div className="px-3 pt-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-pc-text-faint">
                {groupName}
              </div>
              {items.map((s) => {
                const active = s.key === activeSectionKey;
                const hasKids = sectionHasChildren(s);
                const isOpen = expanded.has(s.key);
                // Flat / backend-picker sections: the row IS the entity.
                // Section row is highlighted when it's the selection target
                // and there's no deeper entity selected.
                const sectionSelected =
                  active &&
                  !hasKids &&
                  (selectedPath === `/config/${encodeURIComponent(s.key)}` ||
                    selectedPath === `/config/${s.key}`);
                return (
                  <div key={s.key}>
                    <div
                      className={[
                        "group mx-1.5 flex items-center gap-1 rounded-[var(--radius-sm)]",
                        "transition-colors",
                        active
                          ? "bg-pc-accent/10"
                          : "hover:bg-pc-elevated/60",
                      ].join(" ")}
                    >
                      {hasKids ? (
                        <button
                          type="button"
                          onClick={() => toggle(s.key)}
                          aria-label={isOpen ? "Collapse" : "Expand"}
                          aria-expanded={isOpen}
                          className="flex-shrink-0 p-1.5 text-pc-text-muted hover:text-pc-text"
                        >
                          {isOpen ? (
                            <ChevronDown className="h-3.5 w-3.5" />
                          ) : (
                            <ChevronRight className="h-3.5 w-3.5" />
                          )}
                        </button>
                      ) : (
                        <span className="flex-shrink-0 w-[26px]" />
                      )}
                      <button
                        type="button"
                        role="treeitem"
                        data-nav-row
                        aria-expanded={hasKids ? isOpen : undefined}
                        onClick={() =>
                          hasKids ? toggle(s.key) : onSelectSection(s.key)
                        }
                        aria-current={
                          active || sectionSelected ? "page" : undefined
                        }
                        className={[
                          "flex-1 min-w-0 text-left text-sm py-1.5 pr-1 transition-colors truncate rounded-[var(--radius-sm)]",
                          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-inset",
                          active || sectionSelected
                            ? "text-pc-accent font-medium"
                            : "text-pc-text-secondary group-hover:text-pc-text",
                        ].join(" ")}
                      >
                        <span className="truncate">{s.label}</span>
                      </button>
                      {hasKids && (
                        <button
                          type="button"
                          onClick={() => onAddToSection(s)}
                          title={`Add to ${s.label}`}
                          aria-label={`Add to ${s.label}`}
                          className="flex-shrink-0 p-1.5 mr-0.5 rounded-[var(--radius-sm)] text-pc-text-faint opacity-0 group-hover:opacity-100 hover:text-pc-accent hover:bg-pc-accent/10 transition-opacity"
                        >
                          <Plus className="h-3.5 w-3.5" />
                        </button>
                      )}
                    </div>
                    {hasKids && isOpen && (
                      <div role="group" className="flex flex-col py-0.5">
                        {renderEntities(s)}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          ))
        )}
      </nav>
    </aside>
  );
}
