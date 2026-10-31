// Pure helpers for `kind === "secret-map"` config rows (MCP `env`/`headers`,
// provider `extra_headers`, …). The gateway emits one container row per
// secret key/value map — present even when the map is empty, so the form can
// offer "add entry" — followed by one secret `<container>.<KEY>` row per
// existing entry. The form renders the entries inside the container block
// instead of scattering them through the alphabetical field list.

import type { ListResponseEntry } from "../../lib/api";

export const SECRET_MAP_KIND = "secret-map";

/** Map each secret-map container path to its entry rows (in list order). An
 *  entry belongs to the longest container whose path is a strict dotted
 *  prefix of it; only secret rows qualify, so a sibling field can never be
 *  swallowed by a container. */
export function groupSecretMapEntries(
  entries: readonly ListResponseEntry[],
): Map<string, ListResponseEntry[]> {
  const containers = entries
    .filter((e) => e.kind === SECRET_MAP_KIND)
    .map((e) => e.path)
    .sort((a, b) => b.length - a.length);
  const grouped = new Map<string, ListResponseEntry[]>();
  for (const c of containers) grouped.set(c, []);
  if (containers.length === 0) return grouped;
  for (const e of entries) {
    if (e.kind === SECRET_MAP_KIND || !e.is_secret) continue;
    const owner = containers.find((c) => e.path.startsWith(`${c}.`));
    if (owner) grouped.get(owner)?.push(e);
  }
  return grouped;
}

/** The entry key of an entry row relative to its container path. */
export function secretMapEntryKey(containerPath: string, entryPath: string): string {
  return entryPath.startsWith(`${containerPath}.`)
    ? entryPath.slice(containerPath.length + 1)
    : entryPath;
}

/** Validate a new entry name. Returns an i18n key for the rejection, or null.
 *  Kept loose on purpose — the same row type fronts env vars, HTTP headers
 *  and plugin settings — so only names that can never be valid are refused
 *  (mirrors zerocode's `secret_map_key_validation_key`). */
export function secretMapKeyError(
  key: string,
  existingKeys: readonly string[],
): string | null {
  // eslint-disable-next-line no-control-regex
  if (key.length === 0 || /[\s=\u0000-\u001f\u007f]/.test(key)) {
    return "fieldform.secret_map_key_invalid";
  }
  if (existingKeys.includes(key)) return "fieldform.secret_map_key_exists";
  return null;
}
