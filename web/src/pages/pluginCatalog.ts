import type { PluginCatalogEntry } from "../lib/api.ts";

export type PluginCatalogFilter = "all" | "installed" | "available";

/** Sort capabilities within the source record that declared them. */
export function catalogCapabilities(capabilities: string[]): string[] {
  return Array.from(new Set(capabilities)).sort();
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
