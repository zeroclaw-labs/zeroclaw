// Config search index for the ⌘K command palette.
//
// Turns the live config tree into a flat, searchable list of jump targets so
// the palette can take an operator straight to any config SECTION or any
// configured ENTITY (alias), not just the static nav destinations.
//
// Entity enumeration mirrors SectionNavigator.tsx exactly — same `shape`
// dispatch, same endpoints (`getSections` / `getSectionPicker` / `getMapKeys`),
// same URL construction — so deep links land on the identical form Config.tsx
// already renders. There are NO hardcoded section keys here: everything is
// driven by the server-emitted `shape`.
//
// The loader caches its result for the session (the config tree is stable
// enough within a session, and re-fetching on every ⌘K open would be wasteful).
// Failures degrade gracefully: a section whose aliases can't be fetched still
// contributes its own section item, and a total fetch failure resolves to an
// empty list so the palette keeps showing its nav destinations.

import { getMapKeys, getSectionPicker, getSections, type SectionInfo } from "./api";
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
  group: "Config section" | "Config entry";
}

// Same predicate SectionNavigator uses: only these two shapes have children.
function sectionHasChildren(s: SectionInfo): boolean {
  return s.shape === "one_tier_alias_map" || s.shape === "typed_family_map";
}

// Build the section-level item every section contributes (its own jump target).
function sectionItem(s: SectionInfo): ConfigSearchItem {
  return {
    label: s.label,
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

// Build the full flat index: every section as a jump target, plus every
// configured entity under map-shaped sections.
async function build(): Promise<ConfigSearchItem[]> {
  const { sections } = await getSections();

  // Sections first (cheap, no extra fetch) so nav-like results are stable.
  const items: ConfigSearchItem[] = sections.map(sectionItem);

  // Fan out the per-section entity fetches in parallel; each already swallows
  // its own errors, so allSettled is belt-and-suspenders only.
  const childSections = sections.filter(sectionHasChildren);
  const entityLists = await Promise.allSettled(childSections.map(loadEntities));
  for (const r of entityLists) {
    if (r.status === "fulfilled") items.push(...r.value);
  }

  return items;
}

// Session cache. We cache the resolved list AND the in-flight promise so
// concurrent opens share one fetch and later opens are instant.
let cache: ConfigSearchItem[] | null = null;
let inFlight: Promise<ConfigSearchItem[]> | null = null;

/**
 * Load the config search index, cached for the session.
 *
 * Never rejects: on any failure it resolves to an empty list so the palette
 * still renders its static nav destinations. Concurrent calls share a single
 * in-flight fetch; once resolved, subsequent calls return the cache instantly.
 */
export async function loadConfigSearchItems(): Promise<ConfigSearchItem[]> {
  if (cache) return cache;
  if (inFlight) return inFlight;

  inFlight = build()
    .then((items) => {
      cache = items;
      return items;
    })
    .catch(() => {
      // Total failure: degrade to no config items (palette keeps nav targets).
      // Don't cache the empty result — a transient failure shouldn't poison
      // the rest of the session; let the next open retry.
      return [];
    })
    .finally(() => {
      inFlight = null;
    });

  return inFlight;
}

/** Test/HMR escape hatch: drop the session cache so the next load refetches. */
export function clearConfigSearchCache(): void {
  cache = null;
  inFlight = null;
}

// Invalidate the session cache whenever config structure/entities change, so
// the next ⌘K open rebuilds the index instead of showing stale entities. The
// event is dispatched by the config-mutating calls in api.ts (patchConfig,
// deleteMapKey, selectSectionItem); a browser event keeps this decoupled and
// avoids a circular import (this module imports from api.ts).
if (typeof window !== "undefined") {
  window.addEventListener("zeroclaw-config-mutated", clearConfigSearchCache);
}
