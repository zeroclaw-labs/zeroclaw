import type { PluginCatalogEntry } from "../lib/api.ts";

export type PluginCatalogFilter = "all" | "installed" | "available";

/** Derive display capabilities without persisting a second package view. */
export function catalogCapabilities(entry: PluginCatalogEntry): string[] {
  return Array.from(
    new Set([
      ...(entry.installed?.capabilities ?? []),
      ...(entry.available?.capabilities ?? []),
    ]),
  ).sort();
}

/** Prefer admitted local metadata while retaining registry-only descriptions. */
export function catalogDescription(entry: PluginCatalogEntry): string | null {
  return entry.installed?.description ?? entry.available?.description ?? null;
}

export function matchesCatalogFilter(
  entry: PluginCatalogEntry,
  filter: PluginCatalogFilter,
): boolean {
  switch (filter) {
    case "installed":
      return entry.installed != null;
    case "available":
      return entry.available != null;
    case "all":
      return true;
  }
}
