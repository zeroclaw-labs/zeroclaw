import { getMapKeys, getSectionPicker, getSections, listProps, type ListResponseEntry, type SectionInfo } from "./api";
import { badgeIsGood } from "../components/sections/SectionPicker";

/** A flat, jump-to-able config search target. */
export interface ConfigSearchItem {
  /** Display + primary match text (section label, or alias / "type / alias"). */
  label: string;
  /** Secondary context line (the owning section's label, or section group). */
  sublabel: string;
  /** Form URL navigated to on select — same scheme Config.tsx deep-links use. */
  url: string;
  /** Coarse bucket used for the palette's grouped headers + matching weight. */
  group: "Config section" | "Config entry" | "Config field";
  /** Canonical config path; values are deliberately never indexed. */
  path?: string;
}

// Same predicate SectionNavigator uses: only these two shapes have children.
function sectionHasChildren(s: SectionInfo): boolean {
  return s.shape === "one_tier_alias_map" || s.shape === "typed_family_map";
}

// Build the section-level item every section contributes (its own jump target).
function sectionItem(s: SectionInfo): ConfigSearchItem {
  return {
    label: s.label,
    path: s.key,
    sublabel: s.group,
    url: `/config/${encodeURIComponent(s.key)}`,
    group: "Config section",
  };
}

// Enumerate the configured entities (aliases) under one section, by shape.
// Mirrors SectionNavigator.loadEntities:
//   one_tier_alias_map → getMapKeys(section.key)
//   typed_family_map   → configured/active types via getSectionPicker, then
//                         getMapKeys(`${key}.${type}`) per type
// Any per-section/per-type fetch error is swallowed so one bad section can't
// sink the whole index.
async function loadEntities(section: SectionInfo): Promise<ConfigSearchItem[]> {
  if (section.shape === "one_tier_alias_map") {
    try {
      const { keys } = await getMapKeys(section.key);
      return keys.map((alias) => ({
        label: alias,
        path: `${section.key}.${alias}`,
        sublabel: section.label,
        url: `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(alias)}`,
        group: "Config entry" as const,
      }));
    } catch {
      return [];
    }
  }

  if (section.shape === "typed_family_map") {
    let configuredTypes: string[] = [];
    try {
      const picker = await getSectionPicker(section.key);
      configuredTypes = picker.items
        .filter((i) => badgeIsGood(i.badge))
        .map((i) => i.key);
    } catch {
      return [];
    }
    const out: ConfigSearchItem[] = [];
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
          path: `${section.key}.${type}.${alias}`,
          sublabel: section.label,
          url: `/config/${encodeURIComponent(section.key)}/${encodeURIComponent(type)}/${encodeURIComponent(alias)}`,
          group: "Config entry",
        });
      }
    }
    return out;
  }

  // direct_form / backend_picker / unknown: no children — the section item alone.
  return [];
}

/** Resolve a field to the longest configured owner, with its schema-provided tab. */
export function fieldSearchItems(owners: ConfigSearchItem[], entries: ListResponseEntry[]): ConfigSearchItem[] {
  const normalize = (path: string) => path.replace(/-/g, '_');
  const sorted = [...owners].sort((a, b) => (b.path?.length ?? 0) - (a.path?.length ?? 0));
  return entries.flatMap((entry) => {
    const owner = sorted.find((item) => item.path && (normalize(entry.path) === normalize(item.path) || normalize(entry.path).startsWith(`${normalize(item.path)}.`)));
    if (!owner?.path) return [];
    const query = new URLSearchParams({ field: entry.path });
    if (entry.tab) query.set('tab', entry.tab.toLowerCase().replace(/\s+/g, '-'));
    return [{
      label: entry.path.slice(owner.path.length + 1).replace(/[._]/g, ' ') || entry.path,
      sublabel: `${owner.label} · ${entry.tab ?? owner.sublabel}`,
      url: `${owner.url}?${query}`,
      group: 'Config field' as const,
      path: entry.path,
    }];
  });
}

async function build(): Promise<ConfigSearchItem[]> {
  const { sections } = await getSections();
  const items = sections.map(sectionItem);
  const [entities, fields] = await Promise.all([
    Promise.allSettled(sections.filter(sectionHasChildren).map(loadEntities)),
    listProps().catch(() => ({ entries: [] })),
  ]);
  for (const result of entities) if (result.status === 'fulfilled') items.push(...result.value);
  return [...items, ...fieldSearchItems(items, fields.entries)];
}

// Coalesce simultaneous opens, but refresh on every later open so another
// client's config changes are reflected. This index never stores field values.
let inFlight: Promise<ConfigSearchItem[]> | null = null;
export function loadConfigSearchItems(): Promise<ConfigSearchItem[]> {
  if (!inFlight) inFlight = build().catch(() => []).finally(() => { inFlight = null; });
  return inFlight;
}
export function clearConfigSearchCache(): void { inFlight = null; }
